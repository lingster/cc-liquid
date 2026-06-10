"""End-to-end check: run the real cc-liquid CLI against hl-proxy.

PRD acceptance — cc-liquid, with only a `base_url` repoint, must run fully
against the Digital Twin Proxy. Two phases:

Phase A (Appendix B v1 — forward + playback market):
* market reads (allMids/meta/spotMeta) served offline from a recorded session,
* account reads forwarded upstream (mocked Hyperliquid here),
* /exchange writes forwarded on testnet with --allow-trading (mocked fills),
* every round-trip captured to rpc_log.jsonl.

Phase B (PRD §7 + §B.6 follow-up — engine-backed sim, fully offline):
* hl-proxy --sim: matching engine + virtual account answer account reads and
  /exchange writes; NO upstream exists at all,
* cc-liquid rebalances, positions appear, a second rebalance sees them,
* determinism (PRD §10.5): two identical runs produce identical fills.

Usage:
    uv run python recorder/scripts/e2e_cc_liquid.py
"""

from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
RECORDER = REPO / "recorder"
DUMMY_KEY = "0x" + "1" * 64
UV_CC_LIQUID = ["uv", "run", "--project", str(REPO), "cc-liquid"]


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


# --- Mock Hyperliquid upstream (Phase A: answers what playback cannot) ---

exchange_requests: list[dict] = []


def info_response(body: dict) -> dict | list:
    ty = body.get("type")
    if ty == "clearinghouseState":
        summary = {
            "accountValue": "10000.0",
            "totalNtlPos": "0.0",
            "totalMarginUsed": "0.0",
            "totalRawUsd": "10000.0",
        }
        return {
            "marginSummary": summary,
            "crossMarginSummary": summary,
            "crossMaintenanceMarginUsed": "0.0",
            "assetPositions": [],
            "withdrawable": "10000.0",
            "time": int(time.time() * 1000),
        }
    if ty == "userFees":
        return {"userCrossRate": "0.00035", "userAddRate": "0.0001", "feeSchedule": {}}
    if ty in ("frontendOpenOrders", "userFills", "userFillsByTime"):
        return []
    return {}


def exchange_response(body: dict) -> dict:
    exchange_requests.append(body)
    action = body.get("action", {})
    if action.get("type") == "order":
        statuses = [
            {"filled": {"totalSz": o.get("s", "0"), "avgPx": o.get("p", "0"), "fee": "0.1"}}
            for o in action.get("orders", [])
        ]
        return {"status": "ok", "response": {"type": "order", "data": {"statuses": statuses}}}
    if action.get("type") == "cancel":
        n = len(action.get("cancels", []))
        return {"status": "ok", "response": {"type": "cancel", "data": {"statuses": ["success"] * n}}}
    return {"status": "ok", "response": {}}


class MockHL(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(length) or b"{}")
        resp = info_response(body) if self.path == "/info" else exchange_response(body)
        data = json.dumps(resp).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):  # quiet
        pass


# --- Harness ---


def run(cmd: list, cwd: Path, env: dict | None = None) -> subprocess.CompletedProcess:
    print(f"\n$ {' '.join(map(str, cmd))}")
    proc = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, env=env)
    sys.stdout.write(proc.stdout[-3000:])
    if proc.stderr:
        sys.stderr.write(proc.stderr[-3000:])
    return proc


def check(proc: subprocess.CompletedProcess, label: str):
    assert proc.returncode == 0, f"{label}: exit code {proc.returncode}"
    for marker in ("Traceback", "✗ Error"):
        assert marker not in proc.stdout, f"{label}: output contains {marker!r}"
        assert marker not in proc.stderr, f"{label}: stderr contains {marker!r}"
    print(f"-- {label}: OK")


def start_proxy(args: list) -> tuple[subprocess.Popen, str]:
    port = free_port()
    proxy = subprocess.Popen(
        [RECORDER / "target/debug/hl-proxy", "--listen", f"127.0.0.1:{port}", *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    for _ in range(50):
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                break
        except OSError:
            if proxy.poll() is not None:
                raise RuntimeError(f"hl-proxy died:\n{proxy.stdout.read()}")
            time.sleep(0.1)
    else:
        raise RuntimeError("hl-proxy never started listening")
    base_url = f"http://127.0.0.1:{port}"
    print(f"-- hl-proxy: {base_url}")
    return proxy, base_url


def write_workdir(work: Path, base_url: str):
    """cc-liquid workdir: config + predictions, base_url repointed only."""
    from datetime import date

    import polars as pl
    from eth_account import Account

    owner = Account.from_key(DUMMY_KEY).address
    (work / "cc-liquid-config.yaml").write_text(
        f"""\
base_url: {base_url}
active_profile: default
profiles:
  default:
    owner: "{owner}"
data:
  source: local
  path: predictions.parquet
  date_column: release_date
  asset_id_column: id
  prediction_column: pred_30d
portfolio:
  num_long: 1
  num_short: 1
  target_leverage: 1.0
  rebalancing:
    mode: full
"""
    )
    pl.DataFrame(
        {
            "release_date": [date.today()] * 3,
            "id": ["BTC", "ETH", "SOL"],
            "pred_30d": [0.9, 0.5, 0.1],
        }
    ).write_parquet(work / "predictions.parquet")


def read_capture(capture: Path) -> list[dict]:
    entries = [
        json.loads(line)
        for line in (capture / "rpc_log.jsonl").read_text().splitlines()
        if line.strip()
    ]
    seqs = [e["seq"] for e in entries]
    assert seqs == list(range(len(entries))), "seq must be gap-free"
    return entries


def sim_fills(entries: list[dict]) -> list[tuple]:
    """Extract (coin-asset, sz, avgPx) of every simulated order fill."""
    fills = []
    for e in entries:
        if e["method_tag"] != "bulk_orders":
            continue
        orders = e["request"]["action"]["orders"]
        statuses = e["response"]["response"]["data"]["statuses"]
        for order, status in zip(orders, statuses):
            if "filled" in status:
                fills.append((order["a"], status["filled"]["totalSz"], status["filled"]["avgPx"]))
    return fills


def phase_a(session: Path, env: dict):
    """Forward + playback market with a mocked upstream exchange."""
    print("\n=== Phase A: playback market + forwarded account/writes ===")
    upstream = ThreadingHTTPServer(("127.0.0.1", 0), MockHL)
    threading.Thread(target=upstream.serve_forever, daemon=True).start()
    upstream_url = f"http://127.0.0.1:{upstream.server_address[1]}"
    print(f"-- mock upstream: {upstream_url}")

    work = Path(tempfile.mkdtemp(prefix="twin-proxy-a-"))
    capture = work / "capture"
    proxy, base_url = start_proxy(
        ["--network", "testnet", "--allow-trading", "--market-source", "playback",
         "--session", str(session), "--end-of-window", "hold",
         "--upstream", upstream_url, "--out", str(capture)]
    )
    try:
        write_workdir(work, base_url)
        check(run([*UV_CC_LIQUID, "account"], work, env), "A: cc-liquid account")
        check(run([*UV_CC_LIQUID, "rebalance", "--skip-confirm"], work, env), "A: cc-liquid rebalance")

        orders = [r for r in exchange_requests if r.get("action", {}).get("type") == "order"]
        assert orders, "no /exchange order reached the upstream"
        print(f"-- upstream received {len(orders)} forwarded order request(s)")

        entries = read_capture(capture)
        by_tag_source = {(e["method_tag"], e["source"]) for e in entries}
        for expected in [
            ("all_mids", "playback"),
            ("meta", "playback"),
            ("spot_meta", "playback"),
            ("user_state", "forward"),
            ("user_fees", "forward"),
            ("bulk_orders", "forward"),
        ]:
            assert expected in by_tag_source, f"missing capture entry {expected}; got {by_tag_source}"
        print(f"-- capture log: {len(entries)} entries, all expected (tag, source) pairs present")
    finally:
        proxy.terminate()
        upstream.shutdown()


def run_sim_once(session: Path, env: dict, label: str) -> list[tuple]:
    """One fully-offline sim run; returns the simulated fills."""
    work = Path(tempfile.mkdtemp(prefix="twin-proxy-b-"))
    capture = work / "capture"
    # No upstream exists: point at a dead loopback port to prove nothing forwards.
    dead_upstream = f"http://127.0.0.1:{free_port()}"
    proxy, base_url = start_proxy(
        ["--network", "mainnet", "--market-source", "playback", "--session", str(session),
         "--sim", "--start-balance", "10000", "--end-of-window", "hold",
         "--upstream", dead_upstream, "--out", str(capture)]
    )
    try:
        write_workdir(work, base_url)
        check(run([*UV_CC_LIQUID, "account"], work, env), f"{label}: account (flat)")
        check(run([*UV_CC_LIQUID, "rebalance", "--skip-confirm"], work, env), f"{label}: rebalance")
        check(run([*UV_CC_LIQUID, "account"], work, env), f"{label}: account (positioned)")

        entries = read_capture(capture)
        # The last user_state served must show the simulated positions.
        last_state = [e for e in entries if e["method_tag"] == "user_state"][-1]
        coins = {
            p["position"]["coin"] for p in last_state["response"]["assetPositions"]
        }
        assert coins == {"BTC", "SOL"}, f"expected BTC long + SOL short, got {coins}"
        assert all(e["source"] == "playback" for e in entries), (
            "fully-offline sim must never forward; sources: "
            + str({e["source"] for e in entries})
        )
        fills = sim_fills(entries)
        assert fills, "sim produced no fills"
        # The virtual account answered the post-trade reads.
        tags = {e["method_tag"] for e in entries}
        assert {"user_state", "user_fees", "bulk_orders", "frontend_open_orders"} <= tags, tags
        print(f"-- {label}: {len(fills)} simulated fill(s), all {len(entries)} entries playback-served")
        return fills
    finally:
        proxy.terminate()


def phase_b(session: Path, env: dict):
    """Engine-backed sim: fully offline trading + determinism (PRD §10.5)."""
    print("\n=== Phase B: engine-backed sim (no upstream at all) ===")
    fills_1 = run_sim_once(session, env, "B/run1")
    fills_2 = run_sim_once(session, env, "B/run2")
    assert fills_1 == fills_2, (
        f"determinism violated:\nrun1: {fills_1}\nrun2: {fills_2}"
    )
    print(f"-- determinism: {len(fills_1)} fills identical across independent runs")


def main():
    check(
        run(["cargo", "build", "--bin", "hl-proxy", "--example", "make_demo_session"], RECORDER),
        "cargo build",
    )
    session = Path(tempfile.mkdtemp(prefix="twin-session-")) / "session"
    check(
        run([RECORDER / "target/debug/examples/make_demo_session", session, "300"], REPO),
        "make_demo_session",
    )
    env = dict(os.environ, HYPERLIQUID_PRIVATE_KEY=DUMMY_KEY)

    phase_a(session, env)
    phase_b(session, env)

    print(
        "\nE2E PASS: cc-liquid ran fully against hl-proxy in both modes — "
        "playback+forward (A) and fully-offline engine-backed sim with deterministic fills (B)"
    )


if __name__ == "__main__":
    main()

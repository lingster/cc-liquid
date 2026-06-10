"""End-to-end check: run the real cc-liquid CLI against hl-proxy.

PRD Appendix B acceptance — cc-liquid, with only a `base_url` repoint, must run
fully against the Digital Twin Proxy:

* market reads (allMids/meta/spotMeta) served offline from a recorded session
  (playback market source),
* account reads forwarded upstream (mocked Hyperliquid here, so the check runs
  with no external network),
* /exchange writes forwarded on testnet with --allow-trading (mocked fills),
* every round-trip captured to rpc_log.jsonl.

Usage:
    uv run python recorder/scripts/e2e_cc_liquid.py
"""

from __future__ import annotations

import json
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


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


# --- Mock Hyperliquid upstream (answers what playback cannot: account reads + writes) ---

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


def run(cmd: list[str], cwd: Path, env: dict | None = None) -> subprocess.CompletedProcess:
    print(f"\n$ {' '.join(map(str, cmd))}")
    proc = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, env=env)
    sys.stdout.write(proc.stdout[-4000:])
    if proc.stderr:
        sys.stderr.write(proc.stderr[-4000:])
    return proc


def check(proc: subprocess.CompletedProcess, label: str):
    assert proc.returncode == 0, f"{label}: exit code {proc.returncode}"
    for marker in ("Traceback", "✗ Error"):
        assert marker not in proc.stdout, f"{label}: output contains {marker!r}"
        assert marker not in proc.stderr, f"{label}: stderr contains {marker!r}"
    print(f"-- {label}: OK")


def main():
    import os

    # 1. Build the proxy and session generator.
    check(
        run(["cargo", "build", "--bin", "hl-proxy", "--example", "make_demo_session"], RECORDER),
        "cargo build",
    )

    work = Path(tempfile.mkdtemp(prefix="twin-proxy-e2e-"))
    session = work / "session"
    capture = work / "capture"

    # 2. Synthetic recorded session (BTC/ETH/SOL mids).
    check(
        run([RECORDER / "target/debug/examples/make_demo_session", session, "300"], work),
        "make_demo_session",
    )

    # 3. Mock Hyperliquid upstream.
    upstream = ThreadingHTTPServer(("127.0.0.1", 0), MockHL)
    threading.Thread(target=upstream.serve_forever, daemon=True).start()
    upstream_url = f"http://127.0.0.1:{upstream.server_address[1]}"
    print(f"-- mock upstream: {upstream_url}")

    # 4. hl-proxy: playback market data, forward the rest, testnet writes allowed.
    port = free_port()
    proxy = subprocess.Popen(
        [
            RECORDER / "target/debug/hl-proxy",
            "--listen", f"127.0.0.1:{port}",
            "--network", "testnet",
            "--allow-trading",
            "--market-source", "playback",
            "--session", session,
            "--upstream", upstream_url,
            "--out", capture,
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    base_url = f"http://127.0.0.1:{port}"
    try:
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
        print(f"-- hl-proxy: {base_url}")

        # 5. cc-liquid workdir: config + predictions, base_url repointed only.
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
        import polars as pl
        from datetime import date

        pl.DataFrame(
            {
                "release_date": [date.today()] * 3,
                "id": ["BTC", "ETH", "SOL"],
                "pred_30d": [0.9, 0.5, 0.1],
            }
        ).write_parquet(work / "predictions.parquet")

        env = dict(os.environ, HYPERLIQUID_PRIVATE_KEY=DUMMY_KEY)
        uv = ["uv", "run", "--project", str(REPO), "cc-liquid"]

        # 6. Full cc-liquid flows through the proxy.
        check(run([*uv, "account"], work, env), "cc-liquid account")
        check(run([*uv, "rebalance", "--skip-confirm"], work, env), "cc-liquid rebalance")

        # 7. The mock exchange must have received forwarded signed orders.
        orders = [r for r in exchange_requests if r.get("action", {}).get("type") == "order"]
        assert orders, "no /exchange order reached the upstream"
        print(f"-- upstream received {len(orders)} forwarded order request(s)")

        # 8. Capture log: self-classifying, includes playback + forward + writes.
        entries = [
            json.loads(line)
            for line in (capture / "rpc_log.jsonl").read_text().splitlines()
            if line.strip()
        ]
        seqs = [e["seq"] for e in entries]
        assert seqs == list(range(len(entries))), "seq must be gap-free"
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

        print("\nE2E PASS: cc-liquid ran fully against hl-proxy (offline market data, "
              "forwarded account reads, captured testnet writes)")
    finally:
        proxy.terminate()
        upstream.shutdown()


if __name__ == "__main__":
    main()

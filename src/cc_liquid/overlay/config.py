"""Configuration for the deepLOB execution overlay (P3).

Self-contained in the overlay package so `cc_liquid.config` can depend on it
without a cycle. Nests under `ExecutionConfig.overlay`; loads from the
`execution.overlay:` YAML block via the generic nested-dataclass loader.
"""

from dataclasses import dataclass


@dataclass
class OverlayConfig:
    """deepLOB micro-execution overlay parameters.

    Disabled by default — the overlay must be opted into explicitly, and any
    failure falls back to the existing execution path (never blocks a trade).
    """

    enabled: bool = False
    url: str = "http://127.0.0.1:8080"  # decision service (hl-live --serve)
    model: str = "qf-ens5"  # which model the service should serve (configurable)
    horizon: int = 10  # signal head, snapshots (~5.4 s) — the best trading horizon
    confidence: float = 0.6  # p threshold to act on a directional call
    max_staleness_ms: int = 2000  # reject a signal older than this (feed-health guard)
    max_wait_snaps: int = 10  # how long to work an order passively before crossing
    snapshot_ms: int = 540  # feed cadence, for snaps<->wall-clock conversions
    timeout_sec: float = 2.0  # per-call budget talking to the decision service
    coins: list[str] | None = None  # optional allowlist; None/empty = trust the service

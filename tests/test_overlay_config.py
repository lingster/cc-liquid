"""Red/green: the execution.overlay block loads onto ExecutionConfig.overlay."""

import yaml
import pytest

from cc_liquid import Config
from cc_liquid.config import DEFAULT_CONFIG_PATH
from cc_liquid.overlay import OverlayConfig


@pytest.fixture(autouse=True)
def in_tmp(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("HYPERLIQUID_PRIVATE_KEY", "0x5678")
    yield


def write_cfg(data: dict):
    with open(DEFAULT_CONFIG_PATH, "w") as f:
        yaml.dump(data, f)


def test_overlay_defaults_disabled():
    write_cfg({"is_testnet": True})
    cfg = Config()
    assert isinstance(cfg.execution.overlay, OverlayConfig)
    assert cfg.execution.overlay.enabled is False


def test_overlay_loads_from_yaml():
    write_cfg(
        {
            "is_testnet": True,
            "execution": {
                "order_type": "limit",
                "overlay": {
                    "enabled": True,
                    "url": "http://127.0.0.1:9999",
                    "model": "mh-winner-calibrated",
                    "confidence": 0.65,
                    "horizon": 10,
                },
            },
        }
    )
    cfg = Config()
    ov = cfg.execution.overlay
    assert ov.enabled is True
    assert ov.url == "http://127.0.0.1:9999"
    assert ov.model == "mh-winner-calibrated"
    assert ov.confidence == 0.65
    # sibling execution fields still load
    assert cfg.execution.order_type == "limit"


def test_overlay_appears_in_to_dict():
    write_cfg({"is_testnet": True})
    cfg = Config()
    assert "overlay" in cfg.to_dict()["execution"]

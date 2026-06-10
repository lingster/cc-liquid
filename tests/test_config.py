import os

import pytest
import yaml

from cc_liquid import Config
from cc_liquid.config import DEFAULT_CONFIG_PATH


@pytest.fixture(autouse=True)
def setup_teardown(tmp_path, monkeypatch):
    # Run each test in a disposable temp cwd: Config resolves
    # DEFAULT_CONFIG_PATH relative to cwd, so the repo's real
    # cc-liquid-config.yaml is never written or deleted by tests.
    monkeypatch.chdir(tmp_path)

    # Setup: create a dummy config file
    config_data = {
        "is_testnet": True,
        "data": {"source": "local", "path": "test_predictions.parquet"},
        "portfolio": {"num_long": 5},
    }
    with open(DEFAULT_CONFIG_PATH, "w") as f:
        yaml.dump(config_data, f)

    # Set dummy env vars (monkeypatch restores originals on teardown)
    monkeypatch.setenv("CROWDCENT_API_KEY", "test_api_key")
    monkeypatch.setenv("HYPERLIQUID_ADDRESS", "0x1234")
    monkeypatch.setenv("HYPERLIQUID_PRIVATE_KEY", "0x5678")

    yield


def test_config_loading_defaults():
    # Remove config to test defaults
    if os.path.exists(DEFAULT_CONFIG_PATH):
        os.remove(DEFAULT_CONFIG_PATH)

    config = Config()
    assert config.is_testnet is False
    assert config.data.source == "crowdcent"
    assert config.portfolio.num_long == 10


def test_config_loading_from_yaml():
    config = Config()

    # Assertions from YAML
    assert config.is_testnet is True
    assert config.data.source == "local"
    assert config.data.path == "test_predictions.parquet"
    assert config.portfolio.num_long == 5

    # Assertions from .env
    assert config.CROWDCENT_API_KEY == "test_api_key"
    assert config.HYPERLIQUID_ADDRESS is None
    assert config.HYPERLIQUID_PRIVATE_KEY is None


def test_to_dict():
    config = Config()
    config_dict = config.to_dict()

    assert config_dict["is_testnet"] is True
    assert config_dict["data"]["source"] == "local"
    assert config_dict["portfolio"]["num_long"] == 5

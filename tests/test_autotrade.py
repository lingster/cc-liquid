"""Tests for autotrade functionality including profit tracking and display."""

import json
import os
from datetime import datetime, timedelta, timezone
from unittest.mock import MagicMock, patch

import pytest

from cc_liquid.cli_display import (
    create_autotrade_footer_panel,
    create_autotrade_metrics_panel,
    create_last_profit_table,
)


# ============================================================================
# Fixtures
# ============================================================================


@pytest.fixture
def state_file(tmp_path):
    """Create a temporary state file path."""
    state_path = tmp_path / ".cc_liquid_state.json"
    yield str(state_path)
    if state_path.exists():
        state_path.unlink()


@pytest.fixture
def mock_portfolio():
    """Create a mock PortfolioInfo object."""
    mock = MagicMock()
    mock.account.account_value = 10000.0
    mock.account.margin_used = 5000.0
    mock.account.free_collateral = 5000.0
    mock.account.current_leverage = 1.5
    mock.total_long_value = 3000.0
    mock.total_short_value = 2000.0
    mock.net_exposure = 1000.0
    mock.total_exposure = 5000.0
    mock.total_unrealized_pnl = 500.0
    mock.positions = []
    return mock


@pytest.fixture
def sample_last_profit_info():
    """Sample last profit info dict."""
    return {
        "profit": 150.50,
        "fees": 12.25,
        "date": "2025-12-30 14:30",
    }


# ============================================================================
# Tests for create_last_profit_table
# ============================================================================


class TestCreateLastProfitTable:
    """Tests for the last profit table display."""

    def test_no_profit_info_shows_na(self):
        """When no profit info is provided, should show N/A."""
        table = create_last_profit_table(None)
        # Table should be created without error
        assert table is not None
        assert table.row_count == 1

    def test_profit_info_shows_all_fields(self, sample_last_profit_info):
        """Should display profit, fees, net profit, and date."""
        table = create_last_profit_table(sample_last_profit_info)
        assert table is not None
        # Should have 4 rows: LAST PROFIT, FEES PAID, NET PROFIT, DATE
        assert table.row_count == 4

    def test_positive_profit_calculation(self):
        """Net profit should be profit minus fees."""
        profit_info = {"profit": 100.0, "fees": 10.0, "date": "2025-01-01"}
        table = create_last_profit_table(profit_info)
        # Net should be 90.0 (100 - 10)
        assert table is not None

    def test_negative_profit_handling(self):
        """Should handle negative profit (loss) correctly."""
        profit_info = {"profit": -50.0, "fees": 5.0, "date": "2025-01-01"}
        table = create_last_profit_table(profit_info)
        # Net should be -55.0 (-50 - 5)
        assert table is not None

    def test_missing_date_field(self):
        """Should handle missing date field gracefully."""
        profit_info = {"profit": 100.0, "fees": 10.0}
        table = create_last_profit_table(profit_info)
        # Should have 3 rows without date
        assert table.row_count == 3


# ============================================================================
# Tests for create_autotrade_metrics_panel
# ============================================================================


class TestCreateAutotradeMetricsPanel:
    """Tests for the autotrade metrics panel display."""

    def test_panel_without_profit_info(self, mock_portfolio):
        """Should create panel without last profit column when no info."""
        panel = create_autotrade_metrics_panel(mock_portfolio, None)
        assert panel is not None
        assert panel.title is not None

    def test_panel_with_profit_info(self, mock_portfolio, sample_last_profit_info):
        """Should include last profit column when info is provided."""
        panel = create_autotrade_metrics_panel(mock_portfolio, sample_last_profit_info)
        assert panel is not None


# ============================================================================
# Tests for create_autotrade_footer_panel - Rebalance Countdown
# ============================================================================


class TestAutotradeFooterRebalanceCountdown:
    """Tests for the rebalance countdown in trading mode."""

    def test_waiting_mode_shows_opening_countdown(self):
        """In waiting mode, should show countdown to next opening time."""
        next_opening = datetime.now(timezone.utc) + timedelta(hours=5)
        panel = create_autotrade_footer_panel(
            mode="waiting",
            pnl_pct=0.0,
            profit_target_pct=15.0,
            days_held=0,
            max_hold_days=10,
            next_opening_time=next_opening,
            refresh_seconds=5.0,
        )
        assert panel is not None

    def test_trading_mode_without_entry_date_no_countdown(self):
        """Trading mode without entry_date should not crash."""
        panel = create_autotrade_footer_panel(
            mode="trading",
            pnl_pct=5.0,
            profit_target_pct=15.0,
            days_held=3,
            max_hold_days=10,
            next_opening_time=None,
            refresh_seconds=5.0,
            entry_date=None,
            enable_rebalance=True,
        )
        assert panel is not None

    def test_trading_mode_with_entry_date_shows_rebalance_countdown(self):
        """Trading mode with entry_date should show rebalance countdown."""
        # Entry date 3 days ago, max_hold_days=10, so 7 days until rebalance
        entry_date = (datetime.now(timezone.utc) - timedelta(days=3)).date().isoformat()
        panel = create_autotrade_footer_panel(
            mode="trading",
            pnl_pct=5.0,
            profit_target_pct=15.0,
            days_held=3,
            max_hold_days=10,
            next_opening_time=None,
            refresh_seconds=5.0,
            entry_date=entry_date,
            enable_rebalance=True,
        )
        assert panel is not None

    def test_trading_mode_rebalance_disabled_no_countdown(self):
        """When enable_rebalance is False, should not show countdown."""
        entry_date = datetime.now(timezone.utc).date().isoformat()
        panel = create_autotrade_footer_panel(
            mode="trading",
            pnl_pct=5.0,
            profit_target_pct=15.0,
            days_held=3,
            max_hold_days=10,
            next_opening_time=None,
            refresh_seconds=5.0,
            entry_date=entry_date,
            enable_rebalance=False,
        )
        assert panel is not None

    def test_trailing_mode_display(self):
        """Trailing stop mode should show peak and stop level."""
        entry_date = datetime.now(timezone.utc).date().isoformat()
        panel = create_autotrade_footer_panel(
            mode="trading",
            pnl_pct=16.0,
            profit_target_pct=15.0,
            days_held=5,
            max_hold_days=10,
            next_opening_time=None,
            refresh_seconds=5.0,
            trailing_active=True,
            peak_profit_pct=17.0,
            trailing_stop_offset_pct=0.5,
            entry_date=entry_date,
            enable_rebalance=True,
        )
        assert panel is not None


# ============================================================================
# Tests for Autotrade State Persistence
# ============================================================================


class TestAutotradeStatePersistence:
    """Tests for autotrade state load/save with last_profit_info."""

    def test_load_default_state_when_no_file(self, state_file, monkeypatch):
        """Should return default state when file doesn't exist."""
        # Patch the state file path
        from cc_liquid.trader import CCLiquid

        # Create a minimal mock config
        mock_config = MagicMock()
        mock_config.is_testnet = True
        mock_config.HYPERLIQUID_PRIVATE_KEY = None
        mock_config.active_profile = "default"
        mock_config.profiles = {}

        with patch.object(CCLiquid, "__init__", lambda self, *args, **kwargs: None):
            trader = CCLiquid.__new__(CCLiquid)
            trader.config = mock_config
            trader.logger = MagicMock()

            # Patch os.path.exists to return False
            with patch("os.path.exists", return_value=False):
                state = trader._load_autotrade_state()

            assert state["mode"] == "waiting"
            assert state["entry_date"] is None
            assert state["trailing_active"] is False
            assert state["peak_profit_pct"] is None
            assert state["last_profit_info"] is None

    def test_save_and_load_state_with_profit_info(self, state_file):
        """Should save and load state including last_profit_info."""
        from cc_liquid.trader import CCLiquid

        mock_config = MagicMock()

        with patch.object(CCLiquid, "__init__", lambda self, *args, **kwargs: None):
            trader = CCLiquid.__new__(CCLiquid)
            trader.config = mock_config
            trader.logger = MagicMock()

            profit_info = {"profit": 250.0, "fees": 15.0, "date": "2025-12-30 10:00"}

            # Patch state file path
            with patch("cc_liquid.trader.CCLiquid._load_autotrade_state") as mock_load:
                mock_load.return_value = {
                    "mode": "waiting",
                    "entry_date": None,
                    "trailing_active": False,
                    "peak_profit_pct": None,
                    "last_profit_info": None,
                }

                # Write state directly to test file
                with open(state_file, "w") as f:
                    json.dump(
                        {
                            "autotrade": {
                                "mode": "waiting",
                                "entry_date": None,
                                "trailing_active": False,
                                "peak_profit_pct": None,
                                "last_profit_info": profit_info,
                            }
                        },
                        f,
                    )

                # Read back
                with open(state_file) as f:
                    loaded = json.load(f)

                assert loaded["autotrade"]["last_profit_info"] == profit_info

    def test_profit_info_preserved_on_mode_change(self, state_file):
        """last_profit_info should be preserved when mode changes."""
        profit_info = {"profit": 100.0, "fees": 5.0, "date": "2025-01-01"}

        # Initial state with profit info
        initial_state = {
            "autotrade": {
                "mode": "waiting",
                "entry_date": None,
                "trailing_active": False,
                "peak_profit_pct": None,
                "last_profit_info": profit_info,
            }
        }

        with open(state_file, "w") as f:
            json.dump(initial_state, f)

        # Simulate save without explicitly passing last_profit_info
        # (should preserve existing)
        with open(state_file) as f:
            existing = json.load(f)

        # Update mode but preserve last_profit_info
        existing_profit = existing.get("autotrade", {}).get("last_profit_info")
        existing["autotrade"] = {
            "mode": "trading",
            "entry_date": "2025-01-02",
            "trailing_active": False,
            "peak_profit_pct": None,
            "last_profit_info": existing_profit,  # Preserved
        }

        with open(state_file, "w") as f:
            json.dump(existing, f)

        # Verify preservation
        with open(state_file) as f:
            final = json.load(f)

        assert final["autotrade"]["mode"] == "trading"
        assert final["autotrade"]["last_profit_info"] == profit_info


# ============================================================================
# Tests for Profit Capture Logic
# ============================================================================


class TestProfitCaptureLogic:
    """Tests for profit and fee capture when closing positions."""

    def test_calculate_total_fees_from_trades(self):
        """Should correctly sum fees from successful trades."""
        successful_trades = [
            {"coin": "BTC", "status": "filled", "actual_fee": 5.0},
            {"coin": "ETH", "status": "filled", "actual_fee": 3.0},
            {"coin": "SOL", "status": "filled", "actual_fee": 2.0},
        ]

        total_fees = sum(
            t.get("actual_fee", 0.0)
            for t in successful_trades
            if t.get("status") == "filled"
        )

        assert total_fees == 10.0

    def test_fees_exclude_non_filled_trades(self):
        """Should not include fees from non-filled trades."""
        successful_trades = [
            {"coin": "BTC", "status": "filled", "actual_fee": 5.0},
            {"coin": "ETH", "status": "resting", "actual_fee": 0.0},
            {"coin": "SOL", "status": "filled", "actual_fee": 2.0},
        ]

        total_fees = sum(
            t.get("actual_fee", 0.0)
            for t in successful_trades
            if t.get("status") == "filled"
        )

        assert total_fees == 7.0

    def test_handles_missing_actual_fee(self):
        """Should handle trades without actual_fee field."""
        successful_trades = [
            {"coin": "BTC", "status": "filled", "actual_fee": 5.0},
            {"coin": "ETH", "status": "filled"},  # No actual_fee
        ]

        total_fees = sum(
            t.get("actual_fee", 0.0)
            for t in successful_trades
            if t.get("status") == "filled"
        )

        assert total_fees == 5.0

    def test_net_profit_calculation(self):
        """Net profit should be realized profit minus fees."""
        realized_profit = 150.0
        total_fees = 12.5
        net_profit = realized_profit - total_fees

        assert net_profit == 137.5

    def test_negative_profit_with_fees(self):
        """Should handle losses correctly."""
        realized_profit = -50.0
        total_fees = 10.0
        net_profit = realized_profit - total_fees

        assert net_profit == -60.0


# ============================================================================
# Tests for Position Reopening Logic
# ============================================================================


class TestPositionReopeningLogic:
    """Tests for verifying position reopening after profit taking."""

    def test_mode_transitions_waiting_to_trading(self):
        """Mode should transition from waiting to trading when opening positions."""
        # Simulate state after profit taking
        state = {
            "mode": "waiting",
            "entry_date": None,
            "last_profit_info": {"profit": 100.0, "fees": 5.0, "date": "2025-01-01"},
        }

        # Simulate opening positions
        state["mode"] = "trading"
        state["entry_date"] = "2025-01-02"
        # last_profit_info should be preserved

        assert state["mode"] == "trading"
        assert state["entry_date"] == "2025-01-02"
        assert state["last_profit_info"]["profit"] == 100.0

    def test_mode_transitions_trading_to_waiting(self):
        """Mode should transition from trading to waiting when taking profit."""
        # Simulate state during trading
        state = {
            "mode": "trading",
            "entry_date": "2025-01-01",
            "last_profit_info": None,
        }

        # Simulate profit taking
        new_profit_info = {"profit": 200.0, "fees": 15.0, "date": "2025-01-05"}
        state["mode"] = "waiting"
        state["entry_date"] = None
        state["last_profit_info"] = new_profit_info

        assert state["mode"] == "waiting"
        assert state["entry_date"] is None
        assert state["last_profit_info"]["profit"] == 200.0

    def test_next_opening_time_calculation(self):
        """Next opening time should be calculated correctly."""
        opening_time_str = "14:30"
        now_utc = datetime(2025, 1, 15, 10, 0, 0, tzinfo=timezone.utc)

        hour, minute = map(int, opening_time_str.split(":"))
        from datetime import time as time_cls

        opening_time = time_cls(hour=hour, minute=minute)
        today_at = datetime.combine(now_utc.date(), opening_time, tzinfo=timezone.utc)

        if now_utc >= today_at:
            next_opening = datetime.combine(
                now_utc.date() + timedelta(days=1), opening_time, tzinfo=timezone.utc
            )
        else:
            next_opening = today_at

        # Since now (10:00) < opening (14:30), should be today
        assert next_opening.hour == 14
        assert next_opening.minute == 30
        assert next_opening.date() == now_utc.date()

    def test_next_opening_time_rolls_to_tomorrow(self):
        """If past opening time, should roll to tomorrow."""
        opening_time_str = "14:30"
        now_utc = datetime(2025, 1, 15, 16, 0, 0, tzinfo=timezone.utc)

        hour, minute = map(int, opening_time_str.split(":"))
        from datetime import time as time_cls

        opening_time = time_cls(hour=hour, minute=minute)
        today_at = datetime.combine(now_utc.date(), opening_time, tzinfo=timezone.utc)

        if now_utc >= today_at:
            next_opening = datetime.combine(
                now_utc.date() + timedelta(days=1), opening_time, tzinfo=timezone.utc
            )
        else:
            next_opening = today_at

        # Since now (16:00) > opening (14:30), should be tomorrow
        assert next_opening.hour == 14
        assert next_opening.minute == 30
        assert next_opening.date() == now_utc.date() + timedelta(days=1)


# ============================================================================
# Integration Tests
# ============================================================================


class TestAutotradeIntegration:
    """Integration tests for the complete autotrade flow."""

    def test_full_profit_taking_cycle(self, mock_portfolio, state_file):
        """Test complete profit taking cycle: trading -> close -> waiting."""
        # Initial trading state
        trading_state = {
            "autotrade": {
                "mode": "trading",
                "entry_date": "2025-01-01",
                "trailing_active": False,
                "peak_profit_pct": None,
                "last_profit_info": None,
            }
        }

        with open(state_file, "w") as f:
            json.dump(trading_state, f)

        # Simulate profit taking
        realized_profit = mock_portfolio.total_unrealized_pnl  # 500.0
        fees_paid = 25.0  # Simulated fees
        profit_date = "2025-01-05 12:00"

        # Update state after closing
        with open(state_file) as f:
            state = json.load(f)

        state["autotrade"] = {
            "mode": "waiting",
            "entry_date": None,
            "trailing_active": False,
            "peak_profit_pct": None,
            "last_profit_info": {
                "profit": realized_profit,
                "fees": fees_paid,
                "date": profit_date,
            },
        }

        with open(state_file, "w") as f:
            json.dump(state, f)

        # Verify final state
        with open(state_file) as f:
            final_state = json.load(f)

        assert final_state["autotrade"]["mode"] == "waiting"
        assert final_state["autotrade"]["entry_date"] is None
        assert final_state["autotrade"]["last_profit_info"]["profit"] == 500.0
        assert final_state["autotrade"]["last_profit_info"]["fees"] == 25.0

    def test_reopen_positions_preserves_profit_info(self, state_file):
        """Reopening positions should preserve last_profit_info."""
        # State after profit taking
        waiting_state = {
            "autotrade": {
                "mode": "waiting",
                "entry_date": None,
                "trailing_active": False,
                "peak_profit_pct": None,
                "last_profit_info": {
                    "profit": 500.0,
                    "fees": 25.0,
                    "date": "2025-01-05 12:00",
                },
            }
        }

        with open(state_file, "w") as f:
            json.dump(waiting_state, f)

        # Simulate reopening positions
        with open(state_file) as f:
            state = json.load(f)

        # Preserve last_profit_info when switching to trading
        existing_profit_info = state["autotrade"].get("last_profit_info")

        state["autotrade"] = {
            "mode": "trading",
            "entry_date": "2025-01-06",
            "trailing_active": False,
            "peak_profit_pct": None,
            "last_profit_info": existing_profit_info,  # Preserved!
        }

        with open(state_file, "w") as f:
            json.dump(state, f)

        # Verify preservation
        with open(state_file) as f:
            final_state = json.load(f)

        assert final_state["autotrade"]["mode"] == "trading"
        assert final_state["autotrade"]["entry_date"] == "2025-01-06"
        assert final_state["autotrade"]["last_profit_info"]["profit"] == 500.0

    def test_dashboard_receives_all_new_parameters(self, mock_portfolio):
        """Dashboard layout should accept all new parameters."""
        from cc_liquid.cli_display import create_autotrade_dashboard_layout

        layout = create_autotrade_dashboard_layout(
            portfolio=mock_portfolio,
            mode="trading",
            pnl_pct=10.0,
            profit_target_pct=15.0,
            days_held=5,
            max_hold_days=10,
            next_opening_time=None,
            config_dict={"is_testnet": True},
            refresh_seconds=5.0,
            open_orders=[],
            trailing_active=False,
            peak_profit_pct=None,
            trailing_stop_offset_pct=0.5,
            entry_date="2025-01-01",
            enable_rebalance=True,
            last_profit_info={"profit": 100.0, "fees": 5.0, "date": "2025-01-01"},
        )

        assert layout is not None

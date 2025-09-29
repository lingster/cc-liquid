import polars as pl
import pytest

from cc_liquid.portfolio import weights_from_ranks


def test_weights_from_ranks_equal_scheme_balances_long_short():
    preds = pl.DataFrame(
        {
            "id": ["AAA", "BBB", "CCC"],
            "pred": [0.9, 0.7, -0.2],
        }
    )

    weights = weights_from_ranks(
        preds,
        id_col="id",
        pred_col="pred",
        long_assets=["AAA", "BBB"],
        short_assets=["CCC"],
        target_gross=1.2,
        scheme="equal",
    )

    assert set(weights) == {"AAA", "BBB", "CCC"}
    assert weights["AAA"] == pytest.approx(0.4)
    assert weights["BBB"] == pytest.approx(0.4)
    assert weights["CCC"] == pytest.approx(-0.4)
    assert sum(abs(w) for w in weights.values()) == pytest.approx(1.2)


def test_weights_from_ranks_rank_power_favors_top_asset():
    preds = {
        "AAA": 0.9,
        "BBB": 0.6,
        "CCC": -0.4,
    }

    weights = weights_from_ranks(
        preds,
        long_assets=["AAA", "BBB"],
        short_assets=["CCC"],
        target_gross=1.0,
        scheme="rank_power",
        power=2.0,
    )

    assert weights["AAA"] > weights["BBB"] > 0
    assert weights["CCC"] < 0
    assert sum(abs(w) for w in weights.values()) == pytest.approx(1.0)


def test_weights_from_ranks_skips_assets_without_scores():
    preds = pl.DataFrame({"id": ["AAA"], "pred": [0.5]})

    weights = weights_from_ranks(
        preds,
        long_assets=["AAA", "BBB"],
        short_assets=[],
        target_gross=1.0,
    )

    assert "AAA" in weights
    assert "BBB" not in weights
    assert sum(abs(w) for w in weights.values()) == pytest.approx(1.0)


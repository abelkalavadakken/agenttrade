import pytest

from agenttrade_agents.config import Config


def test_live_mode_is_rejected():
    with pytest.raises(ValueError, match="no live mode"):
        Config(mode="live")


def test_roundtrip(tmp_path):
    cfg = Config(mode="testnet", symbol="ETH/USD", clock_seconds=30, limits={"max_order_qty": "0.5"})
    path = tmp_path / "config.toml"
    cfg.write(path)
    assert oct(path.stat().st_mode & 0o777) == "0o600"
    back = Config.load(path)
    assert (back.mode, back.symbol, back.clock_seconds, back.limits) == ("testnet", "ETH/USD", 30, {"max_order_qty": "0.5"})


def test_loading_live_mode_from_file_is_rejected(tmp_path):
    path = tmp_path / "config.toml"
    path.write_text('[venue]\nmode = "live"\n')
    with pytest.raises(ValueError):
        Config.load(path)

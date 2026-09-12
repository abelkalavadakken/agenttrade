from agenttrade_agents.state import Instrument


def test_fixed_point_formatting_never_uses_floats():
    i = Instrument("k", "s", price_scale=1, qty_scale=8, tick=1, lot=1)
    assert i.price(612405) == "61240.5"
    assert i.price(-5) == "-0.5"
    assert i.qty(100000000) == "1.00000000"
    assert i.qty(1) == "0.00000001"


def test_state_text_has_every_field(state):
    t = state.as_text()
    for key in ("bid=61240.0", "ask=61241.0", "spread_ticks=10", "rsi=54.2", "ofi=31", "position_qty=0.00000000"):
        assert key in t

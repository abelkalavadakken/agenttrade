from __future__ import annotations

from typing import Any

import pytest

from agenttrade_agents.brain import Reply, Turn
from agenttrade_agents.state import Features, Instrument, Position, State


class FakeBrain:
    """Returns canned output per agent_id and records every turn."""

    def __init__(self, outputs: dict[str, Any]):
        self.outputs = outputs
        self.turns: list[Turn] = []

    def think(self, turn: Turn) -> Reply:
        self.turns.append(turn)
        out = self.outputs[turn.agent_id]
        if isinstance(out, Exception):
            raise out
        return Reply(output=out, cost_usd=0.0, duration_ms=1, session_id="s")


@pytest.fixture
def state() -> State:
    inst = Instrument("kraken", "BTC/USD", price_scale=1, qty_scale=8, tick=1, lot=1)
    return State(1, 0, inst, 612400, 612410, Position(0, 0, 0), Features(54.2, 61238.5, 61190.2, 31), 100000)


@pytest.fixture
def limits() -> dict[str, str]:
    return {"max_position_qty": "0.05", "max_order_qty": "0.01", "max_daily_loss": "50"}

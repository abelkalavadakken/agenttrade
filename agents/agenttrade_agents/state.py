"""Core state as the agents see it. Mirrors GetStateResponse. Integers stay
integers; formatting to decimal text happens once, here, using the
instrument scales."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Protocol


@dataclass(frozen=True)
class Instrument:
    venue: str
    symbol: str
    price_scale: int
    qty_scale: int
    tick: int
    lot: int

    def price(self, v: int) -> str:
        return _fmt(v, self.price_scale)

    def qty(self, v: int) -> str:
        return _fmt(v, self.qty_scale)


@dataclass(frozen=True)
class Position:
    net_qty: int
    average_entry_price: int
    unrealized_pnl: int


@dataclass(frozen=True)
class Features:
    rsi: float
    ema_fast: float
    ema_slow: float
    order_flow_imbalance: int


@dataclass(frozen=True)
class State:
    sequence_id: int
    timestamp_ns: int
    instrument: Instrument
    bid: int
    ask: int
    position: Position
    features: Features
    available_equity: int

    def as_text(self) -> str:
        i = self.instrument
        p = self.position
        f = self.features
        lines = [
            f"venue={i.venue} symbol={i.symbol} seq={self.sequence_id}",
            f"bid={i.price(self.bid)} ask={i.price(self.ask)} spread_ticks={(self.ask - self.bid) // i.tick}",
            f"position_qty={i.qty(p.net_qty)} avg_entry={i.price(p.average_entry_price)} unrealized_pnl={i.price(p.unrealized_pnl)}",
            f"rsi={f.rsi:.1f} ema_fast={f.ema_fast:.2f} ema_slow={f.ema_slow:.2f} ofi={f.order_flow_imbalance}",
            f"available_equity={i.price(self.available_equity)}",
            f"tick={i.price(i.tick)} lot={i.qty(i.lot)}",
        ]
        return "\n".join(lines)


class StateSource(Protocol):
    def get_state(self) -> State: ...


class FixedStateSource:
    """Serves one state. For tests and for `tick` runs before the core is wired."""

    def __init__(self, state: State):
        self.state = state

    def get_state(self) -> State:
        return self.state


def _fmt(v: int, scale: int) -> str:
    if scale == 0:
        return str(v)
    sign = "-" if v < 0 else ""
    v = abs(v)
    whole, frac = divmod(v, 10**scale)
    return f"{sign}{whole}.{frac:0{scale}d}"

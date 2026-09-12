# crates/exec

Order state machine, paper venue, position and equity. Deterministic on
replay: every input is a tape event or a tape-recorded intent, and every
timestamp is tape time, never wall clock.

## Order

```rust
pub struct Order {
    pub id: u64,                 // core-assigned, monotonic
    pub intent_id: String,
    pub side: Side,
    pub price: Price,
    pub stop: Price,
    pub qty: Qty,
    pub filled: Qty,
    pub tif: TimeInForce,
    pub state: OrderState,
    pub reason: Option<CancelReason>,
    pub submitted_ns: i64,
    pub updated_ns: i64,
}

pub enum CancelReason { Requested, IocUnfilled, FokUnfillable, Flatten }
```

## State machine

`OrderState` is in crates/types: PendingNew, Open, PartiallyFilled, Filled,
PendingCancel, Canceled, Rejected.

| From | Event | To |
|---|---|---|
| PendingNew | venue ack | Open |
| PendingNew | venue reject | Rejected |
| PendingNew | full fill on ack (IOC, FOK, or marketable GTC) | Filled |
| PendingNew | partial fill on ack, IOC | Canceled, reason IocUnfilled, `filled > 0` |
| PendingNew | partial fill on ack, GTC | PartiallyFilled |
| PendingNew | no fill on ack, IOC | Canceled, reason IocUnfilled |
| PendingNew | cannot fill in full on ack, FOK | Canceled, reason FokUnfillable |
| Open | partial fill | PartiallyFilled |
| Open | full fill | Filled |
| Open | cancel request | PendingCancel |
| PartiallyFilled | partial fill | PartiallyFilled |
| PartiallyFilled | full fill | Filled |
| PartiallyFilled | cancel request | PendingCancel |
| PendingCancel | cancel ack | Canceled, reason Requested |
| PendingCancel | fill arrives before cancel ack | Filled or stays PendingCancel with more `filled` |

Terminal: Filled, Canceled, Rejected. Any other transition is a bug and
panics in debug, logs and ignores in release. `OrderState::is_terminal`
already exists.

Cancel on a terminal order is a no-op that reports the current state. Cancel
of an unknown id is `INVALID_INTENT` at the api layer.

## Paper venue

The default venue. It never sends anything anywhere. It reads the book and
produces acks and fills.

**Clock.** Every action is scheduled at `event_ns + latency`. `event_ns` is
the tape receive timestamp of the event that caused it. The venue keeps a
min-heap of pending actions and drains everything due on each book event.
This makes a replay produce the same fills from the same tape regardless of
how fast it runs.

```rust
pub struct PaperConfig {
    pub ack_latency_ns: i64,      // submit -> Open
    pub fill_latency_ns: i64,     // book crossing -> fill applied
    pub cancel_latency_ns: i64,   // cancel request -> Canceled
}
```

**Fill model: at touch, against displayed size.** A buy fills against asks
whose price is at or below the limit, best first; a sell against bids at or
above the limit. Each level contributes at most its displayed qty. The fill
price is the level price, so a limit better than the touch gets price
improvement. Partial fills are allowed: the order takes what is there and
rests for the remainder (GTC), cancels the remainder (IOC), or fills nothing
(FOK, if the sum of eligible displayed qty is under the order qty).

The venue does not remove liquidity from the book. The book is what the
venue published, and our paper order was never in it. A resting order is
re-evaluated on every book event; it fills when the touch reaches its price
and only against the qty displayed at that event. Queue position is not
modelled: when the touch reaches a resting price we assume we are at the
front. That is optimistic and is stated in README next to any P&L number.

**Stops.** A `Place` carries a stop. The paper venue registers a stop order
that triggers when the touch reaches the stop price (bid for a long's stop,
ask for a short's) and then fills as a market order against the book, full
depth, walking levels until done. If the depth-10 book cannot absorb it the
remainder fills at the last level's price and the event is flagged
`thin_book`. Stops for a position are cancelled when the position is flat.

**Rejects.** The paper venue rejects nothing; the risk gate already did.
`Rejected` exists for real venue adapters.

## Position and equity

```rust
pub struct Position { pub net_qty: Qty, pub average_entry_price: Price, pub realized_pnl: i64 }
```

`net_qty` positive is long. Prices are at price_scale, qty at qty_scale.
Quote amounts (pnl, equity) are at price_scale. The product
`qty * price` is at `price_scale + qty_scale` and is divided by
`10^qty_scale` once, at the end of each formula, in i128, then narrowed with
`try_from`; failure to narrow is a panic in exec because it means the
position is outside any range this system should hold.

Fill of `fill_qty` at `fill_price`, signed `+` for buy, `-` for sell:

- **Increasing** (same sign as `net_qty`, or flat):
  `avg = (avg * |net| + price * |fill|) / (|net| + |fill|)`, i128, rounded
  toward zero. `net += fill`.
- **Reducing** (opposite sign, `|fill| <= |net|`):
  `realized += sign(net) * (price - avg) * |fill| / 10^qty_scale`.
  `net += fill`. `avg` unchanged; if `net` becomes 0, `avg = 0`.
- **Flipping** (opposite sign, `|fill| > |net|`): apply Reducing for `|net|`,
  then Increasing from flat for the remainder at `fill_price`.

Unrealized: `(mark - avg) * net / 10^qty_scale` with `mark = book.mid()`.
Sign follows `net`. Equity: `starting_cash + realized + unrealized`. Peak
equity is the running max of equity and feeds the drawdown kill switch.

No fees or funding in the paper venue. Stated in README.

## Events on the tape

Exec writes, in order, before acting: the intent, the verdict, the order
transition, each fill with price and qty, the resulting position. Replay in
recorded mode asserts these against the tape.

## Tests

- Every row of the transition table, as one test each.
- Illegal transitions panic in debug.
- Fill at touch: buy at ask fills the displayed qty and no more; buy above
  ask fills at the ask, not the limit.
- Partial then rest then fill on a later book event.
- IOC partial cancels the remainder; FOK with insufficient depth fills
  nothing.
- Latency: an ack scheduled at `t + ack` is not visible at `t + ack - 1`.
- Position arithmetic: increase, reduce, flip, flat, each against a hand
  computed i128 value; realized plus unrealized equals mark-to-market of the
  whole history.
- Determinism: replay the 600 s tape twice with a scripted intent sequence
  and assert identical fill lists and final position.

## Open decisions

1. Stop fills walk full displayed depth and flag `thin_book` when the depth
   runs out, versus resting the remainder. Proposed: walk and flag.
2. Queue position not modelled. Proposed: accept for the paper venue, state it
   next to every P&L number.
3. `Position` grows a `realized_pnl` field. The proto `Position` has
   `unrealized_pnl` only; realized would be a proto change in its own PR.

# crates/risk

The gate. Every intent passes through `Gate::check` before exec sees it.
Architecture section 3 lists six mandatory checks; this note gives each one
its `RejectionCode`, its inputs, and its arithmetic. Everything is int64
fixed-point. Overflow in any check is a rejection, never a wrap.

## Inputs

```rust
pub struct RiskInputs<'a> {
    pub instrument: &'a Instrument,
    pub book: &'a Book,                 // best_bid, best_ask, mid, is_stale
    pub book_sequence_id: u64,          // core sequence at the last applied event
    pub venue_connected: bool,          // feed client has a live session
    pub instrument_halted: bool,        // venue status frame says not online
    pub position: Position,             // net_qty, average_entry_price (types)
    pub equity: i64,                    // quote units at price_scale
    pub peak_equity: i64,               // high-water mark, same scale
    pub open_orders: u32,
    pub intents_in_window: u32,         // submissions in the current window
    pub kill_switch: bool,              // operator flag
    pub now_ns: i64,
}

pub struct RiskConfig {
    pub max_sequence_drift: u64,        // check 2
    pub price_band_bps: i64,            // check 3, e.g. 50
    pub risk_per_trade_bps: i64,        // check 4, 100 = the 1% rule
    pub max_leverage_bps: i64,          // check 4b, 10_000 = 1x
    pub max_open_orders: u32,           // check 5
    pub max_intents_per_window: u32,    // check 5
    pub window_ns: i64,                 // check 5
    pub max_drawdown_bps: i64,          // check 6, drawdown from peak that trips the switch
}
```

`Position` and equity are maintained by exec (docs/exec.md). Risk only reads.

## Verdict

```rust
pub struct Verdict { pub decision: RiskDecision, pub code: RejectionCode, pub reason: String }
```

Checks run in the order below and stop at the first failure. The order puts
the checks that make everything else meaningless first, then the cheap
structural ones, then the arithmetic.

## Checks

### 0. Intent shape, `INVALID_INTENT`

Done by crates/api when it converts `SubmitIntentRequest` into `Intent`
(docs/types.md). Risk never sees an unspecified side, tif or type. Listed
here so the order is complete.

### 6. Kill switch, `KILL_SWITCH_ACTIVE`

Runs first because nothing else matters if it is set.

- Manual: `kill_switch == true`.
- Drawdown: `equity < peak_equity - peak_equity * max_drawdown_bps / 10_000`.
  Computed as `peak_equity.checked_mul(max_drawdown_bps)` then `/ 10_000`;
  overflow trips the switch.

Once tripped the flag stays set until an operator clears it. Risk does not
clear it.

### 1. Instrument and market state, `VENUE_DISCONNECTED` / `INSTRUMENT_HALTED`

- `instrument` found in `instruments::find(venue, symbol)`; otherwise
  `INVALID_INTENT`.
- `venue_connected == true`, else `VENUE_DISCONNECTED`.
- `book.is_stale() == false`, else `VENUE_DISCONNECTED`. Stale covers
  checksum failure, crossed, silence and disconnect (docs/book.md).
- `book.best_bid()` and `book.best_ask()` both present, else
  `VENUE_DISCONNECTED`.
- `instrument_halted == false`, else `INSTRUMENT_HALTED`. The feed client
  sets it from Kraken's status frame: `system` anything but `online`
  (`maintenance`, `cancel_only`, `post_only`) halts new placements. A halt
  is a venue fact, not a connectivity fact, so it gets its own code on the
  tape.

### 2. Staleness, `STALE_STATE`

`book_sequence_id.saturating_sub(intent.source_sequence_id) <= max_sequence_drift`.
An intent whose `source_sequence_id` is ahead of the book is also stale:
`intent.source_sequence_id > book_sequence_id` rejects.

### Alignment, `INVALID_TICK_SIZE`

Applies to `Place` only.

- `instrument.is_tick_aligned(price)` and `is_tick_aligned(stop)`.
- `instrument.is_lot_aligned(qty)` and `qty > 0`. Lot misalignment shares the
  code; the reason string says which.

### 3. Price sanity, `PRICE_OUT_OF_BAND`

Applies to `Place`. `mid = book.mid()` (advisory, rounds down, docs/book.md).

```
diff      = (price - mid).abs()
lhs       = diff.checked_mul(10_000)
rhs       = mid.checked_mul(price_band_bps)
reject if lhs is None || rhs is None || lhs > rhs
```

Both products are at most ~1e6 * 1e4 for BTC/USD at price_scale 1, far
inside i64. The checked forms are there for instruments with larger scales.

Also `PRICE_OUT_OF_BAND`: a buy limit above best ask plus the band or a sell
below best bid minus the band. The mid check already covers this; the reason
string names the side.

### 4. Stop and sizing

**`MISSING_STOP`.** `Place` with `stop == 0`. A stop on the wrong side of
the price (`Buy` with `stop >= price`, `Sell` with `stop <= price`) rejects
with `INVALID_INTENT`, because a stop that cannot trigger is not a stop.
Decided 2026-09-14.

**`EXCEEDS_SINGLE_LOSS_LIMIT`, the 1% rule.** The loss if the stop fills in
full must not exceed `risk_per_trade_bps` of equity.

Units: `qty` is at `qty_scale` (1e8 for BTC), prices at `price_scale`.
The loss in quote units at price_scale is `qty * |price - stop| / 10^qty_scale`.
To avoid the division and its rounding, compare cross-multiplied:

```
risk_ticks = |price - stop|                              // price units
loss_num   = qty.checked_mul(risk_ticks)                 // qty_scale * price_scale
budget_num = equity.checked_mul(risk_per_trade_bps)      // price_scale * bps
             .checked_mul(10^qty_scale)                  // qty_scale * price_scale * bps
loss_num.checked_mul(10_000)  <=  budget_num
```

Any `None` rejects. Worked example for BTC/USD, equity 100,000 USD (`1_000_000` at
price_scale 1), 1% rule (budget 1,000 USD), price 77,362.8, stop 77,000.0,
qty 0.5 BTC (`50_000_000` at qty_scale 8):

```
risk_ticks        = 773628 - 770000            = 3628
loss_num          = 50_000_000 * 3628          = 181_400_000_000   (~1.8e11)
budget_num        = 1_000_000 * 100 * 1e8      = 1e16
loss_num * 10_000 = 1.814e15  <=  1e16          -> allowed
```

Cross-check in USD: 0.5 BTC * 362.8 USD = 181.40 USD, under the 1,000 USD
budget. In integers, `loss_num / 10^qty_scale = 1_814` at price_scale 1,
which is 181.4 USD. The two agree.

Overflow bounds: `qty * risk_ticks * 10_000` for a 1,000 BTC order
(`1e11`) with a 10,000 USD stop distance (`1e5`) is `1e20`, past i64. That
rejects with `EXCEEDS_SINGLE_LOSS_LIMIT` and a reason of "overflow", which
is the right answer for an order that size. Normal orders stay under 1e17.

**`EXCEEDS_MAX_LEVERAGE`.** Notional after the fill must not exceed
`max_leverage_bps` of equity.

```
new_net    = position.net_qty + signed(qty)               // signed by side
notional   = |new_net|.checked_mul(price)                 // qty_scale * price_scale
limit      = equity.checked_mul(max_leverage_bps).checked_mul(10^qty_scale)
notional.checked_mul(10_000) <= limit
```

`Flatten` skips sizing and leverage: it only reduces. `Cancel` and `Noop`
skip everything from Alignment onward.

### 5. Rate limiting, `RATE_LIMIT_EXCEEDED`

- `open_orders < max_open_orders` for `Place`.
- `intents_in_window < max_intents_per_window`. The window is a fixed
  interval starting at the first intent; exec supplies the count.

## What risk does not do

Risk does not mutate state, does not touch the venue, and does not know
about fills. It reads a snapshot of inputs and returns a verdict. The api
crate records the verdict on the tape before exec acts on it.

## Tests

- One test per code with an input that trips exactly that check.
- Order test: an intent failing several checks reports the first in the
  order above.
- 1% rule boundary: loss exactly at budget passes, one lot more fails.
- Overflow: a qty and stop distance whose product exceeds i64 rejects
  rather than panicking or wrapping, under `overflow-checks` on and off.
- Leverage flip: a sell that turns a long into a larger short is measured on
  the new absolute net.
- proptest: for random equity, price, stop and qty inside i64/1e4, the
  integer check agrees with an i128 reference computation.

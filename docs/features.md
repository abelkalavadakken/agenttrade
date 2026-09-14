# crates/features

Written from the consumer's side. The consumers are the observer, which
turns numbers into a narrative, and through it the analyst and trader.
Risk never reads a feature. Features are `f64` and advisory; state and
execution stay fixed-point. Replay determinism holds because every
feature is a pure function of tape events in tape order using only
`+ - * /`, no platform-dependent math.

Agents read features once per tick, on a 60 s clock. The core updates
them on every event, but nothing between ticks is observed. So a
feature earns its place only if a 60 s snapshot of it changes what an
agent says or proposes.

## Inputs we have and do not have

The Kraken feed today carries the book only. There is no trade channel
on the tape. Two features below want trades. Until the feed subscribes
to Kraken's `trade` channel and the tape records it, they run in a
degraded book-only form and say so. Adding the channel is a feed PR and
a tape format change, and it comes before the trade-based forms here.

## OHLCV bars, 1 minute

**Agent use.** The observer narrates the last 15 bars: where the close
sits in the hour's range, whether highs and lows are stepping up or
down, and whether volume is rising into a move or fading. That gives
the analyst a path instead of a point. The regime enum the analyst
emits (`trend_up`, `trend_down`, `range`, `unclear`) is not decidable
from a single bid and ask.

**Window and cadence.** 1 minute bars, ring of 512 closed bars (the
observer reads 15; the ring is sized for the Kronos lookback below). A bar
closes on the minute boundary of tape receive timestamps, never on message
arrival and never on the machine clock, so replay closes the same bars.
`advance(now_ns)` closes every bar whose boundary has passed, so a quiet
minute still closes as a gap bar. The forming bar is exposed separately
from closed bars.

**Input.** Trades: open, high, low, close from trade prices, volume from
trade quantity. Book-only degraded form: open, high, low, close from
mid, volume absent. The observer prompt says "volume unavailable" in the
degraded form rather than printing 0.

**When the book is stale.** No samples enter the forming bar. A bar with
a gap is marked `gap = true` and the observer says the bar is
incomplete. Closed bars before the gap stay valid.

## EMA fast and slow, 12 and 26 bars

**Agent use.** Fast above slow with a widening gap is the analyst's
trend evidence. Fast crossing slow is the one signal the analyst may
cite as a regime change. The trader uses the slope of the slow EMA to
place a stop on the losing side of the trend rather than a fixed
distance.

**Window and cadence.** Over 1 minute closes. Updated on bar close only.
A tick between bar closes sees the same value, which is intended: the
EMA is meant to be slower than the agent clock, not faster.

**Input.** Bar closes, so trades when we have them, mid until then.

**When the book is stale.** Value held. `bars_since_update` is exposed
and grows. The observer reports "EMA last updated N minutes ago" once N
is above 1. A held EMA is still a fact about the last known market; a
missing one is not, so we hold rather than clear.

## RSI, 14 bars

**Agent use.** The analyst treats readings above 70 or below 30 as a
reason to lower confidence in a continuation, not as a signal on its
own. The trader prompt already prefers NOOP under confidence 0.6, so
an extreme RSI mostly produces NOOPs, which is the intended behavior
for a system that should trade rarely.

**Window and cadence.** Wilder's smoothing over 14 one-minute closes.
Bar close only, same as EMA.

**Input.** Bar closes.

**When the book is stale.** Held, with the same `bars_since_update` as
EMA. Both share one bar clock, so one counter serves both.

## Order flow imbalance, 60 second window

**Agent use.** This is the one feature that reads the book itself and
the only one that moves faster than a bar. The observer states the sign
and rough size: "buy pressure at the top of book over the last minute".
The analyst uses it to confirm or doubt the bar trend. A trend with
flow against it lowers confidence.

**Window and cadence.** Sum over the trailing 60 s of per-update OFI,
Cont, Kukanov and Stoikov form: change in best bid quantity when the
best bid price holds or improves, minus the same for the ask. Updated on
every accepted book delta. Exposed as `int64` in quantity fixed-point
units, matching `order_flow_imbalance` in the proto, because it is a
sum of quantities and needs no float.

**Input.** Book only. Best level of each side, before and after each
delta.

**When the book is stale.** Reset to 0 and flagged. There is no flow to
measure on a book we do not trust, and a held OFI would be a claim about
flow that did not happen.

## What the agents see

`GetStateResponse.features` today has `rsi`, `ema_fast`, `ema_slow` and
`order_flow_imbalance`. Bars and the staleness fields above are not in
the proto. The proposed addition, its own PR:

```
message Bar { int64 open = 1; int64 high = 2; int64 low = 3; int64 close = 4;
              int64 volume = 5; int64 close_ns = 6; bool gap = 7; }
message MarketFeatures {
  double rsi = 1; double ema_fast = 2; double ema_slow = 3;
  int64 order_flow_imbalance = 4;
  repeated Bar bars = 5;          // closed, oldest first, up to 15
  Bar forming = 6;
  uint32 bars_since_update = 7;   // 0 when fresh
  bool volume_available = 8;
  bool book_stale = 9;
}
```

Bar prices are fixed-point `int64` like everything else in state; only
the smoothed values are `double`.

## Tests

- Unit: known closes produce the textbook EMA and RSI values to 1e-9.
- Bars: events straddling a minute boundary close exactly one bar with
  the right open and close; a gap marks the bar.
- OFI: hand-built deltas at the best level give the expected signed sum;
  a delta below the best level contributes 0.
- Replay: two runs of the same tape produce byte-identical feature
  streams. Any difference is a bug.

## Not yet

Each of these lacks an agent use I can state in one sentence today.

- Depth-weighted book imbalance. OFI covers flow; static imbalance has
  not shown it changes a read.
- Realized volatility. Would matter for sizing, but sizing is a fixed
  limit in config for now.
- VWAP. No trades yet, and no agent asks where the average fill sits.
- Trade intensity. Same dependency, same missing consumer.
- Regime classifier and `RegimeShiftPayload`. The proto has it; nothing
  emits it. The analyst names the regime today. Automating it is a
  separate note once we have tapes showing what the analyst calls.
- Anything cross-venue. One venue.
- Kronos forecast input. A foundation model over K-line bars that wants a
  512-bar lookback of OHLCV. The bar ring is already 512 deep and `Bar`
  carries what it needs; what is missing is the consumer, a model runtime,
  and a note on how a forecast reaches an agent without becoming a signal
  the risk gate never saw.

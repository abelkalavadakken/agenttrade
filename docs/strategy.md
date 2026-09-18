# crates/strategy

Deterministic strategies run on the core loop. The LLM does not place
orders by default. It has four bounded verbs, allocate, tune, confirm or
reject a gated setup, and a capped discretionary PLACE, plus flatten-all.
Every verb and every strategy order goes through the risk gate. Design
note, written before code.

## 1. The trait

```rust
pub trait Strategy {
    fn id(&self) -> &'static str;                 // stable, appears on every order
    fn horizon(&self) -> Horizon;                 // decides the mode, see 3
    fn params(&self) -> &[Param];                 // name, value, hard min, hard max
    fn set_param(&mut self, name: &str, value: i64) -> Result<(), ParamError>;
    fn on_event(&mut self, state: &State) -> Option<Action>;   // Place(Order) or Flatten
    fn setup_state(&self) -> SetupState;
    fn on_fill(&mut self, fill: &Fill);           // its own fills only
    fn reset(&mut self);                          // after disable, before enable
}

pub enum Horizon { Seconds, Minutes, Hours }
pub enum Mode { Autonomous, Gated }

pub struct Param { pub name: &'static str, pub value: i64, pub min: i64, pub max: i64 }

pub struct State<'a> {
    pub now_ns: i64,
    pub sequence_id: u64,
    pub book: &'a Book,
    pub features: &'a features::Snapshot<'a>,
    pub position: Position,      // this strategy's own net, not the account's
    pub open_orders: u32,        // this strategy's own
}

pub struct Order {
    pub side: Side,
    pub price: Price,
    pub stop: Price,
    pub qty: Qty,                // base size; the runner applies size_multiplier
    pub tif: TimeInForce,
    pub reason: &'static str,    // one line, goes on the tape and into the Wake
}

/// Exits go through the strategy's own Flatten so they never need a stop
/// and never race the protection exec already holds.
pub enum Action { Place(Order), Flatten { reason: &'static str } }

pub enum SetupState {
    None,
    Forming,                     // conditions building, nothing proposed
    Active { setup_id: u64, order: Order },
}
```

Params are `i64` in the same fixed-point units as everything else (ticks,
lots, nanoseconds, bps). `set_param` outside `[min, max]` is
`ParamError::OutOfBounds` and changes nothing; bounds are compile-time
constants in the strategy and cannot be tuned. A strategy never reads a
clock, never allocates after construction, and sees only `State`.

## 2. Allocation

```rust
pub struct Allocation {
    pub enabled: bool,
    pub size_multiplier_bps: i64,   // 10_000 = 1x; bounds in docs/risk.md
    pub on_disable: OnDisable,
}
pub enum OnDisable { Flatten, Hold }
```

Set by the LLM's ALLOCATE verb through the gate, or by config at start.
Default: every strategy disabled, multiplier 1x, `Flatten` on disable.

**Disable mid-position.** `Flatten` (default): the runner cancels the
strategy's open orders (reason `Flatten`) and sends an IOC for the
strategy's net at the worst displayed level, attributed to the strategy,
then calls `reset()`. `Hold`: open orders are cancelled, the position stays
and keeps its stop, the strategy is reset and its P&L keeps accruing
under its id until flat. Re-enabling a held strategy starts it from its
existing position; `on_event` sees it in `State.position`.

## 3. Modes and the horizon

The horizon decides the mode; a strategy does not choose.

| horizon | mode | why |
|---|---|---|
| Seconds | Autonomous | an LLM turn takes longer than the edge lasts |
| Minutes, Hours | Gated | there is time to ask, and the LLM has context the core lacks |

**Autonomous.** `on_event` returns `Some(order)`, the runner applies the
multiplier, wraps it as an `IntentEnvelope` with `agent_id = strategy id`,
and the core runs it through the gate like any other intent.

**Gated.** `on_event` returns `None` but `setup_state()` becomes
`Active { setup_id, order }`. The runner emits a `Wake::SetupActive` with
the strategy id, setup id, the proposed order and reason, a snapshot of the
state that produced it, and the TTL. The order is held. Then one of:

- CONFIRM_SETUP `{ strategy_id, setup_id, size_multiplier_bps }` arrives
  through the gate: the held order is sent with that multiplier applied on
  top of the allocation multiplier. `size_multiplier_bps = 0` is a reject.
- REJECT_SETUP arrives: the setup is cleared, the strategy is told via
  `on_setup_rejected` (default no-op), counted under `rejected`.
- The TTL passes on the core clock: the setup expires, counted under
  `expired`, and the strategy is not re-asked until `setup_state()` has
  been `None` or `Forming` at least once. Silence never trades.

A setup id is `sequence_id` at the moment it went Active; unique because
one strategy has at most one active setup and the sequence only rises.
A confirm naming a setup id that is no longer active is rejected by the
gate with `INVALID_INTENT` and reason "setup not active". The proposed
order is re-checked against the live book at confirm time by the gate;
the strategy's own price is not refreshed, so a stale price rejects as
`PRICE_OUT_OF_BAND` rather than chasing.

TTL is a strategy param (`ttl_ns`) with hard bounds; the runner reads it.

## 4. The runner

`Runner` lives in crates/strategy and is called by the core loop after
every accepted book event, trade, tick, and fill, in that order, with the
same `State` the hash covers. It owns the strategies, the allocations, the
held setups, and the counters. It returns a `Vec<RunnerOutput>`:

```rust
pub enum RunnerOutput {
    Intent(IntentEnvelope),                 // through the gate, agent_id = strategy id
    Wake(Wake),                             // to StreamEvents
    Expired { strategy_id, setup_id },      // to the tape and the counter
}
```

The runner never talks to exec or risk. The core loop does, with the same
code path as an LLM intent, so a strategy order and an LLM order are
indistinguishable on the tape except for `agent_id`.

**P&L attribution.** exec keeps one `Position` per `strategy_id` next to
the account position. Every order carries its `strategy_id`
(`IntentEnvelope.agent_id`); fills update both. `StrategyState` in
`GetStateResponse` reports each strategy's `net_qty` and `realized_pnl`.
Discretionary LLM orders use `strategy_id = "discretionary"`.

**Counters, per strategy:** `setups`, `confirmed`, `rejected`, `expired`,
`orders_sent`, `orders_rejected_by_gate`. All in `StrategyState`, all on
the tape at shutdown, all in the hash.

## 5. Reference strategy: mean reversion on spread and OFI

`id = "mr_ofi"`, horizon Seconds, Autonomous.

Idea: when one side of the top of book has been hit hard over the last
minute and the spread has widened, price tends to snap back a few ticks.
Fade the flow, join the touch, tight stop.

| param | default | min | max | unit |
|---|---|---|---|---|
| `ofi_threshold` | 2 BTC | 0.1 BTC | 50 BTC | qty units |
| `min_spread_ticks` | 2 | 1 | 100 | ticks |
| `size` | 0.01 BTC | 0.0001 | 1 BTC | qty units |
| `stop_ticks` | 50 | 5 | 5000 | ticks |
| `hold_ns` | 30 s | 1 s | 600 s | ns |
| `cooldown_ns` | 60 s | 0 | 3600 s | ns |

Rules, evaluated on every accepted book event:

- flat, not in cooldown, `spread >= min_spread_ticks`, and
  `ofi <= -ofi_threshold`: buy at best bid, stop `stop_ticks` below, GTC.
- symmetric for `ofi >= ofi_threshold`: sell at best ask, stop above.
- in position: exit when the OFI sign flips, or after `hold_ns`, by
  returning an IOC for the net at the touch. Then cooldown.
- book stale or `bars_since_update > 0` at a bar close: no entries.

## 6. Reference strategy: breakout on N-bar range

`id = "breakout"`, horizon Minutes, Gated.

Idea: a close above the high of the last N one-minute bars, with volume on
that bar above the N-bar average, is a setup worth asking about.

| param | default | min | max | unit |
|---|---|---|---|---|
| `lookback_bars` | 20 | 5 | 200 | bars |
| `size` | 0.02 BTC | 0.0001 | 2 BTC | qty units |
| `stop_ticks` | 200 | 10 | 20000 | ticks |
| `volume_mult_bps` | 15_000 | 10_000 | 100_000 | bps of average |
| `ttl_ns` | 120 s | 10 s | 900 s | ns |

Rules, evaluated only when a bar closes (`bars.len()` changed):

- `Forming` while the last close is within the range.
- `Active` when the last closed bar is not a gap, its close is above the
  max high of the previous N closed bars, and its volume is at least
  `volume_mult_bps` of their average (requires `volume_available`; with
  mid bars the volume test is skipped and the reason says so). Proposed
  order: buy at best ask, stop `stop_ticks` below the range low, GTC.
  Symmetric below the range low.
- After confirm, reject or expiry, back to `None`; a new setup needs a new
  bar close beyond the range.

## 7. Wake

Emitted on `StreamEvents`, never coalesced.

```
Wake { reason: Timer | RegimeShift | SetupActive { strategy_id, setup_id,
       order, reason, snapshot, ttl_ms } | DrawdownLimit | VenueStatus | Operator }
```

`Timer` is the agents' 60 s clock, emitted by the core so replay in live
mode drives agents at tape pace. `snapshot` is the `GetStateResponse` at
the moment the setup went Active, so the agent can decide without a second
round trip. The proto shapes are in the proto PR.

## 8. Tests

- Trait bounds: every param rejects a value one past each bound and
  accepts both bounds.
- `mr_ofi` on hand-built OFI and spread sequences: enters on the right side
  at the touch, exits on sign flip and on hold expiry, respects cooldown,
  makes no entry on a stale book.
- `breakout` on hand-built bars: `Forming` inside the range, `Active` on
  the breakout bar with volume, not without volume, symmetric below.
- Runner on the tape fixtures: an autonomous order becomes an intent with
  the multiplier applied; a gated setup emits one Wake, expires after TTL
  with `expired = 1`, and is not re-asked on the next event; confirm with
  a wrong setup id yields no intent; disable with `Flatten` emits cancels
  and an IOC; `Hold` emits only cancels.
- Determinism: runner output over the fixtures is identical across two
  runs and is included in the hash.

## Decisions, 2026-09-18

1. Per-strategy positions live in exec, keyed by `agent_id`, where fills
   attribute. A strategy reads its own position from `State` and never
   keeps a private copy.
2. The core emits `Wake::Timer`. Interval is config, default 60 s. Agents
   may answer with Noop; the wake is recorded either way.
3. No stacking. The confirm multiplier overrides the allocation multiplier
   for that one order, clamped to `[0, allocation multiplier]`. A confirm
   can shrink or skip, never enlarge beyond what allocation granted. The
   effective multiplier is logged on the order's tape record.

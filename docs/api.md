# crates/api

The gRPC server and the process that owns the core. Design note, written
before code. Everything below runs in one process; the agents are the
only thing outside it.

## 1. One process, one owner

```text
 kraken ws ──> feed client (tokio task)
                  │ FeedMsg (raw, event, control)
                  v
            ┌──────────────── core loop (one task, owns all state) ────────────────┐
            │ tape writer │ tracker+book │ features │ risk state │ exec (paper)     │
            │ sequence_id │ state hash   │ clock    │ open-order and intent counts  │
            └──────┬──────────────────────────────────────────┬───────────────────┘
                   │ watch<StateSnapshot>       broadcast<MarketEvent>
                   v                                          v
            gRPC handlers (tonic): GetState, SubmitIntent, StreamEvents, ListInstruments
                   ^
                   │ mpsc<(IntentEnvelope, oneshot<SubmitIntentResponse>)>
```

**The core loop is the only code that mutates state.** It is one tokio
task with a `select!` over three inputs, and it processes each input to
completion before taking the next:

1. `FeedMsg` from the feed client. Raw frames, parsed events, control
   markers.
2. Intent requests from gRPC handlers, each carrying a oneshot for the
   reply.
3. A 1 s tick, which only calls `features.advance(now)` so bars close on
   a quiet market.

The gRPC handlers never touch core state. They read a `watch` channel
that the loop publishes after every accepted event, and they send intents
into an `mpsc`. This is the same shape as the feed: bytes in, events out,
one owner.

**Clock.** The loop has one clock, `now_ns`. Live: the receive timestamp
of the message being processed, which is wall clock at arrival. Replay:
the tape receive timestamp of the record being processed. Exec, features
and risk take `now_ns` as an argument; none of them read a clock. The 1 s
tick in replay is synthesized from tape timestamps, not from a timer, so
recorded mode never waits.

**Config.** `CoreConfig { instrument, depth, risk: RiskConfig, paper:
PaperConfig, features: FeaturesConfig, starting_cash, listen_addr }`.
`crates/config` (PR 2) will populate it from config.toml when it lands;
until then the entrypoint builds it in code.

## 2. Everything is recorded before it is processed

The tape gains sources. Format unchanged; the header table grows.

| id | source | bytes |
|---|---|---|
| 0 | `kraken.ws` | raw venue frame |
| 1 | `record.ctl` | connect, disconnect, resubscribe |
| 2 | `core.intent` | `SubmitIntentRequest`, prost-encoded |
| 3 | `core.verdict` | `SubmitIntentResponse`, prost-encoded |
| 4 | `core.exec` | one `ExecEvent`, prost-encoded (new proto message, see 6) |
| 5 | `core.hash` | `StateHash { sequence_id, hash: bytes }`, prost-encoded |

Order on the tape for one intent: intent, verdict, then each exec event
it caused, then the hash. Order for one venue frame: raw frame, then any
exec events it caused (fills, stop triggers), then the hash if due. The
write happens before the corresponding state change, so a crash leaves a
tape that explains every state the core reached.

The feed client already emits `FeedMsg::Raw` before parsing. The loop
writes it, then hands the bytes to the tracker.

## 3. sequence_id

`u64`, starts at 0, increments once per accepted book event (snapshot or
delta that verified). Trades, intents and ticks do not advance it. It is
the value `GetStateResponse.sequence_id` reports and the value
`SubmitIntentRequest.source_sequence_id` is checked against by the risk
gate. Replay produces the same sequence numbers because the same frames
verify in the same order.

## 4. The state hash

Computed in the core loop, after each accepted book event has been
applied to book, features and exec, and after each intent has been fully
processed. Written to the tape every 1,000 sequence ids and after every
intent, so a 600 s tape carries a few hundred hashes rather than
thousands.

**Chained.** `hash_n = H(hash_{n-1} || encode(state_n))`, `hash_0 =
H(magic)`. A divergence anywhere in the run shows in every later hash.

**What it covers.** Only fixed-point state that execution depends on.

| field | why |
|---|---|
| `sequence_id` | position in the stream |
| book: every level, both sides, best first | the thing fills happen against |
| book `stale`, `crossed` | gate inputs |
| exec: every non-terminal order as `(id, kind, side, price, qty, filled, pending_fill, state)` | the state machine |
| exec: scheduled action queue as `(due_ns, slot, action)` | pending fills and cancels |
| account: `net_qty`, `average_entry_price`, `realized_pnl`, `peak_equity` | position |
| risk: open orders, intents in window, kill switch | gate inputs |
| features: closed bar count, last closed bar `(open, high, low, close, volume, gap)`, `order_flow_imbalance` | fixed-point features only |

**What it does not cover.** `f64` values (EMA, RSI) because they are
advisory and a bit-for-bit f64 check belongs in the features determinism
test, not in the execution hash. Timestamps of individual orders, because
they are inputs, not derived state. The tape writer's own counters.

**Encoding.** A fixed little-endian layout written by a `Hasher` trait
implemented per crate (`book::Book::hash_into`, `exec::PaperVenue::hash_into`,
and so on), so each crate owns what of its state is hashed and no
serialization library is involved.

**Hash function.** BLAKE3, which the architecture note names. It is a new
dependency: `blake3: state hashes for replay determinism` in CLAUDE.md.
Asking before adding. The alternative without a dependency is
`crc32fast` over the same bytes, which detects divergence but is
32 bits; fine for catching bugs, weak as a fingerprint.

**Recorded mode** in `bin/replay`: run the tape through the same core
loop, re-inject each `core.intent` record at its tape position, and
compare every computed hash with the `core.hash` record that follows.
First mismatch prints both sequence ids and the covered fields that
differ, and exits 2. `bin/replay` today prints counts; this is the
addition step 4 of the architecture note calls for.

**Live mode**: paced by tape timestamps, serves gRPC, ignores recorded
intents. Real agents drive it.

## 5. The RPCs

**GetState.** Reads the latest `StateSnapshot` from the watch channel.
No lock on core state. Fields: `sequence_id`, `timestamp_ns` (core clock
at the event), `bid`, `ask`, `Position`, `MarketFeatures`,
`available_equity`. `available_equity` is `equity - notional of open
orders at their limit price`, the sum risk would compute; agents size
against it. Bars and the staleness fields need the `MarketFeatures`
change proposed in docs/features.md, its own proto PR.

**SubmitIntent.** The handler converts `SubmitIntentRequest` to
`IntentEnvelope`, rejecting immediately with `INVALID_INTENT` when
`intent_type` or, for PLACE, `side` or `time_in_force` is unspecified,
when a PLACE has zero quantity, or when `venue`/`symbol` is unknown.
Rejections at this layer are still written to the tape as intent then
verdict, so the tape shows what the agent sent. Valid envelopes go to the
core loop, which writes the intent, runs `risk::check`, writes the
verdict, and on APPROVED calls `exec.submit` and writes its events.
`executed_order_id` is the order id, 0 on rejection. `processed_time_ns`
is the core clock. The handler awaits the oneshot; a full queue returns
gRPC `RESOURCE_EXHAUSTED`, never a silent drop.

**StreamEvents.** A `broadcast` channel the loop publishes to.

| event | when | cadence |
|---|---|---|
| `mid_price_update` | mid changed | coalesced to at most one per 250 ms of core clock; the latest mid wins |
| `position_update` | after every fill | every fill, no coalescing |
| `regime_shift` | never yet | nothing emits it; documented in features.md |

A slow subscriber that lags the broadcast buffer (1,024 events) is
dropped with gRPC `DATA_LOSS`. Agents on a 60 s clock use `GetState`;
`StreamEvents` exists for the terminal watch and later for a
regime-shift trigger.

**ListInstruments.** `instruments::kraken()` mapped to proto names per
docs/types.md.

## 6. Proto additions, own PR

- `ExecEvent` message for tape source 4: a oneof of `OrderTransition`,
  `Fill`, `PositionUpdate`, with the fields exec already has.
- `StateHash { uint64 sequence_id; bytes hash; }` for tape source 5.
- The `MarketFeatures` and `Bar` change from docs/features.md.

Python codegen from the same proto lands with the agents work, not here.

## 7. Latency, allocation

The core loop is one task; a `SubmitIntent` waits for whatever venue
frame is being processed. Book apply is measured at 416 ns p50 (README),
so that wait is microseconds. Latency numbers for `GetState` and
`SubmitIntent` round trips go in README with commit and machine once the
crate exists. Allocation on the loop's hot path (frame in, hash out) is
proven with a bench in a later PR; the note only claims what the book and
features crates already prove.

## 8. Tests

- Conversion: every unspecified or malformed `SubmitIntentRequest` field
  yields `INVALID_INTENT` with a reason naming the field.
- Tape order: one intent produces records in the order intent, verdict,
  exec events, hash.
- Hash: replaying the 78-frame fixture twice through the loop gives the
  same hash chain; flipping one level in the book gives a different hash
  at that sequence id and every later one.
- Recorded mode: the 600 s tape with the paper_demo intents recorded
  replays with zero hash mismatches over N hashes, N reported.
- gRPC: an in-process tonic server; `GetState` reflects the last event,
  `SubmitIntent` returns the gate's code, `StreamEvents` delivers a
  position update after a fill.

## Open decisions

1. BLAKE3 as a new dependency, or crc32 over the same bytes.
2. Hash cadence: every 1,000 sequence ids plus every intent.
3. `mid_price_update` coalescing at 250 ms.
4. The core loop as a tokio task versus a dedicated OS thread with a
   blocking channel. Task is simpler and adequate at current rates; a
   thread is the change to make if a bench shows scheduler jitter.

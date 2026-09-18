# docs/architecture.md

## 1. System Overview & Technology Boundaries

**AgentTrade** separates non-deterministic AI reasoning from deterministic market data ingestion and trade execution. The architecture relies on a strictly typed, two-language stack communicating over gRPC:

+-----------------------------------------------------------------------+
|  Deterministic Core (Rust)                                            |
|  - Ultra-low latency WebSocket ingestion & L2 Book builder            |
|  - Feature math (OHLCV, EMA, RSI)                                     |
|  - Global state machine & branchless int64 math                       |
|  - Append-only event tape (Raw packets, state diffs, intents)         |
|  - Deterministic replay harness (Live & Recorded modes)               |
|  - Hard-coded Risk Gate (Kill switch, leverage, sizing limits)        |
|  - gRPC Server endpoint & venue execution adapters                    |
+----------------------------------+------------------------------------+
                                   | gRPC (Protobuf)
                                   v
+-----------------------------------------------------------------------+
|  Cognitive Agent Swarm (Python / uv)                                  |
|  - Asynchronous slow-clock orchestrator                               |
|  - Observer: Translates numeric core features into text narratives    |
|  - Swarm: Analyst -> Trader -> Reviewer                               |
|  - Local disk journaling for turn-by-turn logic tracking              |
|  - Generates unprivileged, schema-validated Intent payloads           |
+-----------------------------------------------------------------------+

## 2. The Core / Agent Boundary & The Hot Path

### Why the LLM is Off the Hot Path
1. **Latency Asymmetry:** Live market microstructure operates on microsecond intervals. State-of-the-art LLM inference latency runs between 300ms to 3,000ms. Placing an LLM on the synchronous order execution path introduces severe queue delay and slippage.
2. **Determinism vs. Heuristics:** Financial execution engines require complete determinism ($f(\text{Tape}) \to \text{State}$). LLM token generation is probabilistic.
3. **Execution Decoupling:** The Rust core runs continuously at line rate. The Python agent swarm operates asynchronously on a "slow clock" triggered by interval timers or structural regime shifts. 

The agents produce **advisory trade intents**, not direct venue orders. No external LLM output ever reaches an exchange gateway without passing through the Rust risk engine.

## 3. The Deterministic Risk Gate

Every intent submitted via `SubmitIntent` must pass an invariant validation pipeline in the `crates/risk` engine. If any check fails, the intent is immediately dropped and logged with a structured `RejectionCode`.

### Mandatory Verification Checks
1. **Instrument & Market State:** Is the target instrument tradable and not halted? Is the venue connected without sequence gaps?
2. **Staleness & Clock Skew:** Does the intent's `source_sequence_id` match the active book sequence within a configurable drift tolerance?
3. **Price Sanity (PRICE_OUT_OF_BAND):** Is the limit price within N basis points of the current mid-price? This prevents catastrophic hallucinated orders.
4. **Position Sizing & Capital Preservation:** Strict adherence to the 1% risk rule. `PLACE` intents lacking a `stop_loss` are immediately rejected (`MISSING_STOP`). 
5. **Rate Limiting & Churn:** Maximum open orders and intent submissions per interval.
6. **Kill Switch Status:** System-wide circuit breaker state (manual operator kill or catastrophic drawdown trigger).

## 4. Event-Sourced Tape & Deterministic Replay

The Rust core relies on an event-sourced tape for perfect state reconstruction. To prove determinism, the core computes a rolling state hash (e.g., Blake3) at each `sequence_id`.

The `replay` binary supports two distinct modes:

* **Recorded Mode (Regression Test):** Replays both the market data and the *recorded agent responses* from the tape. The core asserts its newly computed state hashes against the historical hashes on the tape. Any divergence flags a broken deterministic invariant.
* **Live Mode (Prompt Evaluation):** Replays the historical market data but allows the *real Python LLM agents* to process it dynamically. This is how new system prompts or model versions are evaluated against historical market regimes.

## 5. gRPC Contract & Message Shapes

The boundary between the Rust execution engine (`crates/api`) and the Python agent swarm (`agents/`) is strictly typed using Protocol Buffers. In line with high-performance financial systems, prices and quantities are handled as raw 64-bit integers, with scaling factors managed at the `Instrument` definition level to avoid floating-point errors and per-message bloat.

```protobuf
syntax = "proto3";

package agenttrade.v1;

// Service exposed by the Rust Core to Python Agents
service AgentCoreService {
  rpc GetState(GetStateRequest) returns (GetStateResponse);
  rpc SubmitIntent(SubmitIntentRequest) returns (SubmitIntentResponse);
  rpc StreamEvents(StreamEventsRequest) returns (stream MarketEvent);
  rpc ListInstruments(ListInstrumentsRequest) returns (ListInstrumentsResponse);
}

// ---------------- Metadata & Types ----------------
message Instrument {
  string venue = 1;
  string symbol = 2;
  int32 price_scale = 3;  // e.g., 8 for 1e-8
  int32 qty_scale = 4;
  int64 min_tick_size = 5;
  int64 min_lot_size = 6;
}

// ---------------- Enums ----------------
enum OrderSide {
  SIDE_UNSPECIFIED = 0;
  BUY = 1;
  SELL = 2;
}

enum IntentType {
  INTENT_UNSPECIFIED = 0;
  PLACE = 1;
  CANCEL = 2;
  FLATTEN = 3;
  NOOP = 4;
}

enum TimeInForce {
  TIF_UNSPECIFIED = 0;
  GTC = 1; // Good Til Canceled
  IOC = 2; // Immediate or Cancel
  FOK = 3; // Fill or Kill
}

enum RiskDecision {
  DECISION_UNSPECIFIED = 0;
  APPROVED = 1;
  REJECTED = 2;
}

enum RejectionCode {
  NONE = 0;
  STALE_STATE = 1;
  EXCEEDS_MAX_LEVERAGE = 2;
  EXCEEDS_SINGLE_LOSS_LIMIT = 3;
  INVALID_TICK_SIZE = 4;
  VENUE_DISCONNECTED = 5;
  KILL_SWITCH_ACTIVE = 6;
  RATE_LIMIT_EXCEEDED = 7;
  MISSING_STOP = 8;
  PRICE_OUT_OF_BAND = 9;
  INVALID_INTENT = 10;
  INSTRUMENT_HALTED = 11;
}

// ---------------- RPC Payloads ----------------
message ListInstrumentsRequest {}

message ListInstrumentsResponse {
  repeated Instrument instruments = 1;
}

message GetStateRequest {
  string venue = 1;
  string symbol = 2;
}

message Position {
  int64 net_qty = 1; // Positive = Long, Negative = Short, 0 = Flat
  int64 average_entry_price = 2;
  int64 unrealized_pnl = 3;
  int64 realized_pnl = 4;
}

message Bar {
  int64 open = 1;
  int64 high = 2;
  int64 low = 3;
  int64 close = 4;
  int64 volume = 5;
  int64 close_ns = 6;
  bool gap = 7;
}

message MarketFeatures {
  double rsi = 1; 
  double ema_fast = 2;
  double ema_slow = 3;
  int64 order_flow_imbalance = 4;
  repeated Bar bars = 5;          // closed, oldest first, up to 15
  Bar forming = 6;
  uint32 bars_since_update = 7;   // 0 when fresh
  bool volume_available = 8;
  bool book_stale = 9;
}

message GetStateResponse {
  uint64 sequence_id = 1;
  int64 timestamp_ns = 2;
  string venue = 3;
  string symbol = 4;
  int64 bid = 5;
  int64 ask = 6;
  Position current_position = 7;
  MarketFeatures features = 8;
  int64 available_equity = 9;
}

message SubmitIntentRequest {
  string intent_id = 1;         
  string agent_id = 2;          
  uint64 source_sequence_id = 3; 
  int64 generated_time_ns = 4;
  IntentType intent_type = 5;
  string venue = 6;
  string symbol = 7;
  OrderSide side = 8;
  TimeInForce time_in_force = 9;
  int64 target_price = 10;
  int64 stop_loss = 11; // Mandatory for PLACE intents
  int64 quantity = 12;
}

message SubmitIntentResponse {
  string intent_id = 1;
  RiskDecision decision = 2;
  RejectionCode rejection_code = 3;
  string rejection_reason = 4;
  uint64 executed_order_id = 5; // 0 if rejected
  int64 processed_time_ns = 6;
}

message StreamEventsRequest {
  string venue = 1;
  string symbol = 2;
}

message RegimeShiftPayload {
  string previous_regime = 1;
  string new_regime = 2;
  double confidence = 3;
}

// ---------------- Tape records (crates/api writes, bin/replay reads) ----------------
enum OrderState {
  ORDER_STATE_UNSPECIFIED = 0;
  ORDER_PENDING_NEW = 1;
  ORDER_OPEN = 2;
  ORDER_PARTIALLY_FILLED = 3;
  ORDER_FILLED = 4;
  ORDER_PENDING_CANCEL = 5;
  ORDER_CANCELED = 6;
  ORDER_REJECTED = 7;
}

message OrderTransition {
  uint64 order_id = 1;
  OrderState from = 2;
  OrderState to = 3;
  string reason = 4;
  int64 ns = 5;
}

message Fill {
  uint64 order_id = 1;
  OrderSide side = 2;
  int64 price = 3;
  int64 qty = 4;
  bool thin_book = 5;
  int64 ns = 6;
}

message PositionUpdate {
  Position position = 1;
  int64 equity = 2;
  int64 ns = 3;
}

message ExecEvent {
  oneof event {
    OrderTransition transition = 1;
    Fill fill = 2;
    PositionUpdate position = 3;
  }
}

message StateHash {
  uint64 sequence_id = 1;
  uint64 event_count = 2;
  bytes hash = 3;
}

message MarketEvent {
  uint64 sequence_id = 1;
  int64 timestamp_ns = 2;
  string venue = 3;
  string symbol = 4;
  oneof event {
    int64 mid_price_update = 5;
    Position position_update = 6;
    RegimeShiftPayload regime_shift = 7;
  }
}
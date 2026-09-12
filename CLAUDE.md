# AgentTrade

Rust market data and execution core with an LLM agent layer on top.
Agents think on a slow clock. The Rust core owns data, state, risk,
and execution. No LLM output reaches a venue without passing the
risk gate. Full design: docs/architecture.md. Read it first.

## Principles
- Core is deterministic on replay. Same tape, same state hashes.
  Any divergence is a bug, not a flake.
- Agents are advisory. Every intent is schema-validated and
  risk-checked in Rust. Rejections carry a RejectionCode.
- Everything is recorded before it is processed: raw venue
  messages, state diffs, intents, risk verdicts, agent turns.
- int64 fixed-point for price and quantity. Scales, tick and lot
  live on Instrument. No floats in state or execution. Features
  (rsi, ema, ofi) are double and advisory only.
- No allocation on the hot path after warmup. Prove it with benches.
- No unsafe without a comment explaining why and a test covering it.
- Paper venue by default. Testnet by flag. No live keys, ever.

## Layout
core/                       Rust workspace
  crates/types              Instrument, events, OrderState, Intent, RejectionCode
  crates/feed               venue WebSocket handlers, one module per venue
  crates/book               L2 builder from snapshot + deltas, checksum verified
  crates/config             config.toml schema, no live mode, fixed-point limits
  crates/features           OHLCV, EMA, RSI, OFI over book and trades
  crates/tape               append-only log, replay reader, state hashes
  crates/exec               order state machine, paper venue, venue adapters
  crates/risk               the gate: every check in architecture.md section 3
  crates/api                gRPC server: GetState, SubmitIntent, StreamEvents, ListInstruments
  bin/record                connect, record N seconds, exit
  bin/replay                --tape <file> --mode recorded|live
agents/                     Python, uv
  observer.py               core state -> text narrative
  analyst.py                narrative -> market read
  trader.py                 read + positions -> intents, JSON schema
  reviewer.py               veto pass
  brain.py                  Claude Agent SDK over the user's Claude Code sign-in
  onboard.py                connect account, write config
  runner.py                 one tick: state -> four agents -> intents
  journal/                  every turn, append-only
proto/agenttrade/v1/        the contract. Change here, regenerate both sides.
docs/                       architecture.md plus one note per crate
bench/                      criterion

## Commands
- cargo test --workspace
- cargo bench
- cargo clippy --workspace -- -D warnings && cargo fmt --check
- cd agents && uv run pytest
- cd agents && uv run agenttrade-agents onboard   # connect Claude sign-in, write config
- cd agents && uv run agenttrade-agents tick      # one swarm tick on demo state
- cargo run --bin record -- --venue kraken --symbol BTC/USD --seconds 600
- cargo run --bin replay -- --tape tapes/<file> --mode recorded

## Rules
- Design note before code for any new crate or agent. Show it, then build.
- Small PRs. One crate or one concern. Branch, PR, CI green, merge.
- Proto changes are their own PR.
- Agent prompts live in docs/prompts/ and are versioned like code.
- Latency and throughput numbers go in README with commit hash and machine.
- Do not add layers, languages, or dependencies not in this file without asking.
- Ask before choosing anything the spec leaves open. Do not guess.
- Any zero-failure metric is reported with its check count, and tests
  assert the count is positive. "0 checksum failures" means nothing
  without "over 4,488 updates".

## Dependencies
Every crate dependency is listed here with its reason. Adding one means
adding a line here first.
- prost, tonic, tonic-build: the gRPC contract in proto/ and its server
- tokio: async runtime for feeds and the api server
- tokio-tungstenite (rustls-tls-webpki-roots): venue WebSocket client
- futures-util: stream and sink combinators over the WebSocket
- rustls (ring): TLS crypto backend, tokio-tungstenite enables rustls without one
- serde, serde_json (arbitrary_precision): venue JSON without float parsing
- crc32fast: Kraken book checksum and per-frame tape crc
- clap: argument parsing for bin/record and bin/replay
- tracing, tracing-subscriber: structured logs
- thiserror: error enums
- criterion: benches
- proptest: property tests for book invariants
- toml: config file parsing in crates/config
- claude-agent-sdk (Python): the agent brain. Runs on the user's own
  Claude Code sign-in, so a subscription works without an API key.
  See docs/onboarding.md.

## Style
Short functions. Names over comments. Comments say why, not what.
No adjectives in docs that a benchmark hasn't earned.
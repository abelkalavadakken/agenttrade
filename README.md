# AgentTrade

Rust market data and execution core with an LLM agent layer on top.
Design: docs/architecture.md. One note per crate in docs/.

## Build

```
cd core
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check
cargo run --release --bin record -- --venue kraken --symbol BTC/USD --seconds 600 --out ../tapes
cargo run --release --bin replay -- --tape ../tapes/<file>.tape --mode recorded
```

## Benchmarks

Numbers come from `bin/replay` over a recorded tape. Each row names the
commit that produced it and the machine it ran on.

### Book apply, Kraken BTC/USD depth 10

Tape: 600 awake seconds recorded 2026-09-12, 4,491 book events
(3 snapshots, 4,488 deltas). Latency is `Instant` around `Book::apply`,
release build, median of 5 runs.

| commit | machine | events applied | crossed | checksum mismatches | p50 | p99 |
|---|---|---|---|---|---|---|
| 9eeff0e | Apple M2, 16 GB, macOS 26.6.2, rustc 1.98.1 | 4,491 | 0 | 0 | 416 ns | 625 ns |

p99 varied between 584 ns and 834 ns across the 5 runs. p50 did not move.

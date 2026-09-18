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
| 4b0a384 | Apple M2, 16 GB, macOS 26.6.2, rustc 1.98.1 | 15,600 | 0 | 0 | 708 ns | 792 ns |

Tape: 600 s recorded 2026-09-18, machine awake, no reconnects, 15,600 book
events (1 snapshot, 15,599 deltas) and 612 trades. p99 varied between 750 ns
and 833 ns across the 5 runs; p50 did not move. The earlier row on the
2026-09-12 tape read 416 ns p50 on a quieter book; the number moved with the
tape and the day, not the code.

### Paper venue demo, Kraken BTC/USD

`cargo run --release -p exec --example paper_demo -- tapes/<file>.tape` replays a
tape through feed, book, risk gate and paper venue with six scripted intents.

| commit | tape | intents | fills | realized P&L | note |
|---|---|---|---|---|---|
| 4b0a384 | 600 s, 2026-09-18, 15,600 book events | 3 approved, 3 rejected | 3 | +3.4 USD on 0.1 BTC round trip | paper venue, no fees, no queue position, optimistic |

The three rejections are MissingStop, PriceOutOfBand and ExceedsSingleLossLimit.

### Replay determinism

`bin/replay --mode recorded` drives the core over the tape, re-injects
recorded intents, and compares every StateHash record. The 2026-09-18 tape
was recorded before the core wrote hashes, so it verifies 0 hashes; the
first tape recorded by the entrypoint will carry them and this line will
report the count.

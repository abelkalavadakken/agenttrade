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

Tape: `tests/fixtures/acceptance.tape` (fetched by `tests/fixtures/fetch.sh`
from the v0.1.0-core release; the first 30 s are in git as
`acceptance-30s.tape`), 900 s recorded 2026-09-18 by
`bin/agenttrade`, 52,775 book events (1 snapshot, 52,774 deltas) and 1,716
trades. Latency is `Instant` around `Book::apply` in `bin/replay`, release
build.

| commit | machine | events applied | crossed | checksum mismatches | p50 | p99 |
|---|---|---|---|---|---|---|
| 582fea3 | Apple M2, 16 GB, macOS 26.6.2, rustc 1.98.1 | 52,775 | 0 | 0 | 416 ns | 584 ns |

### Paper venue demo, Kraken BTC/USD

`cargo run --release -p exec --example paper_demo -- tapes/<file>.tape` replays a
tape through feed, book, risk gate and paper venue with six scripted intents.

| commit | tape | intents | fills | realized P&L | note |
|---|---|---|---|---|---|
| 4b0a384 | 600 s, 2026-09-18, 15,600 book events | 3 approved, 3 rejected | 3 | +3.4 USD on 0.1 BTC round trip | paper venue, no fees, no queue position, optimistic |

The three rejections are MissingStop, PriceOutOfBand and ExceedsSingleLossLimit.

### Replay determinism and the acceptance tape

`bin/replay --mode recorded` drives the core over a tape, re-injects recorded
LLM intents, regenerates strategy intents, and compares every StateHash
record. `crates/api/tests/acceptance.rs` does the same over
`tests/fixtures/acceptance.tape` in CI and asserts the gate for unfreezing
agents:

| what | on the acceptance tape |
|---|---|
| hash records verified | 89, 0 mismatches, final hash identical |
| autonomous strategy `mr_ofi` | 19 orders sent, 44 fills |
| gated strategy `breakout` | 4 setups, 4 Wake events, 4 expired unconfirmed, 0 confirmed |
| gate | 30 approved, 1 rejected (MissingStop) |
| timer wakes | 15 |

The tape was recorded live with `--enable mr_ofi --enable breakout`,
mr_ofi tuned to fire (`ofi_threshold` 0.2 BTC, `min_spread_ticks` 1,
`hold_ns` 20 s, `cooldown_ns` 30 s) and breakout with `lookback_bars` 5,
`volume_mult_bps` 0 and `ttl_ns` 30 s, plus one rejected and one approved
discretionary PLACE and a FLATTEN_ALL sent over gRPC. Paper venue, no fees,
no queue position, optimistic.

# crates/book

Depth-N L2 book built from `BookEvent`. N is 10 for Kraken BTC/USD. Owned by
the core; feed and features read it, risk checks its flags.

## Data structure

```rust
pub struct Book {
    depth: usize,
    bids: Side,          // sorted descending by price
    asks: Side,          // sorted ascending by price
    checksum: u32,       // venue checksum from the last accepted event
    stale: bool,
    crossed: bool,
}

struct Side {
    levels: [Level; MAX_DEPTH],   // MAX_DEPTH = 25, Kraken's largest book depth
    len: usize,
}
```

Fixed arrays, no heap after construction, no allocation in `apply`. At N = 10
a linear scan beats any tree; insert and remove are a memmove of at most
nine levels. `Level` is the `types::Level { price: Price, qty: Qty }`
pair, both fixed-point int64.

## Applying events

**Snapshot** replaces both sides wholesale, truncates each to `depth`,
clears `stale`, and verifies the checksum.

**Delta** applies every level in message order, bids then asks. For each
level: qty 0 removes that price if present; otherwise, if the price exists
its qty is replaced; otherwise the level is inserted in sorted position. A
price can repeat inside one delta; last write wins. After all levels are
applied each side is truncated to `depth` (Kraken sends levels that push
the tail out, and the tail must drop for the checksum to match). Then
the checksum is verified.

Deltas that arrive while no snapshot has been accepted are refused with
`ApplyError::NoSnapshot`. Nothing changes.

`apply` returns `Result<(), ApplyError>` where

```rust
pub enum ApplyError { NoSnapshot, Checksum { expected: u32, computed: u32 }, Crossed }
```

On any error the book keeps its state but sets `stale`. The caller
resyncs. This mirrors what the feed tracker does today; once book lands
the tracker holds a `Book` instead of its own two `BTreeMap`s, so there
is one implementation of the delta rules and of the checksum.

## Checksum

Kraken v2: CRC32 over the top-N asks ascending, then top-N bids
descending. Each level contributes its price then its qty as decimal
strings with the point removed and leading zeros stripped. Our fixed-point
integers are exactly that string, so the input is the integer values
printed back to back. The book formats into a stack buffer of
`MAX_DEPTH * 2 * 2 * 20` bytes with `itoa`-style manual digit writing, no
`String`. `checksum()` recomputes from the current levels; the venue's
value from the last event is kept alongside for the mismatch report.

Verified against 78 live frames in Python before the Rust version, and
against every frame in `tests/fixtures/` in the feed tests.

## Crossed

A book is crossed when `best_bid >= best_ask`. A correct venue never
publishes one; Kraken deltas are atomic per message, so we check only
after the whole delta is applied, not between levels. A crossed book after
a full apply means we lost a message or the venue misbehaved. We set
`crossed` and `stale`, return `ApplyError::Crossed`, and the caller
resyncs exactly as for a checksum failure. Replay counts these and the
count must be 0 on a clean tape.

## Stale

`stale` means "do not trust the prices". It is set by

- construction, until the first snapshot is accepted
- any `ApplyError`
- `mark_stale()`, which the feed calls on a `Silent` or `Disconnected`
  resync

and cleared only by a snapshot whose checksum verifies. `best_bid`,
`best_ask` and `mid` still return values while stale so that observers can
show the last known state; risk reads `is_stale()` and rejects with
`STALE_STATE` or `VENUE_DISCONNECTED`.

## Reads

```rust
fn best_bid(&self) -> Option<Level>
fn best_ask(&self) -> Option<Level>
fn mid(&self) -> Option<Price>         // (bid + ask) / 2 rounded down
fn spread(&self) -> Option<Price>
fn bids(&self) -> &[Level]             // depth-limited, best first
fn asks(&self) -> &[Level]
fn checksum(&self) -> u32
fn is_stale(&self) -> bool
fn is_crossed(&self) -> bool
```

`mid` rounds down to the tick grid, so a half-tick mid loses half a tick.
It is advisory. Its only consumer in the core is the risk gate's
out-of-band check, which tolerates a tick. Anything that needs the exact
midpoint takes `best_bid` and `best_ask` and works in doubled units.

## Tests

- Unit: snapshot then a handful of hand-built deltas, insert, replace,
  remove, tail truncation, repeated price in one delta, crossed detection,
  stale transitions.
- Fixture: every frame in `tests/fixtures/kraken_book_btcusd.jsonl`
  applies without error.
- proptest: from a random valid snapshot, apply random valid deltas
  (random prices near the book, random qty including 0) with the venue
  checksum computed by a reference `BTreeMap` model. Invariants after every
  step: never crossed, each side `len <= depth`, sides strictly sorted,
  `book.checksum() == model checksum`, and `apply` returned `Ok`.
- Replay: `bin/replay` gains updates applied, crossed count, checksum
  mismatches and p50/p99 apply latency measured with `Instant` around each
  `apply`.

## Decisions

- Fixed array bound of 25 with runtime depth. Const generic only if a
  benchmark asks for it.
- `mid` rounds down on a half-tick. Advisory, see Reads.
- proptest is a dev-dependency, listed in CLAUDE.md.
- The feed tracker keeps its own book for now. Switching it to `Book` is a
  follow-up PR; book/initial ships the crate and its tests only.

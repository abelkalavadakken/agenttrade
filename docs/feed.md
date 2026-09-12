# crates/feed

One module per venue. Each venue splits into a pure tracker and a socket client.

## kraken

WebSocket v2, `wss://ws.kraken.com/v2`, book channel, depth 10.

**Tracker** (`kraken::Tracker`) is bytes in, events out, no clock, no socket.
It parses frames, keeps the top-N book per side, applies deltas in order
(the same price can repeat in one update), truncates to depth, and verifies
the venue checksum after every snapshot and update. Numbers are parsed from
their JSON text straight to fixed-point; no float is ever built.

Checksum: CRC32 over the top-N asks ascending then top-N bids descending,
each level as price then qty with the decimal point and leading zeros
removed. Because our fixed-point integers are that string already, the
checksum input is the integer values printed back to back. Verified against
78 live frames before it was written in Rust.

**Resync rules.** Each emits `FeedEvent::Resync(reason)`.

| Reason | Trigger | Then |
|---|---|---|
| ChecksumMismatch | computed crc differs from venue crc | drop book, unsubscribe, resubscribe |
| UnsolicitedSnapshot | a snapshot arrives we did not ask for | accept it as the new book |
| Silent | socket open, heartbeats flowing, no book message for 10 s | unsubscribe, resubscribe |
| Disconnected | socket closed, error, or no frame at all for 10 s | reconnect with backoff |

Kraken v2 book frames carry no sequence number, so "gap" means an
unsolicited snapshot. Updates that arrive while a snapshot is pending are
ignored and counted.

**Client** (`kraken::run`) owns the socket. Reconnect backoff starts at 1 s,
doubles, caps at 30 s. Every raw frame goes downstream before it is parsed
so the tape sees exactly what the venue sent. Lifecycle messages
(`connected`, `disconnected <err>`, `resubscribe`) travel as
`FeedMsg::Control` and the recorder writes them under their own source id.

## Tokio timers and system sleep

Tokio timers run on the monotonic clock, which macOS pauses while the machine
sleeps. A `--seconds 600` recording on a laptop that sleeps for 35 minutes
runs 600 awake seconds and spans 35 minutes of wall clock. Each wake-up looks
like a dead socket (no frames for 10 s) and triggers a reconnect. Tape
timestamps are wall clock, so replay reports the true span. Keep the machine
awake for a clean tape.

## Fixtures

`tests/fixtures/kraken_book_btcusd.jsonl` is 8 s of live frames captured
2026-09-12. Tests parse every line, verify every checksum, corrupt one
checksum, replay a snapshot mid-stream as a synthetic gap, and feed garbage.

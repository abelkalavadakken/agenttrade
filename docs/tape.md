# crates/tape

Append-only binary log of raw frames. Length-prefixed, crc per frame.

```text
header:  "ATTP" | u16 version | u16 source_count | (u16 len, utf8 name)*
frame:   u32 payload_len | payload | u32 crc32(payload)
payload: i64 recv_ns | u16 source_id | bytes
```

All integers little-endian. `source_id` indexes the header table.
bin/record writes `0 = kraken.ws` (raw venue frames) and `1 = record.ctl`
(connect, disconnect, resubscribe markers).

**Reader** iterates records. `Mode::Fast` reads at disk speed. `Mode::Paced`
sleeps so records emerge with their recorded inter-arrival gaps. A crc
mismatch is an error carrying the frame index. A frame cut short by a crash
is `TruncatedTail`, never silently dropped.

**Writer** buffers through `BufWriter`; call `flush` or `into_inner` before
exit.

## bin/record

`--venue kraken --symbol BTC/USD --seconds N --out tapes/`. File name is
`<venue>_<symbol>_<unix seconds>.tape`. Prints size, record count and
event counts on exit. Exits non-zero if the feed client stops early.

## bin/replay

`--tape <file> --mode recorded|live`. Runs the Kraken tracker over the raw
frames and prints record count, duration, gap count, checksum failures and
control markers. `recorded` reads fast, `live` paces at recorded timing.
Later sessions add state hashes to recorded mode.

Tapes are gitignored. Record your own with `cargo run --bin record`.

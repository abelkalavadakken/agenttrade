# crates/types

Fixed-point int64 for price and quantity. `Price` and `Qty` carry no scale;
the scale, tick and lot live on `Instrument`.

## Proto mapping

Rust field names follow CLAUDE.md. crates/api maps them to the wire names.

| Rust `Instrument` | proto `Instrument` |
|---|---|
| `price_scale: u32` | `price_scale: int32` |
| `qty_scale: u32` | `qty_scale: int32` |
| `tick: Price` | `min_tick_size: int64` |
| `lot: Qty` | `min_lot_size: int64` |

`Intent` is the validated internal type, not a mirror of `SubmitIntentRequest`.
crates/api converts the request into `IntentEnvelope { .., intent: Intent }` and
rejects anything unspecified or malformed with `RejectionCode::InvalidIntent`
(`INVALID_INTENT = 10` on the wire).

Enum discriminants for `Side`, `TimeInForce` and `RejectionCode` match the proto
numbering.

## Order lifecycle

`PendingNew -> Open -> PartiallyFilled -> Filled`, with `PendingCancel -> Canceled`
reachable from `Open` and `PartiallyFilled`. `Rejected` is terminal from
`PendingNew`. IOC and FOK orders that do not fill end in `Canceled` with a reason
on the order.

## Instruments

`instruments::kraken()` hardcodes Kraken BTC/USD (price scale 1, qty scale 8).
A config file replaces this later.

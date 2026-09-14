//! Shared core types. Fixed-point int64 everywhere; scales live on Instrument.

mod instrument;
pub mod instruments;
mod scalar;

pub use instrument::Instrument;
pub use scalar::{Price, Qty};

/// Order side. Numbering matches proto OrderSide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Buy = 1,
    Sell = 2,
}

/// One price level. `qty == 0` in a delta means remove the level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level {
    pub price: Price,
    pub qty: Qty,
}

/// L2 book event as emitted by a venue feed, already parsed to fixed-point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookEvent {
    Snapshot {
        bids: Vec<Level>,
        asks: Vec<Level>,
        checksum: u32,
    },
    Delta {
        bids: Vec<Level>,
        asks: Vec<Level>,
        checksum: u32,
        venue_time_ns: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeEvent {
    pub side: Side,
    pub price: Price,
    pub qty: Qty,
    pub venue_time_ns: i64,
    pub trade_id: u64,
}

/// Why a feed dropped and rebuilt its book.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResyncReason {
    ChecksumMismatch,
    /// Checksum verified but best bid met or crossed best ask.
    Crossed,
    /// A snapshot arrived that we did not ask for. Kraken v2 has no sequence numbers.
    UnsolicitedSnapshot,
    /// Socket open and heartbeats flowing, but no book message for the silence window.
    Silent,
    Disconnected,
}

/// Everything a venue feed hands downstream, keyed by receive time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedEvent {
    Book(BookEvent),
    Trade(TradeEvent),
    Resync(ResyncReason),
}

/// Order lifecycle. Owned by crates/exec; defined here so risk and api share it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderState {
    PendingNew,
    Open,
    PartiallyFilled,
    Filled,
    PendingCancel,
    /// IOC and FOK orders that do not fill end here, with a reason on the order.
    Canceled,
    Rejected,
}

impl OrderState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Filled | Self::Canceled | Self::Rejected)
    }
}

/// Numbering matches proto TimeInForce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimeInForce {
    Gtc = 1,
    Ioc = 2,
    Fok = 3,
}

/// A validated intent. crates/api builds this from SubmitIntentRequest and
/// rejects anything unspecified or malformed with RejectionCode::InvalidIntent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    Place {
        side: Side,
        price: Price,
        stop: Price,
        qty: Qty,
        tif: TimeInForce,
    },
    Cancel {
        order_id: u64,
    },
    Flatten,
    Noop,
}

/// Who sent an intent and against which state. Travels alongside Intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentEnvelope {
    pub intent_id: String,
    pub agent_id: String,
    pub source_sequence_id: u64,
    pub generated_time_ns: i64,
    pub venue: String,
    pub symbol: String,
    pub intent: Intent,
}

/// Numbering matches proto RejectionCode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RejectionCode {
    None = 0,
    StaleState = 1,
    ExceedsMaxLeverage = 2,
    ExceedsSingleLossLimit = 3,
    InvalidTickSize = 4,
    VenueDisconnected = 5,
    KillSwitchActive = 6,
    RateLimitExceeded = 7,
    MissingStop = 8,
    PriceOutOfBand = 9,
    InvalidIntent = 10,
    InstrumentHalted = 11,
}

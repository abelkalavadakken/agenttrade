//! Venue WebSocket handlers. One module per venue.
//!
//! Each venue splits into a pure tracker (bytes in, events out, no IO) and a
//! client that owns the socket. Replay runs the tracker over a tape; record
//! runs the client and writes every raw frame before handing it to the tracker.

pub mod kraken;
pub mod time;

use types::FeedEvent;

/// What a feed client hands downstream, in arrival order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedMsg {
    /// A raw venue frame, exactly as received.
    Raw { recv_ns: i64, bytes: Vec<u8> },
    /// A parsed event derived from the preceding raw frame or from client state.
    Event { recv_ns: i64, event: FeedEvent },
    /// Client lifecycle: connect, disconnect, resubscribe. Text is for the tape.
    Control { recv_ns: i64, text: String },
}

pub fn now_ns() -> i64 {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before unix epoch");
    d.as_nanos() as i64
}

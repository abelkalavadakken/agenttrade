//! Kraken WebSocket v2, book channel.

mod client;
mod tracker;
mod wire;

pub use client::{run, Config};
pub use tracker::{Handled, MsgKind, Tracker, TrackerStats};

pub const WS_URL: &str = "wss://ws.kraken.com/v2";

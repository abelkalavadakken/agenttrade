//! Serde shapes for Kraken v2 frames. Numbers stay as text so no float is ever parsed.

use serde::Deserialize;
use serde_json::Number;

#[derive(Debug, Deserialize)]
pub struct Frame<'a> {
    #[serde(borrow)]
    pub channel: Option<&'a str>,
    #[serde(borrow)]
    pub method: Option<&'a str>,
    pub success: Option<bool>,
    #[serde(borrow)]
    pub error: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
pub struct BookFrame<'a> {
    #[serde(borrow, rename = "type")]
    pub kind: &'a str,
    #[serde(borrow)]
    pub data: Vec<BookData<'a>>,
}

#[derive(Debug, Deserialize)]
pub struct BookData<'a> {
    #[serde(borrow)]
    pub symbol: &'a str,
    pub bids: Vec<WireLevel>,
    pub asks: Vec<WireLevel>,
    pub checksum: u32,
    #[serde(borrow)]
    pub timestamp: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
pub struct WireLevel {
    pub price: Number,
    pub qty: Number,
}

pub fn subscribe(symbol: &str, depth: u32) -> String {
    serde_json::json!({
        "method": "subscribe",
        "params": { "channel": "book", "symbol": [symbol], "depth": depth }
    })
    .to_string()
}

pub fn unsubscribe(symbol: &str, depth: u32) -> String {
    serde_json::json!({
        "method": "unsubscribe",
        "params": { "channel": "book", "symbol": [symbol], "depth": depth }
    })
    .to_string()
}

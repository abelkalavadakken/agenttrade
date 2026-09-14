//! Pure Kraken book tracker. Bytes in, events out. No clock, no socket.
//! The book itself lives in crates/book; this module owns the venue protocol.

use book::{ApplyError, Book};
use types::{BookEvent, FeedEvent, Instrument, Level, ResyncReason, Side, TradeEvent};

use super::wire::{BookFrame, Frame, TradeFrame, WireLevel};
use crate::time::parse_rfc3339_ns;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgKind {
    Snapshot,
    Update,
    Trade,
    Heartbeat,
    Status,
    SubscribeAck,
    UnsubscribeAck,
    Error,
    Other,
    Unparsed,
}

/// Result of feeding one frame.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Handled {
    pub kind: Option<MsgKind>,
    pub events: Vec<FeedEvent>,
    /// The client must unsubscribe and resubscribe before the book is valid again.
    pub resubscribe: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TrackerStats {
    pub snapshots: u64,
    pub updates: u64,
    pub trades: u64,
    pub heartbeats: u64,
    pub checksum_failures: u64,
    pub crossed: u64,
    pub unsolicited_snapshots: u64,
    pub unparsed: u64,
    pub ignored_updates: u64,
}

pub struct Tracker {
    instrument: Instrument,
    book: Book,
    /// True between (re)subscribe and the snapshot that answers it.
    expect_snapshot: bool,
    stats: TrackerStats,
}

impl Tracker {
    pub fn new(instrument: Instrument, depth: usize) -> Self {
        Self {
            instrument,
            book: Book::new(depth),
            expect_snapshot: true,
            stats: TrackerStats::default(),
        }
    }

    pub fn stats(&self) -> TrackerStats {
        self.stats
    }

    pub fn book(&self) -> &Book {
        &self.book
    }

    pub fn has_book(&self) -> bool {
        !self.expect_snapshot
    }

    /// Call after sending a subscribe so the next snapshot is not counted as unsolicited.
    pub fn expect_snapshot(&mut self) {
        self.expect_snapshot = true;
        self.book.mark_stale();
    }

    /// Drop the book for a reason decided outside the tracker (silence, disconnect).
    pub fn force_resync(&mut self, reason: ResyncReason) -> Handled {
        self.expect_snapshot();
        Handled {
            kind: None,
            events: vec![FeedEvent::Resync(reason)],
            resubscribe: reason != ResyncReason::Disconnected,
        }
    }

    pub fn on_frame(&mut self, bytes: &[u8]) -> Handled {
        let Ok(text) = std::str::from_utf8(bytes) else {
            return self.unparsed();
        };
        let Ok(frame) = serde_json::from_str::<Frame>(text) else {
            return self.unparsed();
        };
        match (frame.channel, frame.method, frame.success) {
            (Some("book"), _, _) => self.on_book(text),
            (Some("trade"), _, _) => self.on_trade(text),
            (Some("heartbeat"), _, _) => {
                self.stats.heartbeats += 1;
                Handled::kind(MsgKind::Heartbeat)
            }
            (Some("status"), _, _) => Handled::kind(MsgKind::Status),
            (_, Some("subscribe"), Some(true)) => Handled::kind(MsgKind::SubscribeAck),
            (_, Some("unsubscribe"), Some(true)) => Handled::kind(MsgKind::UnsubscribeAck),
            (_, Some(_), Some(false)) => Handled::kind(MsgKind::Error),
            _ if frame.error.is_some() => Handled::kind(MsgKind::Error),
            _ => Handled::kind(MsgKind::Other),
        }
    }

    fn unparsed(&mut self) -> Handled {
        self.stats.unparsed += 1;
        Handled::kind(MsgKind::Unparsed)
    }

    fn on_book(&mut self, text: &str) -> Handled {
        let Ok(frame) = serde_json::from_str::<BookFrame>(text) else {
            return self.unparsed();
        };
        let mut out = Handled::default();
        for data in &frame.data {
            if data.symbol != self.instrument.symbol {
                continue;
            }
            let (Some(bids), Some(asks)) = (self.levels(&data.bids), self.levels(&data.asks))
            else {
                return self.unparsed();
            };
            let event = match frame.kind {
                "snapshot" => {
                    out.kind = Some(MsgKind::Snapshot);
                    self.stats.snapshots += 1;
                    if !self.expect_snapshot {
                        self.stats.unsolicited_snapshots += 1;
                        out.events
                            .push(FeedEvent::Resync(ResyncReason::UnsolicitedSnapshot));
                    }
                    self.expect_snapshot = false;
                    BookEvent::Snapshot {
                        bids,
                        asks,
                        checksum: data.checksum,
                    }
                }
                "update" => {
                    out.kind = Some(MsgKind::Update);
                    self.stats.updates += 1;
                    if self.expect_snapshot {
                        self.stats.ignored_updates += 1;
                        continue;
                    }
                    BookEvent::Delta {
                        bids,
                        asks,
                        checksum: data.checksum,
                        venue_time_ns: data.timestamp.and_then(parse_rfc3339_ns).unwrap_or(0),
                    }
                }
                _ => return self.unparsed(),
            };
            match self.book.apply(&event) {
                Ok(()) => out.events.push(FeedEvent::Book(event)),
                Err(e) => {
                    self.fail(e, &mut out);
                    break;
                }
            }
        }
        out
    }

    /// Trades carry no sequence and need no book; each frame is one or more events.
    fn on_trade(&mut self, text: &str) -> Handled {
        let Ok(frame) = serde_json::from_str::<TradeFrame>(text) else {
            return self.unparsed();
        };
        let mut out = Handled::kind(MsgKind::Trade);
        for t in &frame.data {
            if t.symbol != self.instrument.symbol {
                continue;
            }
            let side = match t.side {
                "buy" => Side::Buy,
                "sell" => Side::Sell,
                _ => return self.unparsed(),
            };
            let (Some(price), Some(qty), Some(venue_time_ns)) = (
                self.instrument.parse_price(t.price.as_str()),
                self.instrument.parse_qty(t.qty.as_str()),
                parse_rfc3339_ns(t.timestamp),
            ) else {
                return self.unparsed();
            };
            self.stats.trades += 1;
            out.events.push(FeedEvent::Trade(TradeEvent {
                side,
                price,
                qty,
                venue_time_ns,
                trade_id: t.trade_id,
            }));
        }
        out
    }

    fn fail(&mut self, e: ApplyError, out: &mut Handled) {
        let reason = match e {
            ApplyError::Crossed => {
                self.stats.crossed += 1;
                ResyncReason::Crossed
            }
            ApplyError::Checksum { .. } | ApplyError::NoSnapshot => {
                self.stats.checksum_failures += 1;
                ResyncReason::ChecksumMismatch
            }
        };
        self.expect_snapshot();
        out.events.push(FeedEvent::Resync(reason));
        out.resubscribe = true;
    }

    fn levels(&self, wire: &[WireLevel]) -> Option<Vec<Level>> {
        wire.iter()
            .map(|w| {
                Some(Level {
                    price: self.instrument.parse_price(w.price.as_str())?,
                    qty: self.instrument.parse_qty(w.qty.as_str())?,
                })
            })
            .collect()
    }
}

impl Handled {
    fn kind(kind: MsgKind) -> Self {
        Self {
            kind: Some(kind),
            ..Default::default()
        }
    }
}

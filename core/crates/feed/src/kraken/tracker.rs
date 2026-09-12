//! Pure Kraken book tracker. Bytes in, events out. No clock, no socket.

use std::collections::BTreeMap;

use types::{BookEvent, FeedEvent, Instrument, Level, Qty, ResyncReason};

use super::checksum::book_checksum;
use super::wire::{BookFrame, Frame, WireLevel};
use crate::time::parse_rfc3339_ns;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgKind {
    Snapshot,
    Update,
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
    pub heartbeats: u64,
    pub checksum_failures: u64,
    pub gaps: u64,
    pub unparsed: u64,
    pub ignored_updates: u64,
}

pub struct Tracker {
    instrument: Instrument,
    depth: usize,
    bids: BTreeMap<i64, i64>,
    asks: BTreeMap<i64, i64>,
    /// True between (re)subscribe and the snapshot that answers it.
    expect_snapshot: bool,
    stats: TrackerStats,
}

impl Tracker {
    pub fn new(instrument: Instrument, depth: usize) -> Self {
        Self {
            instrument,
            depth,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            expect_snapshot: true,
            stats: TrackerStats::default(),
        }
    }

    pub fn stats(&self) -> TrackerStats {
        self.stats
    }

    pub fn has_book(&self) -> bool {
        !self.expect_snapshot
    }

    /// Call after sending a subscribe so the next snapshot is not counted as a gap.
    pub fn expect_snapshot(&mut self) {
        self.expect_snapshot = true;
        self.bids.clear();
        self.asks.clear();
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
            match frame.kind {
                "snapshot" => self.apply_snapshot(bids, asks, data.checksum, &mut out),
                "update" => {
                    let ts = data.timestamp.and_then(parse_rfc3339_ns).unwrap_or(0);
                    self.apply_update(bids, asks, data.checksum, ts, &mut out);
                }
                _ => return self.unparsed(),
            }
            if out.resubscribe {
                break;
            }
        }
        out
    }

    fn apply_snapshot(
        &mut self,
        bids: Vec<Level>,
        asks: Vec<Level>,
        checksum: u32,
        out: &mut Handled,
    ) {
        out.kind = Some(MsgKind::Snapshot);
        self.stats.snapshots += 1;
        if !self.expect_snapshot {
            self.stats.gaps += 1;
            out.events
                .push(FeedEvent::Resync(ResyncReason::UnsolicitedSnapshot));
        }
        self.bids = bids.iter().map(|l| (l.price.0, l.qty.0)).collect();
        self.asks = asks.iter().map(|l| (l.price.0, l.qty.0)).collect();
        self.expect_snapshot = false;
        if !self.verify(checksum, out) {
            return;
        }
        out.events.push(FeedEvent::Book(BookEvent::Snapshot {
            bids,
            asks,
            checksum,
        }));
    }

    fn apply_update(
        &mut self,
        bids: Vec<Level>,
        asks: Vec<Level>,
        checksum: u32,
        venue_time_ns: i64,
        out: &mut Handled,
    ) {
        out.kind = Some(MsgKind::Update);
        self.stats.updates += 1;
        if self.expect_snapshot {
            self.stats.ignored_updates += 1;
            return;
        }
        for l in &bids {
            apply_level(&mut self.bids, l);
        }
        for l in &asks {
            apply_level(&mut self.asks, l);
        }
        truncate_low(&mut self.bids, self.depth);
        truncate_high(&mut self.asks, self.depth);
        if !self.verify(checksum, out) {
            return;
        }
        out.events.push(FeedEvent::Book(BookEvent::Delta {
            bids,
            asks,
            checksum,
            venue_time_ns,
        }));
    }

    fn verify(&mut self, expected: u32, out: &mut Handled) -> bool {
        if book_checksum(&self.asks, &self.bids, self.depth) == expected {
            return true;
        }
        self.stats.checksum_failures += 1;
        self.expect_snapshot();
        out.events
            .push(FeedEvent::Resync(ResyncReason::ChecksumMismatch));
        out.resubscribe = true;
        false
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

fn apply_level(side: &mut BTreeMap<i64, i64>, l: &Level) {
    if l.qty == Qty::ZERO {
        side.remove(&l.price.0);
    } else {
        side.insert(l.price.0, l.qty.0);
    }
}

/// Bids: keep the highest `depth` prices.
fn truncate_low(side: &mut BTreeMap<i64, i64>, depth: usize) {
    while side.len() > depth {
        side.pop_first();
    }
}

/// Asks: keep the lowest `depth` prices.
fn truncate_high(side: &mut BTreeMap<i64, i64>, depth: usize) {
    while side.len() > depth {
        side.pop_last();
    }
}

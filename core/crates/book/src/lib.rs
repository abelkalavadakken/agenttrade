//! Depth-N L2 book from BookEvent. Fixed arrays, no heap in apply. See docs/book.md.

mod checksum;
mod side;

use types::{BookEvent, Level, Price, Qty};

pub use checksum::MAX_DEPTH;
use side::Side;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ApplyError {
    #[error("delta before snapshot")]
    NoSnapshot,
    #[error("checksum: venue {expected:#010x}, computed {computed:#010x}")]
    Checksum { expected: u32, computed: u32 },
    #[error("book crossed")]
    Crossed,
}

#[derive(Debug, Clone)]
pub struct Book {
    depth: usize,
    bids: Side,
    asks: Side,
    venue_checksum: u32,
    stale: bool,
    crossed: bool,
}

impl Book {
    /// `depth` is clamped to MAX_DEPTH.
    pub fn new(depth: usize) -> Self {
        Self {
            depth: depth.min(MAX_DEPTH),
            bids: Side::new(true),
            asks: Side::new(false),
            venue_checksum: 0,
            stale: true,
            crossed: false,
        }
    }

    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Applies one event. On error the book keeps its levels and turns stale.
    pub fn apply(&mut self, event: &BookEvent) -> Result<(), ApplyError> {
        let result = match event {
            BookEvent::Snapshot {
                bids,
                asks,
                checksum,
            } => self.apply_snapshot(bids, asks, *checksum),
            BookEvent::Delta {
                bids,
                asks,
                checksum,
                ..
            } => self.apply_delta(bids, asks, *checksum),
        };
        if result.is_err() {
            self.stale = true;
        }
        result
    }

    fn apply_snapshot(
        &mut self,
        bids: &[Level],
        asks: &[Level],
        checksum: u32,
    ) -> Result<(), ApplyError> {
        self.bids.clear();
        self.asks.clear();
        for l in bids {
            self.bids.set(l);
        }
        for l in asks {
            self.asks.set(l);
        }
        self.finish(checksum)?;
        self.stale = false;
        Ok(())
    }

    fn apply_delta(
        &mut self,
        bids: &[Level],
        asks: &[Level],
        checksum: u32,
    ) -> Result<(), ApplyError> {
        if self.stale && self.bids.is_empty() && self.asks.is_empty() {
            return Err(ApplyError::NoSnapshot);
        }
        for l in bids {
            self.bids.set(l);
        }
        for l in asks {
            self.asks.set(l);
        }
        self.finish(checksum)
    }

    fn finish(&mut self, checksum: u32) -> Result<(), ApplyError> {
        self.bids.truncate(self.depth);
        self.asks.truncate(self.depth);
        self.venue_checksum = checksum;
        let computed = self.checksum();
        if computed != checksum {
            return Err(ApplyError::Checksum {
                expected: checksum,
                computed,
            });
        }
        self.crossed = matches!(
            (self.best_bid(), self.best_ask()),
            (Some(b), Some(a)) if b.price >= a.price
        );
        if self.crossed {
            return Err(ApplyError::Crossed);
        }
        Ok(())
    }

    /// The feed calls this on a Silent or Disconnected resync.
    pub fn mark_stale(&mut self) {
        self.stale = true;
    }

    pub fn is_stale(&self) -> bool {
        self.stale
    }

    pub fn is_crossed(&self) -> bool {
        self.crossed
    }

    /// Best first.
    pub fn bids(&self) -> &[Level] {
        self.bids.as_slice()
    }

    /// Best first.
    pub fn asks(&self) -> &[Level] {
        self.asks.as_slice()
    }

    pub fn best_bid(&self) -> Option<Level> {
        self.bids.as_slice().first().copied()
    }

    pub fn best_ask(&self) -> Option<Level> {
        self.asks.as_slice().first().copied()
    }

    /// (bid + ask) / 2 rounded down. Advisory; see docs/book.md.
    pub fn mid(&self) -> Option<Price> {
        let (b, a) = (self.best_bid()?, self.best_ask()?);
        Some(Price((b.price.0 + a.price.0).div_euclid(2)))
    }

    pub fn spread(&self) -> Option<Price> {
        let (b, a) = (self.best_bid()?, self.best_ask()?);
        Some(a.price - b.price)
    }

    /// Kraken checksum over the current levels.
    pub fn checksum(&self) -> u32 {
        checksum::compute(self.asks.as_slice(), self.bids.as_slice(), self.depth)
    }

    /// Checksum the venue sent with the last applied event.
    pub fn venue_checksum(&self) -> u32 {
        self.venue_checksum
    }

    pub fn total_qty(&self) -> (Qty, Qty) {
        let sum = |s: &[Level]| Qty(s.iter().map(|l| l.qty.0).sum());
        (sum(self.bids()), sum(self.asks()))
    }
}

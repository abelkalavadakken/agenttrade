//! One-minute OHLCV bars on tape time.

use types::{Price, Qty};

pub const BAR_NS: i64 = 60_000_000_000;
/// Closed bars kept. 512 is the Kronos lookback; the observer reads 15.
pub const RING: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarSource {
    /// OHLC from trade prices, volume from trade qty.
    Trades,
    /// OHLC from mid, volume absent. The degraded book-only form.
    Mid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Bar {
    pub open: Price,
    pub high: Price,
    pub low: Price,
    pub close: Price,
    pub volume: Qty,
    /// Start of the bar, minute aligned.
    pub open_ns: i64,
    /// End of the bar, exclusive. `open_ns + BAR_NS`.
    pub close_ns: i64,
    /// No sample entered, or a sample was refused because the book was stale.
    pub gap: bool,
    pub samples: u32,
}

impl Bar {
    fn empty(open_ns: i64) -> Self {
        Self {
            open_ns,
            close_ns: open_ns + BAR_NS,
            gap: true,
            ..Default::default()
        }
    }
}

pub struct Bars {
    closed: Vec<Bar>,
    /// Index of the oldest closed bar once the ring wraps.
    head: usize,
    forming: Option<Bar>,
    scratch: Vec<Bar>,
}

impl Bars {
    pub fn new() -> Self {
        Self {
            closed: Vec::with_capacity(RING),
            head: 0,
            forming: None,
            scratch: Vec::with_capacity(RING),
        }
    }

    /// Returns the next closed bar whose boundary is at or before `now_ns`,
    /// or `None`. Loop until `None` to catch up over a quiet stretch.
    pub fn advance(&mut self, now_ns: i64) -> Option<Bar> {
        let forming = self
            .forming
            .get_or_insert_with(|| Bar::empty(floor_minute(now_ns)));
        if now_ns < forming.close_ns {
            return None;
        }
        let done = *forming;
        // A clock leap past the whole ring (machine asleep for more than
        // 512 minutes, or a test mixing clocks) is not walked minute by
        // minute: the forming bar jumps to the current minute and the
        // missing bars are simply absent. `closed()` shows the gap by time.
        let next_open = if now_ns - done.close_ns >= RING as i64 * BAR_NS {
            floor_minute(now_ns)
        } else {
            done.close_ns
        };
        let mut next = Bar::empty(next_open);
        if !done.gap {
            // Carry the close forward so a gap bar still shows the last price.
            next.open = done.close;
            next.high = done.close;
            next.low = done.close;
            next.close = done.close;
        }
        self.forming = Some(next);
        self.push(done);
        Some(done)
    }

    pub fn sample(&mut self, price: Price, qty: Option<Qty>, now_ns: i64) {
        let bar = self
            .forming
            .get_or_insert_with(|| Bar::empty(floor_minute(now_ns)));
        debug_assert!(now_ns < bar.close_ns, "advance before sample");
        if bar.samples == 0 {
            bar.open = price;
            bar.high = price;
            bar.low = price;
            bar.gap = false;
        }
        bar.high = bar.high.max(price);
        bar.low = bar.low.min(price);
        bar.close = price;
        if let Some(q) = qty {
            bar.volume = bar.volume + q;
        }
        bar.samples += 1;
    }

    /// A sample was refused because the book was stale; the bar is incomplete.
    pub fn mark_gap(&mut self) {
        if let Some(bar) = self.forming.as_mut() {
            bar.gap = true;
        }
    }

    pub fn forming(&self) -> Bar {
        self.forming.unwrap_or_default()
    }

    /// Oldest first, up to RING bars. Reads reorder into scratch only when
    /// the ring has wrapped; before that it is the vector itself.
    pub fn closed(&self) -> &[Bar] {
        if self.head == 0 {
            &self.closed
        } else {
            &self.scratch
        }
    }

    fn push(&mut self, bar: Bar) {
        if self.closed.len() < RING {
            self.closed.push(bar);
            return;
        }
        self.closed[self.head] = bar;
        self.head = (self.head + 1) % RING;
        self.scratch.clear();
        self.scratch.extend_from_slice(&self.closed[self.head..]);
        self.scratch.extend_from_slice(&self.closed[..self.head]);
    }
}

impl Default for Bars {
    fn default() -> Self {
        Self::new()
    }
}

fn floor_minute(ns: i64) -> i64 {
    ns.div_euclid(BAR_NS) * BAR_NS
}

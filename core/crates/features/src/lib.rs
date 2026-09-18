//! OHLCV bars, EMA, RSI and OFI. Advisory f64 and fixed-point sums; a pure
//! function of tape events in tape order. See docs/features.md.

mod bars;
mod ofi;
mod smooth;

pub use bars::{Bar, BarSource, Bars, BAR_NS, RING};
pub use ofi::Ofi;
pub use smooth::{Ema, Rsi};

use book::Book;
use types::{Price, TradeEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeaturesConfig {
    pub bar_source: BarSource,
    pub ema_fast: u32,
    pub ema_slow: u32,
    pub rsi_period: u32,
    pub ofi_window_ns: i64,
}

impl Default for FeaturesConfig {
    fn default() -> Self {
        Self {
            bar_source: BarSource::Trades,
            ema_fast: 12,
            ema_slow: 26,
            rsi_period: 14,
            ofi_window_ns: 60_000_000_000,
        }
    }
}

/// What the api hands to agents. Bars are fixed-point; smoothed values are f64.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot<'a> {
    pub rsi: Option<f64>,
    pub ema_fast: Option<f64>,
    pub ema_slow: Option<f64>,
    pub order_flow_imbalance: i64,
    pub bars: &'a [Bar],
    pub forming: Bar,
    pub bars_since_update: u32,
    pub volume_available: bool,
    pub book_stale: bool,
}

pub struct Features {
    cfg: FeaturesConfig,
    bars: Bars,
    ema_fast: Ema,
    ema_slow: Ema,
    rsi: Rsi,
    ofi: Ofi,
    bars_since_update: u32,
    book_stale: bool,
}

impl Features {
    pub fn new(cfg: FeaturesConfig) -> Self {
        Self {
            cfg,
            bars: Bars::new(),
            ema_fast: Ema::new(cfg.ema_fast),
            ema_slow: Ema::new(cfg.ema_slow),
            rsi: Rsi::new(cfg.rsi_period),
            ofi: Ofi::new(cfg.ofi_window_ns),
            bars_since_update: 0,
            book_stale: true,
        }
    }

    /// Closes every bar whose boundary is at or before `now_ns`. Call from a
    /// clock as well as from events so a quiet minute still closes.
    pub fn advance(&mut self, now_ns: i64) {
        while let Some(bar) = self.bars.advance(now_ns) {
            self.on_close(&bar);
        }
    }

    fn on_close(&mut self, bar: &Bar) {
        if bar.gap {
            self.bars_since_update += 1;
            return;
        }
        let close = bar.close.0 as f64;
        self.ema_fast.update(close);
        self.ema_slow.update(close);
        self.rsi.update(close);
        self.bars_since_update = 0;
    }

    /// Every accepted book event, after the book applied it.
    pub fn on_book(&mut self, book: &Book, recv_ns: i64) {
        self.advance(recv_ns);
        self.book_stale = book.is_stale();
        if self.book_stale {
            self.ofi.reset();
            self.bars.mark_gap();
            return;
        }
        self.ofi.on_book(book, recv_ns);
        if self.cfg.bar_source == BarSource::Mid {
            if let Some(mid) = book.mid() {
                self.bars.sample(mid, None, recv_ns);
            }
        }
    }

    pub fn on_trade(&mut self, trade: &TradeEvent, recv_ns: i64) {
        self.advance(recv_ns);
        if self.cfg.bar_source != BarSource::Trades {
            return;
        }
        if self.book_stale {
            self.bars.mark_gap();
            return;
        }
        self.bars.sample(trade.price, Some(trade.qty), recv_ns);
    }

    /// Feed resyncs and disconnects: the book cannot be trusted until a snapshot.
    pub fn on_stale(&mut self) {
        self.book_stale = true;
        self.ofi.reset();
        self.bars.mark_gap();
    }

    pub fn snapshot(&self) -> Snapshot<'_> {
        Snapshot {
            rsi: self.rsi.value(),
            ema_fast: self.ema_fast.value(),
            ema_slow: self.ema_slow.value(),
            order_flow_imbalance: self.ofi.value(),
            bars: self.bars.closed(),
            forming: self.bars.forming(),
            bars_since_update: self.bars_since_update,
            volume_available: self.cfg.bar_source == BarSource::Trades,
            book_stale: self.book_stale,
        }
    }

    pub fn last_close(&self) -> Option<Price> {
        self.bars.closed().last().map(|b| b.close)
    }
}

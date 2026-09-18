use book::Book;
use features::{Bar, BarSource, Features, FeaturesConfig, BAR_NS, RING};
use types::{BookEvent, Level, Price, Qty, Side, TradeEvent};

fn lv(p: i64, q: i64) -> Level {
    Level {
        price: Price(p),
        qty: Qty(q),
    }
}

fn checksum(asks: &[Level], bids: &[Level]) -> u32 {
    let mut s = String::new();
    for l in asks.iter().chain(bids) {
        s.push_str(&format!("{}{}", l.price.0, l.qty.0));
    }
    crc32fast::hash(s.as_bytes())
}

fn book(bid: (i64, i64), ask: (i64, i64)) -> Book {
    let bids = vec![lv(bid.0, bid.1)];
    let asks = vec![lv(ask.0, ask.1)];
    let mut b = Book::new(10);
    b.apply(&BookEvent::Snapshot {
        checksum: checksum(&asks, &bids),
        bids,
        asks,
    })
    .unwrap();
    b
}

fn trade(price: i64, qty: i64, id: u64) -> TradeEvent {
    TradeEvent {
        side: Side::Buy,
        price: Price(price),
        qty: Qty(qty),
        venue_time_ns: 0,
        trade_id: id,
    }
}

fn trades_features() -> Features {
    let mut f = Features::new(FeaturesConfig::default());
    f.on_book(&book((100, 1), (101, 1)), 0); // book is live from t=0
    f
}

#[test]
fn events_straddling_a_minute_close_exactly_one_bar() {
    let mut f = trades_features();
    f.on_trade(&trade(100, 5, 1), 10 * 1_000_000_000);
    f.on_trade(&trade(103, 5, 2), 20 * 1_000_000_000);
    f.on_trade(&trade(99, 5, 3), 59 * 1_000_000_000);
    assert!(f.snapshot().bars.is_empty());
    f.on_trade(&trade(102, 7, 4), BAR_NS + 1);
    let s = f.snapshot();
    assert_eq!(s.bars.len(), 1);
    let b = s.bars[0];
    assert_eq!(
        (b.open.0, b.high.0, b.low.0, b.close.0, b.volume.0),
        (100, 103, 99, 99, 15)
    );
    assert_eq!((b.open_ns, b.close_ns), (0, BAR_NS));
    assert!(!b.gap);
    assert_eq!(b.samples, 3);
    assert_eq!(s.forming.open.0, 102);
    assert_eq!(s.forming.volume.0, 7);
    assert!(s.volume_available);
}

#[test]
fn quiet_minutes_close_as_gap_bars_on_advance() {
    let mut f = trades_features();
    f.on_trade(&trade(100, 1, 1), 1);
    f.advance(3 * BAR_NS + 5);
    let s = f.snapshot();
    assert_eq!(s.bars.len(), 3);
    assert!(!s.bars[0].gap);
    assert!(s.bars[1].gap && s.bars[2].gap);
    // Gap bars carry the last close forward so a chart has a price.
    assert_eq!(s.bars[1].close.0, 100);
    assert_eq!(s.bars_since_update, 2);
}

#[test]
fn stale_book_refuses_samples_and_marks_gap() {
    let mut f = trades_features();
    f.on_trade(&trade(100, 1, 1), 1);
    f.on_stale();
    f.on_trade(&trade(105, 1, 2), 2);
    f.advance(BAR_NS);
    let s = f.snapshot();
    assert!(s.bars[0].gap, "a refused sample marks the bar");
    assert_eq!(s.bars[0].close.0, 100, "the refused trade did not enter");
    assert_eq!(s.order_flow_imbalance, 0);
    assert!(s.book_stale);
}

#[test]
fn mid_source_uses_book_and_reports_no_volume() {
    let mut f = Features::new(FeaturesConfig {
        bar_source: BarSource::Mid,
        ..Default::default()
    });
    f.on_book(&book((100, 1), (102, 1)), 1);
    f.on_book(&book((104, 1), (106, 1)), 2);
    f.on_trade(&trade(999, 1, 1), 3); // ignored in Mid mode
    f.advance(BAR_NS);
    let s = f.snapshot();
    assert_eq!((s.bars[0].open.0, s.bars[0].close.0), (101, 105));
    assert_eq!(s.bars[0].volume, Qty::ZERO);
    assert!(!s.volume_available);
}

#[test]
fn ema_and_rsi_update_on_close_only_and_hold_over_gaps() {
    let mut f = trades_features();
    for i in 0..30i64 {
        f.on_trade(&trade(100 + i, 1, i as u64), i * BAR_NS + 1);
    }
    f.advance(30 * BAR_NS);
    let s = f.snapshot();
    assert_eq!(s.bars.len(), 30);
    let fast = s.ema_fast.unwrap();
    let slow = s.ema_slow.unwrap();
    assert!(fast > slow, "rising closes: fast above slow");
    assert_eq!(s.rsi, Some(100.0), "every close up");
    assert_eq!(s.bars_since_update, 0);
    // A trade mid-bar changes nothing until the bar closes.
    f.on_trade(&trade(50, 1, 99), 30 * BAR_NS + 5);
    assert_eq!(f.snapshot().ema_fast, Some(fast));
    f.advance(31 * BAR_NS);
    let after_close = f.snapshot().ema_fast.unwrap();
    assert!(after_close < fast, "the 50 close pulled the EMA down");
    // Three quiet minutes: values held, counter grows.
    f.advance(34 * BAR_NS);
    let s = f.snapshot();
    assert_eq!(s.ema_fast, Some(after_close));
    assert_eq!(s.bars_since_update, 3);
}

#[test]
fn ofi_sums_best_level_changes_over_the_window() {
    let mut f = Features::new(FeaturesConfig {
        ofi_window_ns: 10,
        ..Default::default()
    });
    f.on_book(&book((100, 5), (101, 5)), 0);
    f.on_book(&book((100, 8), (101, 5)), 1); // +3
    f.on_book(&book((100, 8), (101, 9)), 2); // -4
    assert_eq!(f.snapshot().order_flow_imbalance, -1);
    // Changes below the best level contribute 0: same touch, deeper level differs.
    let mut deep = Book::new(10);
    let bids = vec![lv(100, 8), lv(99, 50)];
    let asks = vec![lv(101, 9)];
    deep.apply(&BookEvent::Snapshot {
        checksum: checksum(&asks, &bids),
        bids,
        asks,
    })
    .unwrap();
    f.on_book(&deep, 3);
    assert_eq!(f.snapshot().order_flow_imbalance, -1);
    // Window expiry drops the +3 at t=11 and the -4 at t=12.
    f.on_book(&deep, 11);
    assert_eq!(f.snapshot().order_flow_imbalance, -4);
    f.on_book(&deep, 12);
    assert_eq!(f.snapshot().order_flow_imbalance, 0);
}

#[test]
fn ring_holds_512_bars_oldest_first() {
    let mut f = trades_features();
    let n = RING as i64 + 10;
    for i in 0..n {
        f.on_trade(&trade(1000 + i, 1, i as u64), i * BAR_NS + 1);
    }
    f.advance(n * BAR_NS);
    let bars: &[Bar] = f.snapshot().bars;
    assert_eq!(bars.len(), RING);
    assert_eq!(bars[0].close.0, 1000 + 10);
    assert_eq!(bars[RING - 1].close.0, 1000 + n - 1);
    assert!(bars.windows(2).all(|w| w[0].close_ns == w[1].open_ns));
}

#[test]
fn clock_leap_past_the_ring_jumps_instead_of_crawling() {
    let mut f = trades_features();
    f.on_trade(&trade(100, 1, 1), 1);
    let far = 1_000_000 * BAR_NS + 7;
    f.advance(far);
    let s = f.snapshot();
    assert_eq!(s.bars.len(), 1, "only the bar that had data closed");
    assert_eq!(s.forming.open_ns, 1_000_000 * BAR_NS);
    assert!(s.forming.gap);
}

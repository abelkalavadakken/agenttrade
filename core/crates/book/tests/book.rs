use book::{ApplyError, Book};
use types::{BookEvent, Level, Price, Qty};

fn lv(p: i64, q: i64) -> Level {
    Level {
        price: Price(p),
        qty: Qty(q),
    }
}

/// Reference checksum: the string rule, independent of the crate's buffer code.
fn reference_checksum(asks: &[Level], bids: &[Level]) -> u32 {
    let mut s = String::new();
    for l in asks.iter().chain(bids) {
        s.push_str(&l.price.0.to_string());
        s.push_str(&l.qty.0.to_string());
    }
    crc32fast::hash(s.as_bytes())
}

fn snapshot(bids: Vec<Level>, asks: Vec<Level>) -> BookEvent {
    let checksum = reference_checksum(&asks, &bids);
    BookEvent::Snapshot {
        bids,
        asks,
        checksum,
    }
}

fn delta(book_after: (&[Level], &[Level]), bids: Vec<Level>, asks: Vec<Level>) -> BookEvent {
    BookEvent::Delta {
        bids,
        asks,
        checksum: reference_checksum(book_after.1, book_after.0),
        venue_time_ns: 0,
    }
}

fn base() -> Book {
    let mut b = Book::new(3);
    b.apply(&snapshot(
        vec![lv(100, 5), lv(90, 6), lv(80, 7)],
        vec![lv(110, 1), lv(120, 2), lv(130, 3)],
    ))
    .unwrap();
    b
}

#[test]
fn new_book_is_stale_and_empty() {
    let b = Book::new(10);
    assert!(b.is_stale());
    assert!(b.best_bid().is_none());
    assert!(b.mid().is_none());
    assert_eq!(b.depth(), 10);
    assert_eq!(Book::new(99).depth(), book::MAX_DEPTH);
}

#[test]
fn delta_before_snapshot_is_refused() {
    let mut b = Book::new(3);
    let e = delta((&[], &[]), vec![lv(1, 1)], vec![]);
    assert_eq!(b.apply(&e), Err(ApplyError::NoSnapshot));
    assert!(b.is_stale());
}

#[test]
fn snapshot_sorts_and_reads() {
    let mut b = Book::new(3);
    let sorted_bids = [lv(100, 5), lv(99, 6), lv(98, 7)];
    let sorted_asks = [lv(101, 1), lv(102, 2), lv(103, 3)];
    b.apply(&BookEvent::Snapshot {
        bids: vec![lv(98, 7), lv(100, 5), lv(99, 6)],
        asks: vec![lv(103, 3), lv(101, 1), lv(102, 2)],
        checksum: reference_checksum(&sorted_asks, &sorted_bids),
    })
    .unwrap();
    assert!(!b.is_stale());
    assert_eq!(b.bids(), &[lv(100, 5), lv(99, 6), lv(98, 7)]);
    assert_eq!(b.asks(), &[lv(101, 1), lv(102, 2), lv(103, 3)]);
    assert_eq!(b.best_bid(), Some(lv(100, 5)));
    assert_eq!(b.best_ask(), Some(lv(101, 1)));
    assert_eq!(b.mid(), Some(Price(100)));
    assert_eq!(b.spread(), Some(Price(1)));
    assert_eq!(b.total_qty(), (Qty(18), Qty(6)));
}

#[test]
fn delta_replace_insert_remove_truncate() {
    let mut b = base();
    // replace 90, insert 95 -> pushes 80 out, remove 110
    let after_bids = [lv(100, 5), lv(95, 1), lv(90, 9)];
    let after_asks = [lv(120, 2), lv(130, 3)];
    let e = delta(
        (&after_bids, &after_asks),
        vec![lv(90, 9), lv(95, 1)],
        vec![lv(110, 0)],
    );
    b.apply(&e).unwrap();
    assert_eq!(b.bids(), &after_bids);
    assert_eq!(b.asks(), &after_asks);
}

#[test]
fn repeated_price_last_write_wins() {
    let mut b = base();
    let after_bids = [lv(100, 5), lv(90, 3), lv(80, 7)];
    let after_asks = [lv(110, 1), lv(120, 2), lv(130, 3)];
    let e = delta(
        (&after_bids, &after_asks),
        vec![lv(90, 1), lv(90, 2), lv(90, 3)],
        vec![],
    );
    b.apply(&e).unwrap();
    assert_eq!(b.bids()[1], lv(90, 3));
}

#[test]
fn remove_absent_price_is_noop() {
    let mut b = base();
    let bids = b.bids().to_vec();
    let asks = b.asks().to_vec();
    let e = delta((&bids, &asks), vec![lv(50, 0)], vec![lv(500, 0)]);
    b.apply(&e).unwrap();
    assert_eq!(b.bids(), &bids[..]);
}

#[test]
fn checksum_mismatch_keeps_levels_and_marks_stale() {
    let mut b = base();
    let bad = BookEvent::Delta {
        bids: vec![lv(100, 1)],
        asks: vec![],
        checksum: 1,
        venue_time_ns: 0,
    };
    let err = b.apply(&bad).unwrap_err();
    assert!(matches!(err, ApplyError::Checksum { expected: 1, .. }));
    assert!(b.is_stale());
    assert_eq!(
        b.best_bid(),
        Some(lv(100, 1)),
        "levels applied, then flagged"
    );
    assert_eq!(b.venue_checksum(), 1);
}

#[test]
fn crossed_book_is_detected() {
    let mut b = base();
    let after_bids = [lv(110, 1), lv(100, 5), lv(90, 6)];
    let after_asks = [lv(110, 1), lv(120, 2), lv(130, 3)];
    let e = delta((&after_bids, &after_asks), vec![lv(110, 1)], vec![]);
    assert_eq!(b.apply(&e), Err(ApplyError::Crossed));
    assert!(b.is_crossed());
    assert!(b.is_stale());
}

#[test]
fn snapshot_clears_stale_and_crossed() {
    let mut b = base();
    b.mark_stale();
    assert!(b.is_stale());
    b.apply(&snapshot(vec![lv(10, 1)], vec![lv(11, 1)]))
        .unwrap();
    assert!(!b.is_stale());
    assert!(!b.is_crossed());
    assert_eq!(b.bids(), &[lv(10, 1)]);
}

#[test]
fn mid_rounds_down_on_half_tick() {
    let mut b = Book::new(1);
    b.apply(&snapshot(vec![lv(100, 1)], vec![lv(103, 1)]))
        .unwrap();
    assert_eq!(b.mid(), Some(Price(101)));
    b.apply(&snapshot(vec![lv(-3, 1)], vec![lv(0, 1)])).unwrap();
    assert_eq!(b.mid(), Some(Price(-2)));
}

#[test]
fn insert_beyond_max_depth_is_dropped() {
    let mut b = Book::new(book::MAX_DEPTH);
    let bids: Vec<Level> = (0..book::MAX_DEPTH as i64)
        .map(|i| lv(1000 - i, 1))
        .collect();
    b.apply(&snapshot(bids.clone(), vec![lv(2000, 1)])).unwrap();
    let e = delta((&bids, &[lv(2000, 1)]), vec![lv(1, 1)], vec![]);
    b.apply(&e).unwrap();
    assert_eq!(b.bids().len(), book::MAX_DEPTH);
    assert_eq!(b.bids(), &bids[..]);
}

//! Parser, checksum and gap detection against captured Kraken v2 frames.

use std::collections::BTreeMap;

use feed::kraken::{book_checksum, MsgKind, Tracker};
use types::{instruments, BookEvent, FeedEvent, ResyncReason};

const FIXTURE: &str = include_str!("../../../../tests/fixtures/kraken_book_btcusd.jsonl");

fn tracker() -> Tracker {
    Tracker::new(instruments::find("kraken", "BTC/USD").unwrap(), 10)
}

fn frames() -> impl Iterator<Item = &'static str> {
    FIXTURE.lines().filter(|l| !l.is_empty())
}

fn is_book(l: &str, kind: &str) -> bool {
    l.starts_with(&format!("{{\"channel\":\"book\",\"type\":\"{kind}\""))
}

fn snapshot_frame() -> &'static str {
    frames().find(|l| is_book(l, "snapshot")).unwrap()
}

fn update_frames() -> impl Iterator<Item = &'static str> {
    frames().filter(|l| is_book(l, "update"))
}

#[test]
fn parses_every_captured_frame_and_verifies_every_checksum() {
    let mut t = tracker();
    let mut books = 0;
    for line in frames() {
        let h = t.on_frame(line.as_bytes());
        assert_ne!(h.kind, Some(MsgKind::Unparsed), "unparsed: {line}");
        assert!(!h.resubscribe, "checksum failure on: {line}");
        books += h
            .events
            .iter()
            .filter(|e| matches!(e, FeedEvent::Book(_)))
            .count();
    }
    let s = t.stats();
    assert_eq!(s.snapshots, 1);
    assert_eq!(s.checksum_failures, 0);
    assert_eq!(s.gaps, 0);
    assert_eq!(s.unparsed, 0);
    assert!(s.heartbeats > 0);
    assert_eq!(books as u64, s.snapshots + s.updates);
}

#[test]
fn snapshot_parses_to_fixed_point() {
    let mut t = tracker();
    let h = t.on_frame(snapshot_frame().as_bytes());
    let Some(FeedEvent::Book(BookEvent::Snapshot { bids, asks, .. })) = h.events.first() else {
        panic!("expected snapshot, got {h:?}");
    };
    assert_eq!(bids.len(), 10);
    assert_eq!(asks.len(), 10);
    // Fixture top bid is 77362.8 at price_scale 1.
    assert_eq!(bids[0].price.0, 773_628);
    assert_eq!(bids[0].qty.0, 5_300_065);
    assert!(bids.windows(2).all(|w| w[0].price > w[1].price));
    assert!(asks.windows(2).all(|w| w[0].price < w[1].price));
}

#[test]
fn checksum_matches_known_good_snapshot() {
    let mut t = tracker();
    let h = t.on_frame(snapshot_frame().as_bytes());
    let Some(FeedEvent::Book(BookEvent::Snapshot {
        bids,
        asks,
        checksum,
    })) = h.events.first()
    else {
        panic!("expected snapshot");
    };
    let bids: BTreeMap<i64, i64> = bids.iter().map(|l| (l.price.0, l.qty.0)).collect();
    let asks: BTreeMap<i64, i64> = asks.iter().map(|l| (l.price.0, l.qty.0)).collect();
    assert_eq!(book_checksum(&asks, &bids, 10), *checksum);
    assert_ne!(book_checksum(&asks, &bids, 9), *checksum);
}

#[test]
fn checksum_mismatch_triggers_resync_and_drops_book() {
    let mut t = tracker();
    t.on_frame(snapshot_frame().as_bytes());
    let bad = update_frames()
        .next()
        .unwrap()
        .replace("\"checksum\":", "\"checksum\":4,\"orig\":");
    let h = t.on_frame(bad.as_bytes());
    assert!(h.resubscribe);
    assert_eq!(
        h.events,
        vec![FeedEvent::Resync(ResyncReason::ChecksumMismatch)]
    );
    assert!(!t.has_book());
    assert_eq!(t.stats().checksum_failures, 1);
}

#[test]
fn updates_before_snapshot_are_ignored() {
    let mut t = tracker();
    let update = update_frames().next().unwrap();
    let h = t.on_frame(update.as_bytes());
    assert_eq!(h.kind, Some(MsgKind::Update));
    assert!(h.events.is_empty());
    assert_eq!(t.stats().ignored_updates, 1);
}

#[test]
fn unsolicited_snapshot_is_a_gap() {
    let mut t = tracker();
    let snap = snapshot_frame();
    let first = t.on_frame(snap.as_bytes());
    assert!(first
        .events
        .iter()
        .all(|e| !matches!(e, FeedEvent::Resync(_))));
    let second = t.on_frame(snap.as_bytes());
    assert_eq!(
        second.events.first(),
        Some(&FeedEvent::Resync(ResyncReason::SequenceGap))
    );
    assert!(!second.resubscribe, "the snapshot itself is a fresh book");
    assert!(t.has_book());
    assert_eq!(t.stats().gaps, 1);

    // After an explicit resubscribe, the snapshot is expected again.
    t.expect_snapshot();
    let third = t.on_frame(snap.as_bytes());
    assert!(third
        .events
        .iter()
        .all(|e| !matches!(e, FeedEvent::Resync(_))));
    assert_eq!(t.stats().gaps, 1);
}

#[test]
fn synthetic_sequence_gap_then_recovery() {
    let mut t = tracker();
    let snap = snapshot_frame();
    let updates: Vec<&str> = update_frames().take(3).collect();
    t.on_frame(snap.as_bytes());
    t.on_frame(updates[0].as_bytes());
    // Venue restarts the stream mid-flight: snapshot without our asking.
    let h = t.on_frame(snap.as_bytes());
    assert_eq!(h.events.len(), 2);
    assert_eq!(h.events[0], FeedEvent::Resync(ResyncReason::SequenceGap));
    assert!(matches!(
        h.events[1],
        FeedEvent::Book(BookEvent::Snapshot { .. })
    ));
    // The stream continues from the fresh snapshot and checksums hold.
    for u in &updates {
        let h = t.on_frame(u.as_bytes());
        assert!(!h.resubscribe);
    }
    assert_eq!(t.stats().gaps, 1);
    assert_eq!(t.stats().checksum_failures, 0);
}

#[test]
fn garbage_and_other_channels() {
    let mut t = tracker();
    assert_eq!(t.on_frame(b"not json").kind, Some(MsgKind::Unparsed));
    assert_eq!(t.on_frame(b"\xff\xfe").kind, Some(MsgKind::Unparsed));
    assert_eq!(
        t.on_frame(br#"{"channel":"heartbeat"}"#).kind,
        Some(MsgKind::Heartbeat)
    );
    assert_eq!(
        t.on_frame(br#"{"method":"subscribe","success":false,"error":"x"}"#)
            .kind,
        Some(MsgKind::Error)
    );
    assert_eq!(
        t.on_frame(br#"{"channel":"book","type":"update","data":[{"symbol":"ETH/USD","bids":[],"asks":[],"checksum":1}]}"#)
            .events,
        vec![]
    );
}

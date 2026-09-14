//! Trade channel parser against captured Kraken v2 frames.

use feed::kraken::{MsgKind, Tracker};
use types::{instruments, FeedEvent, Side};

const FIXTURE: &str = include_str!("../../../../tests/fixtures/kraken_trade_btcusd.jsonl");

fn tracker() -> Tracker {
    Tracker::new(instruments::find("kraken", "BTC/USD").unwrap(), 10)
}

fn trade_frames() -> impl Iterator<Item = &'static str> {
    FIXTURE
        .lines()
        .filter(|l| l.starts_with("{\"channel\":\"trade\""))
}

#[test]
fn parses_every_trade_frame() {
    let mut t = tracker();
    let mut frames = 0;
    let mut events = 0;
    for line in FIXTURE.lines().filter(|l| !l.is_empty()) {
        let h = t.on_frame(line.as_bytes());
        assert_ne!(h.kind, Some(MsgKind::Unparsed), "unparsed: {line}");
        if h.kind == Some(MsgKind::Trade) {
            frames += 1;
            events += h.events.len();
            assert!(h.events.iter().all(|e| matches!(e, FeedEvent::Trade(_))));
        }
    }
    let data_entries: usize = trade_frames()
        .map(|l| l.matches("\"trade_id\"").count())
        .sum();
    assert_eq!(frames, 14, "fixture has this many trade frames");
    assert_eq!(events, data_entries);
    assert_eq!(t.stats().trades as usize, events);
    assert_eq!(t.stats().unparsed, 0);
    assert!(events > frames, "at least one frame carries several trades");
}

#[test]
fn first_trade_fields_are_fixed_point() {
    let mut t = tracker();
    let h = t.on_frame(trade_frames().next().unwrap().as_bytes());
    let FeedEvent::Trade(tr) = &h.events[0] else {
        panic!("expected trade");
    };
    assert_eq!(tr.side, Side::Buy);
    assert_eq!(tr.price.0, 791_929);
    assert_eq!(tr.qty.0, 5_319);
    assert_eq!(tr.trade_id, 107_725_589);
    assert!(tr.venue_time_ns > 1_700_000_000 * 1_000_000_000);
}

#[test]
fn trades_do_not_need_a_book_and_ignore_other_symbols() {
    let mut t = tracker();
    assert!(!t.has_book());
    let h = t.on_frame(trade_frames().next().unwrap().as_bytes());
    assert!(!h.events.is_empty());
    assert!(!h.resubscribe);
    let other = br#"{"channel":"trade","type":"update","data":[{"symbol":"ETH/USD","side":"sell","price":1.5,"qty":1,"ord_type":"market","trade_id":1,"timestamp":"2026-01-01T00:00:00Z"}]}"#;
    assert_eq!(t.on_frame(other).events, vec![]);
    let bad_side = br#"{"channel":"trade","type":"update","data":[{"symbol":"BTC/USD","side":"up","price":1.5,"qty":1,"ord_type":"market","trade_id":1,"timestamp":"2026-01-01T00:00:00Z"}]}"#;
    assert_eq!(t.on_frame(bad_side).kind, Some(MsgKind::Unparsed));
}

//! Reference strategies on hand-built state, and param bounds.

use book::Book;
use features::{Bar, Snapshot, BAR_NS};
use strategy::{Action, Breakout, Mode, MrOfi, ParamError, SetupState, State, Strategy};
use types::{BookEvent, Level, Position, Price, Qty, Side};

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

fn book(bid: i64, ask: i64) -> Book {
    let bids = vec![lv(bid, 100_000_000)];
    let asks = vec![lv(ask, 100_000_000)];
    let mut b = Book::new(10);
    b.apply(&BookEvent::Snapshot {
        checksum: checksum(&asks, &bids),
        bids,
        asks,
    })
    .unwrap();
    b
}

fn snapshot<'a>(bars: &'a [Bar], ofi: i64, volume_available: bool) -> Snapshot<'a> {
    Snapshot {
        rsi: None,
        ema_fast: None,
        ema_slow: None,
        order_flow_imbalance: ofi,
        bars,
        forming: Bar::default(),
        bars_since_update: 0,
        volume_available,
        book_stale: false,
    }
}

fn state<'a>(now_ns: i64, seq: u64, book: &'a Book, f: &'a Snapshot<'a>, net: i64) -> State<'a> {
    State {
        now_ns,
        sequence_id: seq,
        book,
        features: f,
        position: Position {
            net_qty: Qty(net),
            ..Default::default()
        },
        open_orders: 0,
    }
}

#[test]
fn params_accept_bounds_and_reject_one_past() {
    for s in [
        Box::new(MrOfi::new()) as Box<dyn Strategy>,
        Box::new(Breakout::new()),
    ] {
        let mut s = s;
        for p in s.params().to_vec() {
            assert!(s.set_param(p.name, p.min).is_ok(), "{} min", p.name);
            assert!(s.set_param(p.name, p.max).is_ok(), "{} max", p.name);
            assert!(
                matches!(
                    s.set_param(p.name, p.min - 1),
                    Err(ParamError::OutOfBounds { .. })
                ),
                "{} under",
                p.name
            );
            assert!(
                matches!(
                    s.set_param(p.name, p.max + 1),
                    Err(ParamError::OutOfBounds { .. })
                ),
                "{} over",
                p.name
            );
            assert_eq!(s.param(p.name), Some(p.max));
        }
        assert_eq!(s.set_param("nope", 1), Err(ParamError::Unknown));
    }
    assert_eq!(MrOfi::new().mode(), Mode::Autonomous);
    assert_eq!(Breakout::new().mode(), Mode::Gated);
}

#[test]
fn mr_ofi_enters_on_the_right_side_at_the_touch() {
    let mut s = MrOfi::new();
    let b = book(1000, 1003);
    // Heavy sell flow: buy at the bid with the stop below.
    let f = snapshot(&[], -300_000_000, true);
    let a = s.on_event(&state(0, 1, &b, &f, 0)).unwrap();
    let Action::Place(o) = a else {
        panic!("expected place")
    };
    assert_eq!(
        (o.side, o.price, o.stop, o.qty),
        (Side::Buy, Price(1000), Price(950), Qty(1_000_000))
    );
    // Heavy buy flow: sell at the ask.
    let mut s = MrOfi::new();
    let f = snapshot(&[], 300_000_000, true);
    let Action::Place(o) = s.on_event(&state(0, 1, &b, &f, 0)).unwrap() else {
        panic!()
    };
    assert_eq!(
        (o.side, o.price, o.stop),
        (Side::Sell, Price(1003), Price(1053))
    );
}

#[test]
fn mr_ofi_needs_spread_flow_and_a_live_book() {
    let mut s = MrOfi::new();
    let f = snapshot(&[], -300_000_000, true);
    assert!(
        s.on_event(&state(0, 1, &book(1000, 1001), &f, 0)).is_none(),
        "spread 1 tick"
    );
    let f = snapshot(&[], -100_000_000, true);
    assert!(
        s.on_event(&state(0, 1, &book(1000, 1003), &f, 0)).is_none(),
        "flow under threshold"
    );
    let mut f = snapshot(&[], -300_000_000, true);
    f.book_stale = true;
    assert!(
        s.on_event(&state(0, 1, &book(1000, 1003), &f, 0)).is_none(),
        "stale"
    );
    let mut stale = book(1000, 1003);
    stale.mark_stale();
    let f = snapshot(&[], -300_000_000, true);
    assert!(
        s.on_event(&state(0, 1, &stale, &f, 0)).is_none(),
        "book stale flag"
    );
}

#[test]
fn mr_ofi_exits_on_flip_or_hold_then_cools_down() {
    let mut s = MrOfi::new();
    let b = book(1000, 1003);
    let f = snapshot(&[], -300_000_000, true);
    assert!(matches!(
        s.on_event(&state(0, 1, &b, &f, 0)),
        Some(Action::Place(_))
    ));
    // In position, flow flips to buying: flatten.
    let f2 = snapshot(&[], 50_000_000, true);
    assert!(matches!(
        s.on_event(&state(1, 2, &b, &f2, 1_000_000)),
        Some(Action::Flatten {
            reason: "ofi flipped"
        })
    ));
    // Cooldown 60 s: no entry at t=30 s even with flow, entry again at 61 s.
    assert!(s.on_event(&state(30_000_000_000, 3, &b, &f, 0)).is_none());
    assert!(matches!(
        s.on_event(&state(61_000_000_000, 4, &b, &f, 0)),
        Some(Action::Place(_))
    ));
    // Hold expiry: same-sign flow for 30 s.
    let held = s.on_event(&state(
        61_000_000_000 + 30_000_000_000,
        5,
        &b,
        &f,
        1_000_000,
    ));
    assert!(matches!(
        held,
        Some(Action::Flatten {
            reason: "hold expired"
        })
    ));
}

fn bars(closes: &[i64], last_volume: i64) -> Vec<Bar> {
    closes
        .iter()
        .enumerate()
        .map(|(i, c)| Bar {
            open: Price(*c),
            high: Price(*c + 5),
            low: Price(*c - 5),
            close: Price(*c),
            volume: Qty(if i == closes.len() - 1 {
                last_volume
            } else {
                100
            }),
            open_ns: i as i64 * BAR_NS,
            close_ns: (i as i64 + 1) * BAR_NS,
            gap: false,
            samples: 1,
        })
        .collect()
}

#[test]
fn breakout_forms_inside_the_range_and_activates_on_a_close_beyond_with_volume() {
    let mut s = Breakout::new();
    s.set_param("lookback_bars", 5).unwrap();
    let b = book(1000, 1001);
    // Five range bars around 1000, then a close inside: Forming.
    let inside = bars(&[1000, 1002, 998, 1001, 999, 1003], 200);
    let f = snapshot(&inside, 0, true);
    assert!(s.on_event(&state(0, 10, &b, &f, 0)).is_none());
    assert_eq!(s.setup_state(), SetupState::Forming);
    // Same bar count: not re-evaluated.
    assert_eq!(s.setup_state(), SetupState::Forming);
    // A close above the range high (1007) with 2x volume: Active.
    let above = bars(&[1000, 1002, 998, 1001, 999, 1003, 1020], 200);
    let f = snapshot(&above, 0, true);
    s.on_event(&state(1, 11, &b, &f, 0));
    let SetupState::Active { setup_id, order } = s.setup_state() else {
        panic!("expected active")
    };
    assert_eq!(setup_id, 11);
    assert_eq!((order.side, order.price), (Side::Buy, Price(1001)));
    assert_eq!(order.stop, Price(993 - 200), "range low minus stop_ticks");
    // Cleared: back to None until a new bar closes.
    s.on_setup_cleared();
    assert_eq!(s.setup_state(), SetupState::None);
    s.on_event(&state(2, 12, &b, &f, 0));
    assert_eq!(
        s.setup_state(),
        SetupState::None,
        "no new bar, no new setup"
    );
}

#[test]
fn breakout_without_volume_stays_forming_and_symmetric_below() {
    let mut s = Breakout::new();
    s.set_param("lookback_bars", 5).unwrap();
    let b = book(1000, 1001);
    let thin = bars(&[1000, 1002, 998, 1001, 999, 1003, 1020], 100);
    let f = snapshot(&thin, 0, true);
    s.on_event(&state(0, 10, &b, &f, 0));
    assert_eq!(
        s.setup_state(),
        SetupState::Forming,
        "volume 1x is under 1.5x"
    );
    // Mid bars, volume unavailable: the volume test is skipped, reason says so.
    let mut s = Breakout::new();
    s.set_param("lookback_bars", 5).unwrap();
    let below = bars(&[1000, 1002, 998, 1001, 999, 1003, 980], 0);
    let f = snapshot(&below, 0, false);
    s.on_event(&state(0, 10, &b, &f, 0));
    let SetupState::Active { order, .. } = s.setup_state() else {
        panic!()
    };
    assert_eq!(order.side, Side::Sell);
    assert_eq!(order.stop, Price(1008 + 200));
    assert!(order.reason.contains("volume unavailable"));
}

#[test]
fn mr_ofi_sends_one_flatten_and_waits_for_the_exit() {
    let mut s = MrOfi::new();
    let b = book(1000, 1003);
    let f = snapshot(&[], -300_000_000, true);
    assert!(matches!(
        s.on_event(&state(0, 1, &b, &f, 0)),
        Some(Action::Place(_))
    ));
    let flip = snapshot(&[], 50_000_000, true);
    assert!(matches!(
        s.on_event(&state(1, 2, &b, &flip, 1_000_000)),
        Some(Action::Flatten { .. })
    ));
    // Still in position while the IOC is in flight: silence.
    assert!(s.on_event(&state(2, 3, &b, &flip, 1_000_000)).is_none());
    assert!(s.on_event(&state(3, 4, &b, &flip, 1_000_000)).is_none());
    // Flat again: the exit is done; cooldown applies to the next entry.
    assert!(s.on_event(&state(4, 5, &b, &f, 0)).is_none(), "in cooldown");
    assert!(matches!(
        s.on_event(&state(61_000_000_000, 6, &b, &f, 0)),
        Some(Action::Place(_))
    ));
}

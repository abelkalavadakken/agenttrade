use book::Book;
use exec::{CancelReason, ExecError, ExecEvent, Order, PaperConfig, PaperVenue};
use types::{BookEvent, Intent, IntentEnvelope, Level, OrderState, Price, Qty, Side, TimeInForce};

const ACK: i64 = 1_000;
const FILL: i64 = 500;
const CANCEL: i64 = 700;

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

/// Bids 100/99/98, asks 101/102/103, one unit each side at qty 1e8 (1 BTC).
fn book_at(bid: i64, ask: i64) -> Book {
    let bids = vec![
        lv(bid, 100_000_000),
        lv(bid - 1, 100_000_000),
        lv(bid - 2, 100_000_000),
    ];
    let asks = vec![
        lv(ask, 100_000_000),
        lv(ask + 1, 100_000_000),
        lv(ask + 2, 100_000_000),
    ];
    let mut b = Book::new(10);
    b.apply(&BookEvent::Snapshot {
        checksum: checksum(&asks, &bids),
        bids,
        asks,
    })
    .unwrap();
    b
}

fn venue() -> PaperVenue {
    PaperVenue::new(
        PaperConfig {
            ack_latency_ns: ACK,
            fill_latency_ns: FILL,
            cancel_latency_ns: CANCEL,
        },
        1_000_000,
        8,
    )
}

fn env(intent: Intent) -> IntentEnvelope {
    IntentEnvelope {
        intent_id: "i".into(),
        agent_id: "t".into(),
        source_sequence_id: 1,
        generated_time_ns: 0,
        venue: "kraken".into(),
        symbol: "BTC/USD".into(),
        intent,
    }
}

fn place(side: Side, price: i64, stop: i64, qty: i64, tif: TimeInForce) -> Intent {
    Intent::Place {
        side,
        price: Price(price),
        stop: Price(stop),
        take_profit: Price::ZERO,
        qty: Qty(qty),
        tif,
    }
}

fn state(v: &PaperVenue, id: u64) -> OrderState {
    v.order(id).unwrap().state
}

fn fills(out: &[ExecEvent]) -> Vec<(u64, i64, i64, bool)> {
    out.iter()
        .filter_map(|e| match e {
            ExecEvent::Fill {
                order_id,
                price,
                qty,
                thin_book,
                ..
            } => Some((*order_id, price.0, qty.0, *thin_book)),
            _ => None,
        })
        .collect()
}

fn transitions(out: &[ExecEvent], id: u64) -> Vec<(OrderState, OrderState)> {
    out.iter()
        .filter_map(|e| match e {
            ExecEvent::Transition {
                order_id, from, to, ..
            } if *order_id == id => Some((*from, *to)),
            _ => None,
        })
        .collect()
}

#[test]
fn ack_is_not_visible_before_latency() {
    let mut v = venue();
    let b = book_at(100, 101);
    let mut out = Vec::new();
    let id = v
        .submit(
            &env(place(Side::Buy, 99, 90, 50_000_000, TimeInForce::Gtc)),
            &b,
            0,
            &mut out,
        )
        .unwrap();
    assert_eq!(state(&v, id), OrderState::PendingNew);
    v.on_book(&b, ACK - 1, &mut out);
    assert_eq!(state(&v, id), OrderState::PendingNew);
    v.on_book(&b, ACK, &mut out);
    assert_eq!(state(&v, id), OrderState::Open);
    assert_eq!(
        transitions(&out, id),
        vec![(OrderState::PendingNew, OrderState::Open)]
    );
}

#[test]
fn buy_at_ask_fills_displayed_qty_only() {
    let mut v = venue();
    let b = book_at(100, 101);
    let mut out = Vec::new();
    // Want 1.5 BTC at 101; only 1 BTC displayed at 101.
    let id = v
        .submit(
            &env(place(Side::Buy, 101, 90, 150_000_000, TimeInForce::Gtc)),
            &b,
            0,
            &mut out,
        )
        .unwrap();
    v.on_book(&b, ACK, &mut out);
    assert_eq!(fills(&out), vec![(id, 101, 100_000_000, false)]);
    assert_eq!(state(&v, id), OrderState::PartiallyFilled);
    assert_eq!(v.order(id).unwrap().filled, Qty(100_000_000));
    assert_eq!(v.account().position.net_qty, Qty(100_000_000));
}

#[test]
fn buy_above_ask_fills_at_ask_with_improvement() {
    let mut v = venue();
    let b = book_at(100, 101);
    let mut out = Vec::new();
    let id = v
        .submit(
            &env(place(Side::Buy, 102, 90, 150_000_000, TimeInForce::Gtc)),
            &b,
            0,
            &mut out,
        )
        .unwrap();
    v.on_book(&b, ACK, &mut out);
    assert_eq!(
        fills(&out),
        vec![(id, 101, 100_000_000, false), (id, 102, 50_000_000, false)]
    );
    assert_eq!(state(&v, id), OrderState::Filled);
    // avg entry = (101 * 1 + 102 * 0.5) / 1.5 = 101.33 -> 101 rounded toward zero
    assert_eq!(v.account().position.average_entry_price, Price(101));
}

#[test]
fn rest_then_fill_on_later_book_after_fill_latency() {
    let mut v = venue();
    let mut out = Vec::new();
    let id = v
        .submit(
            &env(place(Side::Buy, 99, 90, 50_000_000, TimeInForce::Gtc)),
            &book_at(100, 101),
            0,
            &mut out,
        )
        .unwrap();
    v.on_book(&book_at(100, 101), ACK, &mut out);
    assert_eq!(state(&v, id), OrderState::Open);
    assert!(fills(&out).is_empty());
    // Ask drops to 99 at t=5000: fill scheduled for 5500.
    let crossed = book_at(98, 99);
    v.on_book(&crossed, 5_000, &mut out);
    assert!(fills(&out).is_empty());
    assert_eq!(v.order(id).unwrap().pending_fill, Qty(50_000_000));
    v.on_book(&crossed, 5_499, &mut out);
    assert!(fills(&out).is_empty());
    v.on_book(&crossed, 5_500, &mut out);
    assert_eq!(fills(&out), vec![(id, 99, 50_000_000, false)]);
    assert_eq!(state(&v, id), OrderState::Filled);
    assert_eq!(v.order(id).unwrap().pending_fill, Qty::ZERO);
}

#[test]
fn resting_order_is_not_matched_twice_while_fill_pending() {
    let mut v = venue();
    let mut out = Vec::new();
    let id = v
        .submit(
            &env(place(Side::Buy, 99, 90, 50_000_000, TimeInForce::Gtc)),
            &book_at(100, 101),
            0,
            &mut out,
        )
        .unwrap();
    v.on_book(&book_at(100, 101), ACK, &mut out);
    let crossed = book_at(98, 99);
    v.on_book(&crossed, 5_000, &mut out);
    v.on_book(&crossed, 5_100, &mut out);
    v.on_book(&crossed, 5_200, &mut out);
    v.on_book(&crossed, 6_000, &mut out);
    assert_eq!(fills(&out), vec![(id, 99, 50_000_000, false)]);
    assert_eq!(v.order(id).unwrap().filled, Qty(50_000_000));
}

#[test]
fn ioc_partial_cancels_remainder() {
    let mut v = venue();
    let b = book_at(100, 101);
    let mut out = Vec::new();
    let id = v
        .submit(
            &env(place(Side::Buy, 101, 90, 150_000_000, TimeInForce::Ioc)),
            &b,
            0,
            &mut out,
        )
        .unwrap();
    v.on_book(&b, ACK, &mut out);
    assert_eq!(fills(&out), vec![(id, 101, 100_000_000, false)]);
    let o: &Order = v.order(id).unwrap();
    assert_eq!(o.state, OrderState::Canceled);
    assert_eq!(o.reason, Some(CancelReason::IocUnfilled));
    assert_eq!(o.filled, Qty(100_000_000));
}

#[test]
fn ioc_no_fill_cancels_with_zero_filled() {
    let mut v = venue();
    let b = book_at(100, 101);
    let mut out = Vec::new();
    let id = v
        .submit(
            &env(place(Side::Buy, 99, 90, 100_000_000, TimeInForce::Ioc)),
            &b,
            0,
            &mut out,
        )
        .unwrap();
    v.on_book(&b, ACK, &mut out);
    assert!(fills(&out).is_empty());
    assert_eq!(state(&v, id), OrderState::Canceled);
    assert_eq!(v.order(id).unwrap().filled, Qty::ZERO);
}

#[test]
fn fok_insufficient_depth_fills_nothing() {
    let mut v = venue();
    let b = book_at(100, 101);
    let mut out = Vec::new();
    // 3 BTC displayed across 101..103; ask for 3.5 at 103.
    let id = v
        .submit(
            &env(place(Side::Buy, 103, 90, 350_000_000, TimeInForce::Fok)),
            &b,
            0,
            &mut out,
        )
        .unwrap();
    v.on_book(&b, ACK, &mut out);
    assert!(fills(&out).is_empty());
    let o = v.order(id).unwrap();
    assert_eq!(o.state, OrderState::Canceled);
    assert_eq!(o.reason, Some(CancelReason::FokUnfillable));
    assert!(v.account().is_flat());
    // Exactly 3 BTC fills in full.
    let id2 = v
        .submit(
            &env(place(Side::Buy, 103, 90, 300_000_000, TimeInForce::Fok)),
            &b,
            ACK,
            &mut out,
        )
        .unwrap();
    v.on_book(&b, 2 * ACK, &mut out);
    assert_eq!(state(&v, id2), OrderState::Filled);
    assert_eq!(fills(&out).len(), 3);
}

#[test]
fn cancel_request_then_ack_after_latency() {
    let mut v = venue();
    let b = book_at(100, 101);
    let mut out = Vec::new();
    let id = v
        .submit(
            &env(place(Side::Buy, 99, 90, 50_000_000, TimeInForce::Gtc)),
            &b,
            0,
            &mut out,
        )
        .unwrap();
    v.on_book(&b, ACK, &mut out);
    v.submit(&env(Intent::Cancel { order_id: id }), &b, 2_000, &mut out)
        .unwrap();
    assert_eq!(state(&v, id), OrderState::PendingCancel);
    v.on_book(&b, 2_000 + CANCEL - 1, &mut out);
    assert_eq!(state(&v, id), OrderState::PendingCancel);
    v.on_book(&b, 2_000 + CANCEL, &mut out);
    let o = v.order(id).unwrap();
    assert_eq!(o.state, OrderState::Canceled);
    assert_eq!(o.reason, Some(CancelReason::Requested));
    assert_eq!(
        transitions(&out, id),
        vec![
            (OrderState::PendingNew, OrderState::Open),
            (OrderState::Open, OrderState::PendingCancel),
            (OrderState::PendingCancel, OrderState::Canceled),
        ]
    );
}

#[test]
fn fill_racing_cancel_wins_when_due_first() {
    let mut v = venue();
    let mut out = Vec::new();
    let id = v
        .submit(
            &env(place(Side::Buy, 99, 90, 50_000_000, TimeInForce::Gtc)),
            &book_at(100, 101),
            0,
            &mut out,
        )
        .unwrap();
    v.on_book(&book_at(100, 101), ACK, &mut out);
    let crossed = book_at(98, 99);
    v.on_book(&crossed, 5_000, &mut out); // fill due 5_500
    v.submit(
        &env(Intent::Cancel { order_id: id }),
        &crossed,
        5_100,
        &mut out,
    )
    .unwrap(); // cancel due 5_800
    v.on_book(&crossed, 6_000, &mut out);
    assert_eq!(state(&v, id), OrderState::Filled);
    assert_eq!(
        transitions(&out, id),
        vec![
            (OrderState::PendingNew, OrderState::Open),
            (OrderState::Open, OrderState::PendingCancel),
            (OrderState::PendingCancel, OrderState::Filled),
        ]
    );
}

#[test]
fn cancel_errors() {
    let mut v = venue();
    let b = book_at(100, 101);
    let mut out = Vec::new();
    assert_eq!(
        v.submit(&env(Intent::Cancel { order_id: 42 }), &b, 0, &mut out),
        Err(ExecError::UnknownOrder(42))
    );
    let id = v
        .submit(
            &env(place(Side::Buy, 99, 90, 50_000_000, TimeInForce::Gtc)),
            &b,
            0,
            &mut out,
        )
        .unwrap();
    assert_eq!(
        v.submit(&env(Intent::Cancel { order_id: id }), &b, 1, &mut out),
        Err(ExecError::NotOpen(id))
    );
}

#[test]
fn stop_arms_on_fill_triggers_at_touch_and_walks_depth() {
    let mut v = venue();
    let mut out = Vec::new();
    // Buy 2.5 BTC at 103 (fills 1@101, 1@102, 0.5@103), stop 95.
    let id = v
        .submit(
            &env(place(Side::Buy, 103, 95, 250_000_000, TimeInForce::Gtc)),
            &book_at(100, 101),
            0,
            &mut out,
        )
        .unwrap();
    v.on_book(&book_at(100, 101), ACK, &mut out);
    assert_eq!(state(&v, id), OrderState::Filled);
    let stop = v
        .orders()
        .find(|o| matches!(o.kind, exec::OrderKind::Stop { parent } if parent == id))
        .unwrap()
        .clone();
    assert_eq!(stop.side, Side::Sell);
    assert_eq!(stop.price, Price(95));
    assert_eq!(stop.qty, Qty(250_000_000));
    assert_eq!(stop.state, OrderState::PendingNew);
    // Bid at 96: not triggered.
    v.on_book(&book_at(96, 97), 2_000, &mut out);
    assert_eq!(state(&v, stop.id), OrderState::PendingNew);
    // Bid at 95: triggered, walks 3 BTC displayed, needs 2.5 -> not thin.
    v.on_book(&book_at(95, 96), 3_000, &mut out);
    assert_eq!(state(&v, stop.id), OrderState::Open);
    v.on_book(&book_at(95, 96), 3_000 + FILL, &mut out);
    let stop_fills: Vec<_> = fills(&out).into_iter().filter(|f| f.0 == stop.id).collect();
    assert_eq!(
        stop_fills,
        vec![
            (stop.id, 95, 100_000_000, false),
            (stop.id, 94, 100_000_000, false),
            (stop.id, 93, 50_000_000, false)
        ]
    );
    assert_eq!(state(&v, stop.id), OrderState::Filled);
    assert!(v.account().is_flat());
}

#[test]
fn thin_book_stop_rests_marketable_and_flags_fills() {
    let mut v = venue();
    let mut out = Vec::new();
    // Long 3 BTC.
    let id = v
        .submit(
            &env(place(Side::Buy, 103, 95, 300_000_000, TimeInForce::Gtc)),
            &book_at(100, 101),
            0,
            &mut out,
        )
        .unwrap();
    v.on_book(&book_at(100, 101), ACK, &mut out);
    assert_eq!(state(&v, id), OrderState::Filled);
    let stop_id = v
        .orders()
        .find(|o| matches!(o.kind, exec::OrderKind::Stop { .. }))
        .unwrap()
        .id;
    // Thin book: only 1 BTC on the bid side.
    let bids = vec![lv(95, 100_000_000)];
    let asks = vec![lv(96, 100_000_000)];
    let mut thin = Book::new(10);
    thin.apply(&BookEvent::Snapshot {
        checksum: checksum(&asks, &bids),
        bids,
        asks,
    })
    .unwrap();
    v.on_book(&thin, 3_000, &mut out);
    v.on_book(&thin, 3_000 + FILL, &mut out);
    let f: Vec<_> = fills(&out).into_iter().filter(|f| f.0 == stop_id).collect();
    assert_eq!(f, vec![(stop_id, 95, 100_000_000, true)]);
    assert_eq!(state(&v, stop_id), OrderState::PartiallyFilled);
    assert!(v.order(stop_id).unwrap().thin_book);
    // Next update shows 3 BTC again: remainder fills, still flagged.
    v.on_book(&book_at(94, 95), 4_000, &mut out);
    v.on_book(&book_at(94, 95), 4_000 + FILL, &mut out);
    let f: Vec<_> = fills(&out).into_iter().filter(|f| f.0 == stop_id).collect();
    assert_eq!(f.len(), 3);
    assert!(f.iter().all(|f| f.3));
    assert_eq!(state(&v, stop_id), OrderState::Filled);
    assert!(v.account().is_flat());
}

#[test]
fn stops_cancelled_when_flat() {
    let mut v = venue();
    let mut out = Vec::new();
    v.submit(
        &env(place(Side::Buy, 101, 95, 100_000_000, TimeInForce::Gtc)),
        &book_at(100, 101),
        0,
        &mut out,
    )
    .unwrap();
    v.on_book(&book_at(100, 101), ACK, &mut out);
    let stop_id = v
        .orders()
        .find(|o| matches!(o.kind, exec::OrderKind::Stop { .. }))
        .unwrap()
        .id;
    // Sell 1 BTC at 100 to go flat.
    v.submit(
        &env(place(Side::Sell, 100, 105, 100_000_000, TimeInForce::Gtc)),
        &book_at(100, 101),
        ACK,
        &mut out,
    )
    .unwrap();
    v.on_book(&book_at(100, 101), 2 * ACK, &mut out);
    assert!(v.account().is_flat());
    let stop = v.order(stop_id).unwrap();
    assert_eq!(stop.state, OrderState::Canceled);
    assert_eq!(stop.reason, Some(CancelReason::PositionFlat));
    assert_eq!(v.orders().filter(|o| !o.is_terminal()).count(), 0);
}

#[test]
fn flatten_cancels_open_orders_and_sends_ioc_at_worst_level() {
    let mut v = venue();
    let b = book_at(100, 101);
    let mut out = Vec::new();
    v.submit(
        &env(place(Side::Buy, 102, 95, 150_000_000, TimeInForce::Gtc)),
        &b,
        0,
        &mut out,
    )
    .unwrap();
    let resting = v
        .submit(
            &env(place(Side::Buy, 90, 80, 100_000_000, TimeInForce::Gtc)),
            &b,
            0,
            &mut out,
        )
        .unwrap();
    v.on_book(&b, ACK, &mut out);
    assert_eq!(v.account().position.net_qty, Qty(150_000_000));
    let flat_id = v
        .submit(&env(Intent::Flatten), &b, 2_000, &mut out)
        .unwrap();
    assert_eq!(state(&v, resting), OrderState::PendingCancel);
    assert_eq!(
        v.order(resting).unwrap().reason,
        Some(CancelReason::Flatten)
    );
    let f = v.order(flat_id).unwrap();
    assert_eq!(
        (f.side, f.price, f.qty, f.tif),
        (Side::Sell, Price(98), Qty(150_000_000), TimeInForce::Ioc)
    );
    v.on_book(&b, 2_000 + ACK, &mut out);
    assert!(v.account().is_flat());
    assert_eq!(state(&v, flat_id), OrderState::Filled);
    assert_eq!(v.open_orders(), 0);
    assert_eq!(
        v.submit(&env(Intent::Flatten), &b, 5_000, &mut out),
        Err(ExecError::Flat)
    );
}

#[test]
fn illegal_transition_is_rejected_by_table() {
    use OrderState::*;
    assert!(Order::can_transition(PendingNew, Open));
    assert!(Order::can_transition(PendingCancel, Filled));
    assert!(!Order::can_transition(Filled, Open));
    assert!(!Order::can_transition(Canceled, PartiallyFilled));
    assert!(!Order::can_transition(Open, PendingNew));
    assert!(!Order::can_transition(Rejected, Canceled));
}

#[test]
fn position_events_follow_fills_and_marks() {
    let mut v = venue();
    let b = book_at(100, 101);
    let mut out = Vec::new();
    v.submit(
        &env(place(Side::Buy, 101, 95, 100_000_000, TimeInForce::Gtc)),
        &b,
        0,
        &mut out,
    )
    .unwrap();
    v.on_book(&b, ACK, &mut out);
    let last = out
        .iter()
        .rev()
        .find_map(|e| match e {
            ExecEvent::Position {
                position, equity, ..
            } => Some((*position, *equity)),
            _ => None,
        })
        .unwrap();
    assert_eq!(last.0.net_qty, Qty(100_000_000));
    // mid 100, entry 101: unrealized -1.0 -> equity 999_999
    assert_eq!(last.1, 999_999);
}

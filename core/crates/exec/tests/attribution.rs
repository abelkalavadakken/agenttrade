//! Fills attribute to strategy_id; take-profit exits; flatten per strategy and all.

use book::Book;
use exec::{CancelReason, ExecError, ExecEvent, OrderKind, PaperConfig, PaperVenue};
use types::{BookEvent, Intent, IntentEnvelope, Level, OrderState, Price, Qty, Side, TimeInForce};

const ACK: i64 = 1_000;

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
            fill_latency_ns: 500,
            cancel_latency_ns: 700,
        },
        1_000_000,
        8,
    )
}

fn env(agent: &str, intent: Intent) -> IntentEnvelope {
    IntentEnvelope {
        intent_id: "i".into(),
        agent_id: agent.into(),
        source_sequence_id: 1,
        generated_time_ns: 0,
        venue: "kraken".into(),
        symbol: "BTC/USD".into(),
        intent,
    }
}

fn place(side: Side, price: i64, stop: i64, take_profit: i64, qty: i64) -> Intent {
    Intent::Place {
        side,
        price: Price(price),
        stop: Price(stop),
        take_profit: Price(take_profit),
        qty: Qty(qty),
        tif: TimeInForce::Gtc,
    }
}

fn fills_of(out: &[ExecEvent], strategy: &str) -> Vec<(i64, i64)> {
    out.iter()
        .filter_map(|e| match e {
            ExecEvent::Fill {
                strategy_id,
                price,
                qty,
                ..
            } if strategy_id == strategy => Some((price.0, qty.0)),
            _ => None,
        })
        .collect()
}

#[test]
fn fills_attribute_to_each_strategy_and_the_account() {
    let mut v = venue();
    let b = book_at(100, 101);
    let mut out = Vec::new();
    v.submit(
        &env("mr_ofi", place(Side::Buy, 101, 90, 0, 100_000_000)),
        &b,
        0,
        &mut out,
    )
    .unwrap();
    v.submit(
        &env("discretionary", place(Side::Sell, 100, 110, 0, 50_000_000)),
        &b,
        0,
        &mut out,
    )
    .unwrap();
    v.on_book(&b, ACK, &mut out);
    assert_eq!(fills_of(&out, "mr_ofi"), vec![(101, 100_000_000)]);
    assert_eq!(fills_of(&out, "discretionary"), vec![(100, 50_000_000)]);
    assert_eq!(v.strategy_position("mr_ofi").net_qty, Qty(100_000_000));
    assert_eq!(
        v.strategy_position("discretionary").net_qty,
        Qty(-50_000_000)
    );
    assert_eq!(v.account().position.net_qty, Qty(50_000_000));
    assert_eq!(v.strategies().len(), 2);
    assert_eq!(v.strategy_position("nobody"), Default::default());
}

#[test]
fn take_profit_rests_as_an_exit_and_cancels_the_stop_when_hit() {
    let mut v = venue();
    let mut out = Vec::new();
    let id = v
        .submit(
            &env("s", place(Side::Buy, 101, 95, 105, 100_000_000)),
            &book_at(100, 101),
            0,
            &mut out,
        )
        .unwrap();
    v.on_book(&book_at(100, 101), ACK, &mut out);
    let exit = v
        .orders()
        .find(|o| o.kind == OrderKind::Exit { parent: id })
        .unwrap()
        .clone();
    let stop = v
        .orders()
        .find(|o| o.kind == OrderKind::Stop { parent: id })
        .unwrap()
        .clone();
    assert_eq!(
        (exit.side, exit.price, exit.qty),
        (Side::Sell, Price(105), Qty(100_000_000))
    );
    assert_eq!(exit.strategy_id, "s");
    // The exit acks and rests.
    v.on_book(&book_at(100, 101), 2 * ACK, &mut out);
    assert_eq!(v.order(exit.id).unwrap().state, OrderState::Open);
    // Bid reaches 105: the exit fills after fill latency, the strategy is flat, the stop goes.
    v.on_book(&book_at(105, 106), 3_000, &mut out);
    v.on_book(&book_at(105, 106), 3_500, &mut out);
    assert_eq!(v.order(exit.id).unwrap().state, OrderState::Filled);
    assert!(v.strategy_position("s").net_qty.is_zero());
    assert_eq!(
        v.strategy_position("s").realized_pnl,
        4,
        "1 BTC times 4 ticks"
    );
    let stop = v.order(stop.id).unwrap();
    assert_eq!(stop.state, OrderState::Canceled);
    assert_eq!(stop.reason, Some(CancelReason::PositionFlat));
}

#[test]
fn flatten_targets_only_the_callers_strategy() {
    let mut v = venue();
    let b = book_at(100, 101);
    let mut out = Vec::new();
    v.submit(
        &env("a", place(Side::Buy, 101, 90, 0, 100_000_000)),
        &b,
        0,
        &mut out,
    )
    .unwrap();
    v.submit(
        &env("b", place(Side::Buy, 101, 90, 0, 100_000_000)),
        &b,
        0,
        &mut out,
    )
    .unwrap();
    v.on_book(&b, ACK, &mut out);
    let id = v
        .submit(&env("a", Intent::Flatten), &b, 2_000, &mut out)
        .unwrap();
    v.on_book(&b, 2_000 + ACK, &mut out);
    assert_eq!(v.order(id).unwrap().strategy_id, "a");
    assert!(v.strategy_position("a").net_qty.is_zero());
    assert_eq!(v.strategy_position("b").net_qty, Qty(100_000_000));
    assert_eq!(v.account().position.net_qty, Qty(100_000_000));
    // b's stop still stands; a's is cancelled.
    let stops: Vec<_> = v
        .orders()
        .filter(|o| matches!(o.kind, OrderKind::Stop { .. }))
        .map(|o| (o.strategy_id.clone(), o.state))
        .collect();
    assert!(stops.contains(&("a".to_string(), OrderState::Canceled)));
    assert!(stops.contains(&("b".to_string(), OrderState::PendingNew)));
}

#[test]
fn flatten_all_cancels_everything_and_flattens_every_strategy() {
    let mut v = venue();
    let b = book_at(100, 101);
    let mut out = Vec::new();
    v.submit(
        &env("a", place(Side::Buy, 101, 90, 0, 100_000_000)),
        &b,
        0,
        &mut out,
    )
    .unwrap();
    v.submit(
        &env("b", place(Side::Sell, 100, 110, 0, 50_000_000)),
        &b,
        0,
        &mut out,
    )
    .unwrap();
    let resting = v
        .submit(
            &env("c", place(Side::Buy, 90, 80, 0, 10_000_000)),
            &b,
            0,
            &mut out,
        )
        .unwrap();
    v.on_book(&b, ACK, &mut out);
    v.submit(&env("operator", Intent::FlattenAll), &b, 2_000, &mut out)
        .unwrap();
    assert_eq!(v.order(resting).unwrap().state, OrderState::PendingCancel);
    v.on_book(&b, 2_000 + ACK, &mut out);
    assert!(v.strategies().values().all(|p| p.net_qty.is_zero()));
    assert!(v.account().is_flat());
    assert_eq!(v.open_orders(), 0);
    assert_eq!(
        v.orders().filter(|o| !o.is_terminal()).count(),
        0,
        "no protective orders survive"
    );
    assert!(matches!(
        v.submit(&env("operator", Intent::FlattenAll), &b, 5_000, &mut out),
        Ok(0)
    ));
}

#[test]
fn open_orders_of_counts_one_strategy() {
    let mut v = venue();
    let b = book_at(100, 101);
    let mut out = Vec::new();
    v.submit(
        &env("a", place(Side::Buy, 90, 80, 0, 100_000_000)),
        &b,
        0,
        &mut out,
    )
    .unwrap();
    v.submit(
        &env("a", place(Side::Buy, 89, 80, 0, 100_000_000)),
        &b,
        0,
        &mut out,
    )
    .unwrap();
    v.submit(
        &env("b", place(Side::Buy, 88, 80, 0, 100_000_000)),
        &b,
        0,
        &mut out,
    )
    .unwrap();
    v.on_book(&b, ACK, &mut out);
    assert_eq!(v.open_orders_of("a"), 2);
    assert_eq!(v.open_orders_of("b"), 1);
    assert_eq!(v.open_orders(), 3);
}

#[test]
fn overlapping_flattens_do_not_flip_the_position() {
    let mut v = venue();
    let b = book_at(100, 101);
    let mut out = Vec::new();
    v.submit(
        &env("a", place(Side::Buy, 101, 90, 0, 100_000_000)),
        &b,
        0,
        &mut out,
    )
    .unwrap();
    v.on_book(&b, ACK, &mut out);
    assert_eq!(v.strategy_position("a").net_qty, Qty(100_000_000));
    // Three flattens before the first one acks: one IOC, two "nothing to flatten".
    let first = v.submit(&env("a", Intent::Flatten), &b, 2_000, &mut out);
    assert!(first.is_ok());
    assert_eq!(
        v.submit(&env("a", Intent::Flatten), &b, 2_001, &mut out),
        Err(ExecError::Flat)
    );
    assert_eq!(
        v.submit(&env("a", Intent::Flatten), &b, 2_002, &mut out),
        Err(ExecError::Flat)
    );
    v.on_book(&b, 2_000 + ACK, &mut out);
    assert!(
        v.strategy_position("a").net_qty.is_zero(),
        "flat, not flipped"
    );
    assert_eq!(v.orders().filter(|o| o.tif == TimeInForce::Ioc).count(), 1);
}

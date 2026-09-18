use book::Book;
use risk::{check, RiskConfig, RiskInputs, Verdict};
use types::{
    instruments, BookEvent, Instrument, Intent, IntentEnvelope, Level, Position, Price, Qty,
    RejectionCode, Side, TimeInForce,
};

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

/// Best bid 77362.8, best ask 77362.9, mid rounds down to 77362.8.
fn book() -> Book {
    let bids = vec![lv(773_628, 50_000_000), lv(773_627, 10_000_000)];
    let asks = vec![lv(773_629, 50_000_000), lv(773_630, 10_000_000)];
    let mut b = Book::new(10);
    b.apply(&BookEvent::Snapshot {
        checksum: checksum(&asks, &bids),
        bids,
        asks,
    })
    .unwrap();
    b
}

fn cfg() -> RiskConfig {
    RiskConfig {
        max_sequence_drift: 100,
        price_band_bps: 50,
        risk_per_trade_bps: 100,
        max_leverage_bps: 10_000,
        max_open_orders: 5,
        max_intents_per_window: 10,
        window_ns: 60_000_000_000,
        max_drawdown_bps: 1_000,
    }
}

struct Fixture {
    instrument: Instrument,
    book: Book,
}

impl Fixture {
    fn new() -> Self {
        Self {
            instrument: instruments::find("kraken", "BTC/USD").unwrap(),
            book: book(),
        }
    }

    fn inputs(&self) -> RiskInputs<'_> {
        RiskInputs {
            instrument: &self.instrument,
            book: &self.book,
            book_sequence_id: 1_000,
            venue_connected: true,
            instrument_halted: false,
            position: Position::default(),
            equity: 1_000_000,
            peak_equity: 1_000_000,
            open_orders: 0,
            intents_in_window: 0,
            kill_switch: false,
            now_ns: 0,
        }
    }
}

fn env(intent: Intent) -> IntentEnvelope {
    IntentEnvelope {
        intent_id: "i1".into(),
        agent_id: "trader".into(),
        source_sequence_id: 1_000,
        generated_time_ns: 0,
        venue: "kraken".into(),
        symbol: "BTC/USD".into(),
        intent,
    }
}

/// Buy 0.5 BTC at the ask with a 362.9 USD stop: loss 181 USD on a 1,000 USD budget.
fn good_place() -> Intent {
    Intent::Place {
        side: Side::Buy,
        price: Price(773_629),
        stop: Price(770_000),
        take_profit: Price::ZERO,
        qty: Qty(50_000_000),
        tif: TimeInForce::Gtc,
    }
}

fn place_with(f: impl FnOnce(&mut Intent)) -> Intent {
    let mut i = good_place();
    f(&mut i);
    i
}

fn code(v: Verdict) -> RejectionCode {
    v.code()
}

#[test]
fn good_place_is_approved() {
    let f = Fixture::new();
    assert_eq!(
        check(&cfg(), &f.inputs(), &env(good_place())),
        Verdict::Approved
    );
}

#[test]
fn cancel_flatten_noop_need_only_market_state() {
    let f = Fixture::new();
    for i in [
        Intent::Cancel { order_id: 7 },
        Intent::Flatten,
        Intent::Noop,
    ] {
        assert_eq!(check(&cfg(), &f.inputs(), &env(i)), Verdict::Approved);
    }
}

#[test]
fn kill_switch_manual() {
    let f = Fixture::new();
    let mut inp = f.inputs();
    inp.kill_switch = true;
    assert_eq!(
        code(check(&cfg(), &inp, &env(Intent::Noop))),
        RejectionCode::KillSwitchActive
    );
}

#[test]
fn kill_switch_drawdown() {
    let f = Fixture::new();
    let mut inp = f.inputs();
    inp.equity = 900_000; // exactly 10% down: allowed
    assert_eq!(check(&cfg(), &inp, &env(Intent::Noop)), Verdict::Approved);
    inp.equity = 899_999;
    assert_eq!(
        code(check(&cfg(), &inp, &env(Intent::Noop))),
        RejectionCode::KillSwitchActive
    );
}

#[test]
fn unknown_instrument_is_invalid_intent() {
    let f = Fixture::new();
    let mut e = env(Intent::Noop);
    e.symbol = "ETH/USD".into();
    assert_eq!(
        code(check(&cfg(), &f.inputs(), &e)),
        RejectionCode::InvalidIntent
    );
}

#[test]
fn venue_disconnected_and_stale_book() {
    let f = Fixture::new();
    let mut inp = f.inputs();
    inp.venue_connected = false;
    assert_eq!(
        code(check(&cfg(), &inp, &env(Intent::Noop))),
        RejectionCode::VenueDisconnected
    );

    let mut stale = Fixture::new();
    stale.book.mark_stale();
    assert_eq!(
        code(check(&cfg(), &stale.inputs(), &env(Intent::Noop))),
        RejectionCode::VenueDisconnected
    );
    let empty = Fixture {
        instrument: f.instrument.clone(),
        book: Book::new(10),
    };
    assert_eq!(
        code(check(&cfg(), &empty.inputs(), &env(Intent::Noop))),
        RejectionCode::VenueDisconnected
    );
}

#[test]
fn instrument_halted() {
    let f = Fixture::new();
    let mut inp = f.inputs();
    inp.instrument_halted = true;
    assert_eq!(
        code(check(&cfg(), &inp, &env(Intent::Noop))),
        RejectionCode::InstrumentHalted
    );
}

#[test]
fn stale_state_too_old_and_ahead() {
    let f = Fixture::new();
    let mut e = env(Intent::Noop);
    e.source_sequence_id = 900; // drift 100, allowed
    assert_eq!(check(&cfg(), &f.inputs(), &e), Verdict::Approved);
    e.source_sequence_id = 899;
    assert_eq!(
        code(check(&cfg(), &f.inputs(), &e)),
        RejectionCode::StaleState
    );
    e.source_sequence_id = 1_001;
    assert_eq!(
        code(check(&cfg(), &f.inputs(), &e)),
        RejectionCode::StaleState
    );
}

#[test]
fn tick_and_lot_alignment() {
    let mut coarse = Fixture::new();
    coarse.instrument.tick = Price(5);
    coarse.instrument.lot = Qty(1_000);
    let i = coarse.inputs();
    let off_tick = place_with(|p| {
        if let Intent::Place { price, .. } = p {
            *price = Price(773_631)
        }
    });
    assert_eq!(
        code(check(&cfg(), &i, &env(off_tick))),
        RejectionCode::InvalidTickSize
    );
    let off_lot = place_with(|p| {
        if let Intent::Place { price, qty, .. } = p {
            *price = Price(773_630);
            *qty = Qty(50_000_001)
        }
    });
    assert_eq!(
        code(check(&cfg(), &i, &env(off_lot))),
        RejectionCode::InvalidTickSize
    );
    let zero = place_with(|p| {
        if let Intent::Place { qty, .. } = p {
            *qty = Qty(0)
        }
    });
    assert_eq!(
        code(check(&cfg(), &coarse.inputs(), &env(zero))),
        RejectionCode::InvalidTickSize
    );
}

#[test]
fn price_out_of_band() {
    let f = Fixture::new();
    // 50 bps of 77362.8 is 386.8; 386 up is fine, 387 is not.
    let ok = place_with(|p| {
        if let Intent::Place { price, .. } = p {
            *price = Price(773_628 + 3_868)
        }
    });
    assert_eq!(check(&cfg(), &f.inputs(), &env(ok)), Verdict::Approved);
    let far = place_with(|p| {
        if let Intent::Place { price, .. } = p {
            *price = Price(773_628 + 3_869)
        }
    });
    assert_eq!(
        code(check(&cfg(), &f.inputs(), &env(far))),
        RejectionCode::PriceOutOfBand
    );
}

#[test]
fn missing_stop_and_wrong_side_stop() {
    let f = Fixture::new();
    let none = place_with(|p| {
        if let Intent::Place { stop, .. } = p {
            *stop = Price(0)
        }
    });
    assert_eq!(
        code(check(&cfg(), &f.inputs(), &env(none))),
        RejectionCode::MissingStop
    );
    let above = place_with(|p| {
        if let Intent::Place { stop, .. } = p {
            *stop = Price(773_629)
        }
    });
    assert_eq!(
        code(check(&cfg(), &f.inputs(), &env(above))),
        RejectionCode::InvalidIntent
    );
    let sell_below = Intent::Place {
        side: Side::Sell,
        price: Price(773_628),
        stop: Price(773_000),
        take_profit: Price::ZERO,
        qty: Qty(10_000_000),
        tif: TimeInForce::Gtc,
    };
    assert_eq!(
        code(check(&cfg(), &f.inputs(), &env(sell_below))),
        RejectionCode::InvalidIntent
    );
}

#[test]
fn one_percent_rule_boundary() {
    let f = Fixture::new();
    // budget 1,000 USD = 10_000 at scale 1. stop distance 362.9 -> 3629 ticks.
    // qty * 3629 * 10_000 <= 1e16  ->  qty <= 275_558_004.
    let at = place_with(|p| {
        if let Intent::Place { qty, .. } = p {
            *qty = Qty(275_558_004)
        }
    });
    let over = place_with(|p| {
        if let Intent::Place { qty, .. } = p {
            *qty = Qty(275_558_005)
        }
    });
    // Leverage would also trip at 2.75 BTC (213k USD notional on 100k equity), so widen it.
    let mut c = cfg();
    c.max_leverage_bps = 30_000;
    assert_eq!(check(&c, &f.inputs(), &env(at)), Verdict::Approved);
    assert_eq!(
        code(check(&c, &f.inputs(), &env(over))),
        RejectionCode::ExceedsSingleLossLimit
    );
}

#[test]
fn one_percent_rule_overflow_rejects() {
    let f = Fixture::new();
    let huge = place_with(|p| {
        if let Intent::Place { qty, stop, .. } = p {
            *qty = Qty(i64::MAX / 2);
            *stop = Price(1)
        }
    });
    let v = check(&cfg(), &f.inputs(), &env(huge));
    assert_eq!(code(v), RejectionCode::ExceedsSingleLossLimit);
    assert!(matches!(
        v,
        Verdict::Rejected {
            reason: "overflow",
            ..
        }
    ));
}

#[test]
fn leverage_measured_on_new_absolute_net() {
    let f = Fixture::new();
    let mut inp = f.inputs();
    // Long 0.5 BTC. Selling 1.8 BTC flips to short 1.3 BTC = 100.5k notional > 100k equity.
    inp.position.net_qty = Qty(50_000_000);
    let flip = Intent::Place {
        side: Side::Sell,
        price: Price(773_628),
        stop: Price(780_000),
        take_profit: Price::ZERO,
        qty: Qty(180_000_000),
        tif: TimeInForce::Gtc,
    };
    let mut c = cfg();
    c.risk_per_trade_bps = 10_000; // isolate leverage
    assert_eq!(
        code(check(&c, &inp, &env(flip))),
        RejectionCode::ExceedsMaxLeverage
    );
    // Selling 0.5 BTC flattens: notional 0.
    let flat = Intent::Place {
        side: Side::Sell,
        price: Price(773_628),
        stop: Price(780_000),
        take_profit: Price::ZERO,
        qty: Qty(50_000_000),
        tif: TimeInForce::Gtc,
    };
    assert_eq!(check(&c, &inp, &env(flat)), Verdict::Approved);
}

#[test]
fn rate_limits() {
    let f = Fixture::new();
    let mut inp = f.inputs();
    inp.open_orders = 5;
    assert_eq!(
        code(check(&cfg(), &inp, &env(good_place()))),
        RejectionCode::RateLimitExceeded
    );
    assert_eq!(
        check(&cfg(), &inp, &env(Intent::Flatten)),
        Verdict::Approved
    );
    inp.open_orders = 0;
    inp.intents_in_window = 10;
    assert_eq!(
        code(check(&cfg(), &inp, &env(Intent::Flatten))),
        RejectionCode::RateLimitExceeded
    );
    assert_eq!(
        check(&cfg(), &inp, &env(Intent::Cancel { order_id: 1 })),
        Verdict::Approved
    );
}

#[test]
fn first_failing_check_wins() {
    let f = Fixture::new();
    let mut inp = f.inputs();
    inp.kill_switch = true;
    inp.venue_connected = false;
    inp.open_orders = 99;
    let bad = place_with(|p| {
        if let Intent::Place { stop, .. } = p {
            *stop = Price(0)
        }
    });
    assert_eq!(
        code(check(&cfg(), &inp, &env(bad.clone()))),
        RejectionCode::KillSwitchActive
    );
    inp.kill_switch = false;
    assert_eq!(
        code(check(&cfg(), &inp, &env(bad.clone()))),
        RejectionCode::VenueDisconnected
    );
    inp.venue_connected = true;
    assert_eq!(
        code(check(&cfg(), &inp, &env(bad))),
        RejectionCode::MissingStop
    );
}

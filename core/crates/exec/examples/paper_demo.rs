//! Replays a tape through tracker, book, risk gate and paper venue with a
//! scripted set of intents. Run: cargo run --release -p exec --example paper_demo -- <tape>

use std::fs::File;

use book::Book;
use exec::{ExecEvent, PaperConfig, PaperVenue};
use feed::kraken::Tracker;
use risk::{RiskConfig, RiskInputs, Verdict};
use tape::{Mode, Reader};
use types::{instruments, FeedEvent, Intent, IntentEnvelope, Price, Qty, Side, TimeInForce};

const USD: i64 = 10; // price_scale 1
const BTC: i64 = 100_000_000; // qty_scale 8

fn main() {
    let path = std::env::args().nth(1).expect("tape path");
    let inst = instruments::find("kraken", "BTC/USD").unwrap();
    let mut tracker = Tracker::new(inst.clone(), 10);
    let mut venue = PaperVenue::new(
        PaperConfig {
            ack_latency_ns: 50_000_000,
            fill_latency_ns: 20_000_000,
            cancel_latency_ns: 30_000_000,
        },
        100_000 * USD,
        8,
    );
    let cfg = RiskConfig {
        max_sequence_drift: 1_000,
        price_band_bps: 50,
        risk_per_trade_bps: 100,
        max_leverage_bps: 20_000,
        max_open_orders: 5,
        max_intents_per_window: 100,
        window_ns: 60_000_000_000,
        max_drawdown_bps: 1_000,
        max_strategies_enabled: 2,
        max_size_multiplier_bps: 20_000,
        discretionary_max_qty: Qty(100 * BTC),
        discretionary_max_intents_per_window: 100,
    };
    let mut reader = Reader::new(File::open(&path).unwrap(), Mode::Fast).unwrap();
    let mut seq = 0u64;
    let mut t0 = None;
    let mut out = Vec::new();
    let mut fired = [false; 6];
    let mut rejected = 0;
    let mut approved = 0;

    while let Ok(Some(rec)) = reader.next_record() {
        if rec.source_id != 0 {
            if rec.bytes == b"connected" {
                tracker.expect_snapshot();
            }
            continue;
        }
        let handled = tracker.on_frame(&rec.bytes);
        if !handled
            .events
            .iter()
            .any(|e| matches!(e, FeedEvent::Book(_)))
        {
            continue;
        }
        seq += 1;
        let t0 = *t0.get_or_insert(rec.recv_ns);
        let secs = (rec.recv_ns - t0) / 1_000_000_000;
        let book: &Book = tracker.book();

        let script: [(i64, &str, Intent); 6] = [
            (
                10,
                "buy 0.05 at ask, no stop",
                place(Side::Buy, ask(book), 0, 5 * BTC / 100),
            ),
            (
                11,
                "buy 0.05 at ask+2%, stop 1% below",
                place(
                    Side::Buy,
                    ask(book).0 * 102 / 100,
                    ask(book).0 * 99 / 100,
                    5 * BTC / 100,
                ),
            ),
            (
                12,
                "buy 20 BTC at ask, stop 1% below",
                place(Side::Buy, ask(book), ask(book).0 * 99 / 100, 20 * BTC),
            ),
            (
                13,
                "buy 0.05 at ask, stop 1% below",
                place(Side::Buy, ask(book), ask(book).0 * 99 / 100, 5 * BTC / 100),
            ),
            (
                120,
                "buy 0.05 at bid-0.5 (rests), stop 1% below",
                place(
                    Side::Buy,
                    bid(book).0 - 5,
                    bid(book).0 * 99 / 100,
                    5 * BTC / 100,
                ),
            ),
            (480, "flatten", Intent::Flatten),
        ];
        for (i, (at, label, intent)) in script.into_iter().enumerate() {
            if fired[i] || secs < at {
                continue;
            }
            fired[i] = true;
            let env = IntentEnvelope {
                intent_id: format!("i{i}"),
                agent_id: "demo".into(),
                source_sequence_id: seq,
                generated_time_ns: rec.recv_ns,
                venue: "kraken".into(),
                symbol: "BTC/USD".into(),
                intent,
            };
            let acct = venue.account();
            let mark = book.mid().unwrap();
            let inputs = RiskInputs {
                instrument: &inst,
                book,
                book_sequence_id: seq,
                venue_connected: true,
                instrument_halted: false,
                position: acct.position,
                equity: acct.equity(mark),
                peak_equity: acct.peak_equity,
                open_orders: venue.open_orders() as u32,
                intents_in_window: 0,
                discretionary_intents_in_window: 0,
                kill_switch: false,
                now_ns: rec.recv_ns,
                strategies: &[],
            };
            println!("t+{secs:>4}s  intent   {label}");
            match risk::check(&cfg, &inputs, &env) {
                Verdict::Approved => {
                    approved += 1;
                    let id = venue.submit(&env, book, rec.recv_ns, &mut out).unwrap();
                    println!("         APPROVED -> order {id}");
                }
                Verdict::Rejected { code, reason } => {
                    rejected += 1;
                    println!("         REJECTED {code:?}: {reason}");
                }
            }
        }
        venue.on_book(book, rec.recv_ns, &mut out);
        for e in out.drain(..) {
            match e {
                ExecEvent::Transition {
                    order_id,
                    from,
                    to,
                    reason,
                    ..
                } => {
                    let r = reason.map(|r| format!(" ({r:?})")).unwrap_or_default();
                    println!("t+{secs:>4}s  order {order_id}  {from:?} -> {to:?}{r}");
                }
                ExecEvent::Fill {
                    order_id,
                    side,
                    price,
                    qty,
                    thin_book,
                    ..
                } => {
                    let thin = if thin_book { "  THIN BOOK" } else { "" };
                    println!(
                        "t+{secs:>4}s  fill     order {order_id} {side:?} {} BTC @ {}{thin}",
                        fmt_qty(qty),
                        fmt_px(price)
                    );
                }
                ExecEvent::Position { .. } => {}
            }
        }
    }
    let acct = venue.account();
    let mark = tracker.book().mid().unwrap();
    println!();
    println!("frames applied     {seq}");
    println!(
        "intents            {} approved, {rejected} rejected",
        approved
    );
    println!("position           {} BTC", fmt_qty(acct.position.net_qty));
    println!(
        "realized pnl       {} USD",
        fmt_px(Price(acct.position.realized_pnl))
    );
    println!(
        "equity             {} USD (start 100000.0)",
        fmt_px(Price(acct.equity(mark)))
    );
    println!("peak equity        {} USD", fmt_px(Price(acct.peak_equity)));
    println!("no fees, queue position not modelled");
}

fn place(side: Side, price: impl Into<Px>, stop: i64, qty: i64) -> Intent {
    let price = price.into().0;
    Intent::Place {
        side,
        price: Price(price),
        stop: Price(stop),
        take_profit: Price(price * 101 / 100),
        qty: Qty(qty),
        tif: TimeInForce::Gtc,
    }
}

struct Px(i64);
impl From<Price> for Px {
    fn from(p: Price) -> Self {
        Px(p.0)
    }
}
impl From<i64> for Px {
    fn from(p: i64) -> Self {
        Px(p)
    }
}

fn ask(b: &Book) -> Price {
    b.best_ask().unwrap().price
}
fn bid(b: &Book) -> Price {
    b.best_bid().unwrap().price
}
fn fmt_px(p: Price) -> String {
    format!("{}.{}", p.0 / USD, (p.0 % USD).abs())
}
fn fmt_qty(q: Qty) -> String {
    format!("{}.{:08}", q.0 / BTC, (q.0 % BTC).abs())
}

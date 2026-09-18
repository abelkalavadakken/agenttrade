//! Same frames, same scripted intents, same fills and position, twice over.

use exec::{ExecEvent, PaperConfig, PaperVenue};
use feed::kraken::Tracker;
use types::{instruments, FeedEvent, Intent, IntentEnvelope, Price, Qty, Side, TimeInForce};

const FIXTURE: &str = include_str!("../../../../tests/fixtures/kraken_book_btcusd.jsonl");

fn run() -> (Vec<ExecEvent>, u64) {
    let inst = instruments::find("kraken", "BTC/USD").unwrap();
    let mut tracker = Tracker::new(inst, 10);
    let mut venue = PaperVenue::new(
        PaperConfig {
            ack_latency_ns: 50_000_000,
            fill_latency_ns: 20_000_000,
            cancel_latency_ns: 30_000_000,
        },
        10_000_000,
        8,
    );
    let mut out = Vec::new();
    let mut frames_applied = 0u64;
    for (i, line) in FIXTURE.lines().filter(|l| !l.is_empty()).enumerate() {
        let ns = i as i64 * 100_000_000;
        let handled = tracker.on_frame(line.as_bytes());
        let book_updated = handled
            .events
            .iter()
            .any(|e| matches!(e, FeedEvent::Book(_)));
        if !book_updated {
            continue;
        }
        frames_applied += 1;
        let book = tracker.book();
        // Script: buy 0.01 at the ask on frame 5, flatten on frame 40.
        if frames_applied == 5 {
            let ask = book.best_ask().unwrap().price;
            let intent = Intent::Place {
                side: Side::Buy,
                price: ask,
                stop: Price(ask.0 - 10_000),
                take_profit: Price::ZERO,
                qty: Qty(1_000_000),
                tif: TimeInForce::Gtc,
            };
            venue.submit(&env(intent), book, ns, &mut out).unwrap();
        }
        if frames_applied == 40 {
            venue
                .submit(&env(Intent::Flatten), book, ns, &mut out)
                .unwrap();
        }
        venue.on_book(book, ns, &mut out);
    }
    (out, frames_applied)
}

fn env(intent: Intent) -> IntentEnvelope {
    IntentEnvelope {
        intent_id: "script".into(),
        agent_id: "test".into(),
        source_sequence_id: 0,
        generated_time_ns: 0,
        venue: "kraken".into(),
        symbol: "BTC/USD".into(),
        intent,
    }
}

#[test]
fn replay_twice_is_identical_and_produces_a_round_trip() {
    let (a, frames) = run();
    let (b, _) = run();
    assert_eq!(a, b);
    assert_eq!(frames, 78, "fills below came from this many book frames");
    let fills: Vec<_> = a
        .iter()
        .filter(|e| matches!(e, ExecEvent::Fill { .. }))
        .collect();
    assert_eq!(fills.len(), 2, "one entry fill, one flatten fill");
    let last = a
        .iter()
        .rev()
        .find_map(|e| match e {
            ExecEvent::Position { position, .. } => Some(*position),
            _ => None,
        })
        .unwrap();
    assert!(last.net_qty.is_zero());
}

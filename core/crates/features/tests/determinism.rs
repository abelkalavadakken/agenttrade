//! Two runs over the same frames give bit-identical feature streams.

use features::{Features, FeaturesConfig};
use feed::kraken::Tracker;
use types::{instruments, FeedEvent};

const BOOK: &str = include_str!("../../../../tests/fixtures/kraken_book_btcusd.jsonl");
const TRADES: &str = include_str!("../../../../tests/fixtures/kraken_trade_btcusd.jsonl");

fn run() -> (Vec<String>, u64) {
    let mut tracker = Tracker::new(instruments::find("kraken", "BTC/USD").unwrap(), 10);
    let mut f = Features::new(FeaturesConfig::default());
    let mut stream = Vec::new();
    let mut events = 0;
    // Interleave book and trade frames; spread them over four minutes of tape time.
    let lines: Vec<&str> = BOOK
        .lines()
        .chain(TRADES.lines())
        .filter(|l| !l.is_empty())
        .collect();
    for (i, line) in lines.iter().enumerate() {
        let ns = i as i64 * 2_000_000_000;
        for e in tracker.on_frame(line.as_bytes()).events {
            events += 1;
            match e {
                FeedEvent::Book(_) => f.on_book(tracker.book(), ns),
                FeedEvent::Trade(t) => f.on_trade(&t, ns),
                FeedEvent::Resync(_) => f.on_stale(),
            }
            let s = f.snapshot();
            stream.push(format!(
                "{:?} {:?} {:?} {} {} {:?}",
                s.ema_fast.map(f64::to_bits),
                s.ema_slow.map(f64::to_bits),
                s.rsi.map(f64::to_bits),
                s.order_flow_imbalance,
                s.bars.len(),
                s.forming
            ));
        }
    }
    (stream, events)
}

#[test]
fn feature_stream_is_bit_identical_across_runs() {
    let (a, events) = run();
    let (b, _) = run();
    assert_eq!(a, b);
    assert_eq!(events, 78 + 42, "book events plus trades in the fixtures");
    assert!(
        a.iter().any(|s| !s.ends_with("gap: true, samples: 0 }")),
        "some bar got samples"
    );
}

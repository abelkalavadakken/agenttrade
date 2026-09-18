//! Prints every closed bar from a tape, and what breakout would see. Diagnostic.
use std::fs::File;

use features::{Features, FeaturesConfig};
use feed::kraken::Tracker;
use tape::{Mode, Reader};
use types::{instruments, FeedEvent};

fn main() {
    let path = std::env::args().nth(1).expect("tape");
    let mut tracker = Tracker::new(instruments::find("kraken", "BTC/USD").unwrap(), 10);
    let mut f = Features::new(FeaturesConfig::default());
    let mut reader = Reader::new(File::open(&path).unwrap(), Mode::Fast).unwrap();
    let mut seen = 0;
    while let Ok(Some(rec)) = reader.next_record() {
        match rec.source_id {
            0 => {
                for e in tracker.on_frame(&rec.bytes).events {
                    match e {
                        FeedEvent::Book(_) => f.on_book(tracker.book(), rec.recv_ns),
                        FeedEvent::Trade(t) => f.on_trade(&t, rec.recv_ns),
                        FeedEvent::Resync(_) => f.on_stale(),
                    }
                }
            }
            1 => {
                if rec.bytes == b"tick" {
                    f.advance(rec.recv_ns);
                }
                if rec.bytes == b"connected" {
                    tracker.expect_snapshot();
                }
            }
            _ => {}
        }
        let s = f.snapshot();
        if s.bars.len() > seen {
            seen = s.bars.len();
            let b = s.bars[seen - 1];
            let n = 5usize;
            let range = if seen > n {
                Some(&s.bars[seen - 1 - n..seen - 1])
            } else {
                None
            };
            let (hi, lo) = range.map_or((0, 0), |r| {
                (
                    r.iter().map(|b| b.high.0).max().unwrap(),
                    r.iter().map(|b| b.low.0).min().unwrap(),
                )
            });
            println!(
                "bar {seen:>3} o {} h {} l {} c {} vol {} gap {} samples {}  prev5 hi {hi} lo {lo}  {}",
                b.open.0, b.high.0, b.low.0, b.close.0, b.volume.0, b.gap, b.samples,
                if range.is_some() && (b.close.0 > hi || b.close.0 < lo) { "BREAK" } else { "" }
            );
        }
    }
}

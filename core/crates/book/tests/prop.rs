//! After any sequence of valid deltas: never crossed, len <= depth, sorted,
//! checksum equals a BTreeMap reference model.

use std::collections::BTreeMap;

use book::Book;
use proptest::prelude::*;
use types::{BookEvent, Level, Price, Qty};

const DEPTH: usize = 10;

#[derive(Default)]
struct Model {
    bids: BTreeMap<i64, i64>,
    asks: BTreeMap<i64, i64>,
}

impl Model {
    fn set(side: &mut BTreeMap<i64, i64>, p: i64, q: i64) {
        if q == 0 {
            side.remove(&p);
        } else {
            side.insert(p, q);
        }
    }

    fn truncate(&mut self) {
        while self.bids.len() > DEPTH {
            self.bids.pop_first();
        }
        while self.asks.len() > DEPTH {
            self.asks.pop_last();
        }
    }

    fn checksum(&self) -> u32 {
        let mut s = String::new();
        for (p, q) in self.asks.iter().take(DEPTH) {
            s.push_str(&format!("{p}{q}"));
        }
        for (p, q) in self.bids.iter().rev().take(DEPTH) {
            s.push_str(&format!("{p}{q}"));
        }
        crc32fast::hash(s.as_bytes())
    }

    fn best_bid(&self) -> Option<i64> {
        self.bids.keys().next_back().copied()
    }

    fn best_ask(&self) -> Option<i64> {
        self.asks.keys().next().copied()
    }
}

fn lv(p: i64, q: i64) -> Level {
    Level {
        price: Price(p),
        qty: Qty(q),
    }
}

/// One generated delta step: which side, how far from the touch, what qty.
#[derive(Debug, Clone)]
struct Step {
    bid: bool,
    offset: i64,
    qty: i64,
}

fn step() -> impl Strategy<Value = Step> {
    (
        any::<bool>(),
        0i64..40,
        prop_oneof![Just(0i64), 1i64..1_000_000],
    )
        .prop_map(|(bid, offset, qty)| Step { bid, offset, qty })
}

/// Prices are chosen relative to the current touch so a valid venue could have
/// sent them: bids strictly below the best ask, asks strictly above the best bid.
fn price_for(model: &Model, s: &Step) -> i64 {
    const CENTER: i64 = 1_000_000;
    if s.bid {
        model.best_ask().unwrap_or(CENTER + 1) - 1 - s.offset
    } else {
        model.best_bid().unwrap_or(CENTER - 1) + 1 + s.offset
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]
    #[test]
    fn invariants_hold(
        snap_bids in proptest::collection::btree_map(900_000i64..1_000_000, 1i64..1_000_000, 1..=DEPTH),
        snap_asks in proptest::collection::btree_map(1_000_001i64..1_100_000, 1i64..1_000_000, 1..=DEPTH),
        steps in proptest::collection::vec(step(), 1..200),
        group in 1usize..6,
    ) {
        let mut model = Model { bids: snap_bids, asks: snap_asks };
        let mut book = Book::new(DEPTH);
        let snapshot = BookEvent::Snapshot {
            bids: model.bids.iter().rev().map(|(p, q)| lv(*p, *q)).collect(),
            asks: model.asks.iter().map(|(p, q)| lv(*p, *q)).collect(),
            checksum: model.checksum(),
        };
        book.apply(&snapshot).unwrap();

        for chunk in steps.chunks(group) {
            let mut bids = Vec::new();
            let mut asks = Vec::new();
            for s in chunk {
                let p = price_for(&model, s);
                if s.bid {
                    Model::set(&mut model.bids, p, s.qty);
                    bids.push(lv(p, s.qty));
                } else {
                    Model::set(&mut model.asks, p, s.qty);
                    asks.push(lv(p, s.qty));
                }
            }
            model.truncate();
            let delta = BookEvent::Delta { bids, asks, checksum: model.checksum(), venue_time_ns: 0 };
            prop_assert_eq!(book.apply(&delta), Ok(()));

            prop_assert!(!book.is_crossed());
            prop_assert!(!book.is_stale());
            prop_assert!(book.bids().len() <= DEPTH);
            prop_assert!(book.asks().len() <= DEPTH);
            prop_assert!(book.bids().windows(2).all(|w| w[0].price > w[1].price));
            prop_assert!(book.asks().windows(2).all(|w| w[0].price < w[1].price));
            prop_assert_eq!(book.checksum(), model.checksum());
            if let (Some(b), Some(a)) = (book.best_bid(), book.best_ask()) {
                prop_assert!(b.price < a.price);
                prop_assert_eq!(b.price.0, model.best_bid().unwrap());
                prop_assert_eq!(a.price.0, model.best_ask().unwrap());
            }
        }
    }
}

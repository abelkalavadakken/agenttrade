//! Order flow imbalance, Cont, Kukanov and Stoikov, summed over a trailing window.

use std::collections::VecDeque;

use book::Book;
use types::Level;

/// Enough per-update entries for 60 s of a busy book without reallocating.
const CAPACITY: usize = 65_536;

pub struct Ofi {
    window_ns: i64,
    prev: Option<(Level, Level)>,
    entries: VecDeque<(i64, i64)>,
    sum: i64,
}

impl Ofi {
    pub fn new(window_ns: i64) -> Self {
        Self {
            window_ns,
            prev: None,
            entries: VecDeque::with_capacity(CAPACITY),
            sum: 0,
        }
    }

    pub fn on_book(&mut self, book: &Book, now_ns: i64) {
        let (Some(bid), Some(ask)) = (book.best_bid(), book.best_ask()) else {
            return;
        };
        if let Some((pb, pa)) = self.prev {
            let e = contribution(pb, bid, pa, ask);
            self.sum += e;
            self.entries.push_back((now_ns, e));
        }
        self.prev = Some((bid, ask));
        self.expire(now_ns);
    }

    fn expire(&mut self, now_ns: i64) {
        while let Some(&(ts, e)) = self.entries.front() {
            if now_ns - ts < self.window_ns {
                break;
            }
            self.entries.pop_front();
            self.sum -= e;
        }
    }

    /// Stale book: no flow to measure. Clears the window and the reference touch.
    pub fn reset(&mut self) {
        self.prev = None;
        self.entries.clear();
        self.sum = 0;
    }

    /// Sum in qty fixed-point units. Positive is buy pressure.
    pub fn value(&self) -> i64 {
        self.sum
    }
}

/// e = 1{b >= b'} q_b - 1{b <= b'} q_b' - 1{a <= a'} q_a + 1{a >= a'} q_a'
/// with primes for the previous touch.
fn contribution(pb: Level, b: Level, pa: Level, a: Level) -> i64 {
    let mut e = 0;
    if b.price >= pb.price {
        e += b.qty.0;
    }
    if b.price <= pb.price {
        e -= pb.qty.0;
    }
    if a.price <= pa.price {
        e -= a.qty.0;
    }
    if a.price >= pa.price {
        e += pa.qty.0;
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::{Price, Qty};

    fn lv(p: i64, q: i64) -> Level {
        Level {
            price: Price(p),
            qty: Qty(q),
        }
    }

    #[test]
    fn bid_size_grows_is_positive_flow() {
        assert_eq!(
            contribution(lv(100, 5), lv(100, 8), lv(101, 3), lv(101, 3)),
            3
        );
    }

    #[test]
    fn bid_price_improves_counts_full_new_size() {
        // new bid 101@4 replaces 100@5: +4 (price up) and no minus (b > b')
        // ask unchanged: -3 +3 = 0
        assert_eq!(
            contribution(lv(100, 5), lv(101, 4), lv(102, 3), lv(102, 3)),
            4
        );
    }

    #[test]
    fn ask_size_grows_is_negative_flow() {
        assert_eq!(
            contribution(lv(100, 5), lv(100, 5), lv(101, 3), lv(101, 7)),
            -4
        );
    }

    #[test]
    fn ask_lifted_counts_previous_size() {
        // ask moves up 101@3 -> 102@9: -0 (a > a') + 3 (a >= a')
        assert_eq!(
            contribution(lv(100, 5), lv(100, 5), lv(101, 3), lv(102, 9)),
            3
        );
    }
}

use types::{Level, Price, Qty};

use crate::MAX_DEPTH;

const EMPTY: Level = Level {
    price: Price(0),
    qty: Qty(0),
};

/// One side of the book, sorted best first. Bids descend, asks ascend.
#[derive(Debug, Clone)]
pub struct Side {
    levels: [Level; MAX_DEPTH],
    len: usize,
    descending: bool,
}

impl Side {
    pub fn new(descending: bool) -> Self {
        Self {
            levels: [EMPTY; MAX_DEPTH],
            len: 0,
            descending,
        }
    }

    pub fn as_slice(&self) -> &[Level] {
        &self.levels[..self.len]
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// qty 0 removes; otherwise replace or insert in sorted position.
    /// A level that would land past MAX_DEPTH is dropped: truncation would
    /// remove it anyway.
    pub fn set(&mut self, l: &Level) {
        match self.position(l.price) {
            Ok(i) if l.qty.is_zero() => self.remove(i),
            Ok(i) => self.levels[i].qty = l.qty,
            Err(_) if l.qty.is_zero() => {}
            Err(i) => self.insert(i, *l),
        }
    }

    pub fn truncate(&mut self, depth: usize) {
        self.len = self.len.min(depth);
    }

    /// Ok(index) if present, Err(insert index) otherwise.
    fn position(&self, price: Price) -> Result<usize, usize> {
        let slice = self.as_slice();
        if self.descending {
            slice.binary_search_by(|l| price.cmp(&l.price))
        } else {
            slice.binary_search_by(|l| l.price.cmp(&price))
        }
    }

    fn remove(&mut self, i: usize) {
        self.levels.copy_within(i + 1..self.len, i);
        self.len -= 1;
    }

    fn insert(&mut self, i: usize, l: Level) {
        if i >= MAX_DEPTH {
            return;
        }
        let end = self.len.min(MAX_DEPTH - 1);
        self.levels.copy_within(i..end, i + 1);
        self.levels[i] = l;
        self.len = (self.len + 1).min(MAX_DEPTH);
    }
}

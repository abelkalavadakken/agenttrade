//! Chained BLAKE3 over fixed-point core state. See docs/api.md section 4.

use book::Book;
use exec::PaperVenue;
use features::Bar;

const MAGIC: &[u8] = b"agenttrade.state.v1";

#[derive(Clone)]
pub struct StateHasher {
    chain: [u8; 32],
    events: u64,
}

impl Default for StateHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl StateHasher {
    pub fn new() -> Self {
        Self {
            chain: *blake3::hash(MAGIC).as_bytes(),
            events: 0,
        }
    }

    /// `hash_n = H(hash_{n-1} || encode(state_n))`, without committing.
    pub fn compute(&self, state: &CoreState) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        h.update(&self.chain);
        let mut sink = |b: &[u8]| {
            h.update(b);
        };
        encode(state, &mut sink);
        *h.finalize().as_bytes()
    }

    pub fn commit(&mut self, next: [u8; 32]) {
        self.chain = next;
        self.events += 1;
    }

    pub fn current(&self) -> [u8; 32] {
        self.chain
    }

    pub fn events(&self) -> u64 {
        self.events
    }

    pub fn hex(&self) -> String {
        self.chain.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Everything the hash covers, borrowed from the core for one update.
pub struct CoreState<'a> {
    pub sequence_id: u64,
    pub book: &'a Book,
    pub venue: &'a PaperVenue,
    pub bar_count: usize,
    pub last_bar: Option<Bar>,
    pub order_flow_imbalance: i64,
    pub open_orders: u32,
    pub intents_in_window: u32,
    pub kill_switch: bool,
}

fn encode(s: &CoreState, sink: &mut dyn FnMut(&[u8])) {
    let mut w = |v: i64| sink(&v.to_le_bytes());
    w(s.sequence_id as i64);
    for l in s.book.bids() {
        w(l.price.0);
        w(l.qty.0);
    }
    w(-1);
    for l in s.book.asks() {
        w(l.price.0);
        w(l.qty.0);
    }
    w(-1);
    w(s.book.is_stale() as i64);
    w(s.book.is_crossed() as i64);
    s.venue.hash_into(sink);
    let mut w = |v: i64| sink(&v.to_le_bytes());
    w(s.open_orders as i64);
    w(s.intents_in_window as i64);
    w(s.kill_switch as i64);
    w(s.bar_count as i64);
    if let Some(b) = s.last_bar {
        w(b.open.0);
        w(b.high.0);
        w(b.low.0);
        w(b.close.0);
        w(b.volume.0);
        w(b.gap as i64);
    }
    w(s.order_flow_imbalance);
}

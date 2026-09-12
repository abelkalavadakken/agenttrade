//! Every captured Kraken frame applies to Book without error.

use book::Book;
use types::{instruments, BookEvent, Level};

const FIXTURE: &str = include_str!("../../../../tests/fixtures/kraken_book_btcusd.jsonl");

fn levels(v: &serde_json::Value, inst: &types::Instrument) -> Vec<Level> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|l| Level {
            price: inst.parse_price(&l["price"].to_string()).unwrap(),
            qty: inst.parse_qty(&l["qty"].to_string()).unwrap(),
        })
        .collect()
}

#[test]
fn fixture_applies_cleanly() {
    let inst = instruments::find("kraken", "BTC/USD").unwrap();
    let mut book = Book::new(10);
    let mut applied = 0;
    for line in FIXTURE
        .lines()
        .filter(|l| l.starts_with("{\"channel\":\"book\""))
    {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let d = &v["data"][0];
        let bids = levels(&d["bids"], &inst);
        let asks = levels(&d["asks"], &inst);
        let checksum = d["checksum"].as_u64().unwrap() as u32;
        let event = match v["type"].as_str().unwrap() {
            "snapshot" => BookEvent::Snapshot {
                bids,
                asks,
                checksum,
            },
            _ => BookEvent::Delta {
                bids,
                asks,
                checksum,
                venue_time_ns: 0,
            },
        };
        book.apply(&event)
            .unwrap_or_else(|e| panic!("{e} on {line}"));
        assert!(!book.is_crossed());
        assert!(book.bids().len() <= 10 && book.asks().len() <= 10);
        applied += 1;
    }
    assert_eq!(applied, 78);
    assert!(!book.is_stale());
    assert_eq!(
        book.best_bid().unwrap().price.0 / 1_000,
        773,
        "still around 77.3k"
    );
}

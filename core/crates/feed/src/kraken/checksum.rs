//! Kraken v2 book checksum: CRC32 over top-N asks ascending then top-N bids
//! descending, each level as price then qty with the decimal point and leading
//! zeros removed. Our fixed-point integers already are that string.

use std::collections::BTreeMap;

/// `asks` and `bids` map fixed-point price to fixed-point qty.
pub fn book_checksum(asks: &BTreeMap<i64, i64>, bids: &BTreeMap<i64, i64>, depth: usize) -> u32 {
    let mut buf = String::with_capacity(depth * 2 * 20);
    for (p, q) in asks.iter().take(depth) {
        push(&mut buf, *p);
        push(&mut buf, *q);
    }
    for (p, q) in bids.iter().rev().take(depth) {
        push(&mut buf, *p);
        push(&mut buf, *q);
    }
    crc32fast::hash(buf.as_bytes())
}

fn push(buf: &mut String, v: i64) {
    use std::fmt::Write;
    // Leading zeros are gone because an integer never prints any.
    write!(buf, "{v}").expect("write to String");
}

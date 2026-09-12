//! Kraken v2 checksum from fixed-point levels, formatted into a stack buffer.

use types::Level;

pub const MAX_DEPTH: usize = 25;
/// Two sides, two fields per level, at most 20 digits and a sign each.
const BUF: usize = MAX_DEPTH * 2 * 2 * 21;

/// `asks` ascending, `bids` descending, both best first.
pub fn compute(asks: &[Level], bids: &[Level], depth: usize) -> u32 {
    let mut buf = [0u8; BUF];
    let mut pos = 0;
    for l in asks.iter().take(depth).chain(bids.iter().take(depth)) {
        pos = write_int(&mut buf, pos, l.price.0);
        pos = write_int(&mut buf, pos, l.qty.0);
    }
    crc32fast::hash(&buf[..pos])
}

/// Decimal digits, no leading zeros, which is what Kraken's strip rule yields.
fn write_int(buf: &mut [u8], mut pos: usize, v: i64) -> usize {
    if v == 0 {
        buf[pos] = b'0';
        return pos + 1;
    }
    if v < 0 {
        buf[pos] = b'-';
        pos += 1;
    }
    let mut n = v.unsigned_abs();
    let mut digits = [0u8; 20];
    let mut i = 0;
    while n > 0 {
        digits[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    while i > 0 {
        i -= 1;
        buf[pos] = digits[i];
        pos += 1;
    }
    pos
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
    fn matches_string_concatenation() {
        let asks = [lv(773_629, 7_301_963), lv(773_630, 1)];
        let bids = [lv(773_628, 5_300_065), lv(773_627, 1_292_283)];
        let s = "7736297301963773630177362853000657736271292283";
        assert_eq!(compute(&asks, &bids, 10), crc32fast::hash(s.as_bytes()));
    }

    #[test]
    fn depth_limits_input() {
        let asks: Vec<Level> = (0..12).map(|i| lv(100 + i, 1)).collect();
        let bids: Vec<Level> = (0..12).map(|i| lv(90 - i, 1)).collect();
        assert_ne!(compute(&asks, &bids, 10), compute(&asks, &bids, 12));
        assert_eq!(
            compute(&asks, &bids, 10),
            compute(&asks[..10], &bids[..10], 25)
        );
    }

    #[test]
    fn write_int_cases() {
        let mut b = [0u8; 32];
        let n = write_int(&mut b, 0, 0);
        assert_eq!(&b[..n], b"0");
        let n = write_int(&mut b, 0, -12);
        assert_eq!(&b[..n], b"-12");
        let n = write_int(&mut b, 0, i64::MAX);
        assert_eq!(&b[..n], i64::MAX.to_string().as_bytes());
    }
}

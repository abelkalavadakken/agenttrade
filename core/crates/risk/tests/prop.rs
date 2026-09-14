//! The checked i64 sizing arithmetic agrees with an i128 reference.

use proptest::prelude::*;

fn reference_loss_ok(qty: i64, ticks: i64, equity: i64, bps: i64, scale: u32) -> bool {
    let raw = qty as i128 * ticks as i128;
    let d = 10i128.pow(scale);
    let loss = raw / d + i128::from(raw % d != 0);
    loss * 10_000 <= equity as i128 * bps as i128
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]
    #[test]
    fn loss_check_matches_i128(
        qty in 1i64..10_000_000_000,        // up to 100 BTC
        ticks in 1i64..10_000_000,          // up to 1,000,000 USD at scale 1
        equity in 1i64..100_000_000_000,    // up to 10bn USD
        bps in 1i64..10_000,
    ) {
        let got = risk::loss_within_budget(qty, ticks, equity, bps, 8);
        let want = reference_loss_ok(qty, ticks, equity, bps, 8);
        match got {
            Some(v) => prop_assert_eq!(v, want),
            // Overflow in i64 means rejection; the i128 reference must then say "over budget"
            // or the loss side overflowed, which for these ranges only happens on the loss side.
            None => prop_assert!(qty as i128 * ticks as i128 > i64::MAX as i128),
        }
    }
}

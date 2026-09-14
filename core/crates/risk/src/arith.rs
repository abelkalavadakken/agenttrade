//! Checked i64 arithmetic for the sizing rules. `None` means overflow, which
//! the caller turns into a rejection. See docs/risk.md section 4.
//!
//! Products of qty and price sit at `qty_scale + price_scale`. Dividing by
//! `10^qty_scale` once, rounded up, brings them to quote units before the
//! bps comparison. Rounding up never lets a loss or a notional slip under.

const BPS: i64 = 10_000;

pub fn pow10(scale: u32) -> Option<i64> {
    10i64.checked_pow(scale)
}

fn div_ceil(a: i64, d: i64) -> i64 {
    a / d + i64::from(a % d != 0)
}

/// `ceil(qty * risk_ticks / 10^qty_scale) * 10_000 <= equity * risk_bps`.
pub fn loss_within_budget(
    qty: i64,
    risk_ticks: i64,
    equity: i64,
    risk_bps: i64,
    qty_scale: u32,
) -> Option<bool> {
    let loss = div_ceil(qty.checked_mul(risk_ticks)?, pow10(qty_scale)?);
    let lhs = loss.checked_mul(BPS)?;
    let rhs = equity.checked_mul(risk_bps)?;
    Some(lhs <= rhs)
}

/// `ceil(|net| * price / 10^qty_scale) * 10_000 <= equity * leverage_bps`.
pub fn notional_within_leverage(
    abs_net: u64,
    price: i64,
    equity: i64,
    leverage_bps: i64,
    qty_scale: u32,
) -> Option<bool> {
    let abs_net = i64::try_from(abs_net).ok()?;
    let notional = div_ceil(abs_net.checked_mul(price)?, pow10(qty_scale)?);
    let lhs = notional.checked_mul(BPS)?;
    let rhs = equity.checked_mul(leverage_bps)?;
    Some(lhs <= rhs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worked_example_from_docs() {
        // equity 100,000 USD at scale 1, 1% rule, 0.5 BTC, 362.8 USD stop distance
        assert_eq!(
            loss_within_budget(50_000_000, 3628, 1_000_000, 100, 8),
            Some(true)
        );
        // 2.76 BTC at the same distance loses 1,001 USD on a 1,000 USD budget
        assert_eq!(
            loss_within_budget(276_000_000, 3628, 1_000_000, 100, 8),
            Some(false)
        );
    }

    #[test]
    fn exact_boundary_and_ceil() {
        // equity 10_000 (1,000 USD), 1% -> budget 100 (10 USD). 1 BTC * 100 ticks = 100.
        assert_eq!(
            loss_within_budget(100_000_000, 100, 10_000, 100, 8),
            Some(true)
        );
        // One satoshi more rounds the loss up to 101.
        assert_eq!(
            loss_within_budget(100_000_001, 100, 10_000, 100, 8),
            Some(false)
        );
    }

    #[test]
    fn overflow_is_none_not_panic() {
        assert_eq!(loss_within_budget(i64::MAX, 2, 1, 100, 8), None);
        assert_eq!(loss_within_budget(1, 1, i64::MAX, 100, 8), None);
        assert_eq!(notional_within_leverage(u64::MAX, 1, 1, 1, 8), None);
        assert_eq!(pow10(19), None);
    }

    #[test]
    fn leverage_line_and_large_equity() {
        // equity 100k USD, 1x, price 77362.8: 1 BTC is 77k, 1.3 BTC is 100.5k
        assert_eq!(
            notional_within_leverage(100_000_000, 773_628, 1_000_000, 10_000, 8),
            Some(true)
        );
        assert_eq!(
            notional_within_leverage(130_000_000, 773_628, 1_000_000, 10_000, 8),
            Some(false)
        );
        // 1bn USD equity at 10x with 100 BTC does not overflow
        assert_eq!(
            notional_within_leverage(10_000_000_000, 773_628, 10_000_000_000, 100_000, 8),
            Some(true)
        );
    }

    #[test]
    fn div_ceil_cases() {
        assert_eq!(div_ceil(0, 7), 0);
        assert_eq!(div_ceil(7, 7), 1);
        assert_eq!(div_ceil(8, 7), 2);
    }
}

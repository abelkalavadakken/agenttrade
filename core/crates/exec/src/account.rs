//! Position and equity arithmetic. Products go through i128 and are divided by
//! `10^qty_scale` once. See docs/exec.md "Position and equity".

use types::{Position, Price, Qty, Side};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub starting_cash: i64,
    pub position: Position,
    pub peak_equity: i64,
    qty_unit: i128,
    last_mark: Option<Price>,
}

impl Account {
    pub fn new(starting_cash: i64, qty_scale: u32) -> Self {
        Self {
            starting_cash,
            position: Position::default(),
            peak_equity: starting_cash,
            qty_unit: 10i128.pow(qty_scale),
            last_mark: None,
        }
    }

    pub fn fill(&mut self, side: Side, price: Price, qty: Qty) {
        apply_fill(&mut self.position, side, price, qty, self.qty_unit);
    }

    pub fn unrealized(&self, mark: Price) -> i64 {
        let p = &self.position;
        let v = (mark.0 as i128 - p.average_entry_price.0 as i128) * p.net_qty.0 as i128
            / self.qty_unit;
        narrow(v)
    }

    pub fn equity(&self, mark: Price) -> i64 {
        narrow(
            self.starting_cash as i128
                + self.position.realized_pnl as i128
                + self.unrealized(mark) as i128,
        )
    }

    /// Records the mark and advances the high-water mark. Returns equity.
    pub fn mark(&mut self, mark: Price) -> i64 {
        self.last_mark = Some(mark);
        let e = self.equity(mark);
        self.peak_equity = self.peak_equity.max(e);
        e
    }

    pub fn last_mark(&self) -> Option<Price> {
        self.last_mark
    }

    pub fn is_flat(&self) -> bool {
        self.position.net_qty.is_zero()
    }
}

/// Increase, reduce or flip `p` by one fill. Used for the account and for
/// every per-strategy position, so attribution uses the same arithmetic.
pub fn apply_fill(p: &mut Position, side: Side, price: Price, qty: Qty, qty_unit: i128) {
    let signed = match side {
        Side::Buy => qty.0,
        Side::Sell => -qty.0,
    };
    let net = p.net_qty.0;
    if net == 0 || net.signum() == signed.signum() {
        increase(p, price, signed);
    } else if signed.unsigned_abs() <= net.unsigned_abs() {
        reduce(p, price, signed, qty_unit);
    } else {
        let closing = -net;
        reduce(p, price, closing, qty_unit);
        increase(p, price, signed - closing);
    }
}

fn increase(p: &mut Position, price: Price, signed: i64) {
    let old = p.net_qty.0.unsigned_abs() as i128;
    let add = signed.unsigned_abs() as i128;
    let avg = p.average_entry_price.0 as i128;
    let new_avg = (avg * old + price.0 as i128 * add) / (old + add);
    p.average_entry_price = Price(narrow(new_avg));
    p.net_qty = Qty(p.net_qty.0 + signed);
}

fn reduce(p: &mut Position, price: Price, signed: i64, qty_unit: i128) {
    let closed = signed.unsigned_abs() as i128;
    let direction = p.net_qty.0.signum() as i128;
    let pnl = direction * (price.0 as i128 - p.average_entry_price.0 as i128) * closed / qty_unit;
    p.realized_pnl = narrow(p.realized_pnl as i128 + pnl);
    p.net_qty = Qty(p.net_qty.0 + signed);
    if p.net_qty.is_zero() {
        p.average_entry_price = Price::ZERO;
    }
}

/// A position outside i64 quote units is outside anything this system should hold.
fn narrow(v: i128) -> i64 {
    i64::try_from(v).expect("position arithmetic outside i64")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acct() -> Account {
        Account::new(1_000_000, 8)
    }

    #[test]
    fn increase_averages_entry() {
        let mut a = acct();
        a.fill(Side::Buy, Price(1_000), Qty(100_000_000));
        a.fill(Side::Buy, Price(2_000), Qty(100_000_000));
        assert_eq!(a.position.net_qty, Qty(200_000_000));
        assert_eq!(a.position.average_entry_price, Price(1_500));
        assert_eq!(a.position.realized_pnl, 0);
    }

    #[test]
    fn reduce_realizes() {
        let mut a = acct();
        a.fill(Side::Buy, Price(1_000), Qty(200_000_000)); // 2 BTC @ 100.0
        a.fill(Side::Sell, Price(1_100), Qty(100_000_000)); // 1 BTC @ 110.0 -> +10.0
        assert_eq!(a.position.net_qty, Qty(100_000_000));
        assert_eq!(a.position.average_entry_price, Price(1_000));
        assert_eq!(a.position.realized_pnl, 100);
        a.fill(Side::Sell, Price(900), Qty(100_000_000)); // flat, -10.0
        assert!(a.is_flat());
        assert_eq!(a.position.average_entry_price, Price::ZERO);
        assert_eq!(a.position.realized_pnl, 0);
    }

    #[test]
    fn flip_closes_then_opens() {
        let mut a = acct();
        a.fill(Side::Buy, Price(1_000), Qty(100_000_000));
        a.fill(Side::Sell, Price(1_200), Qty(300_000_000)); // close 1 @ +20.0, short 2 @ 120.0
        assert_eq!(a.position.net_qty, Qty(-200_000_000));
        assert_eq!(a.position.average_entry_price, Price(1_200));
        assert_eq!(a.position.realized_pnl, 200);
        a.fill(Side::Buy, Price(1_100), Qty(200_000_000)); // cover 2 @ +20.0
        assert!(a.is_flat());
        assert_eq!(a.position.realized_pnl, 400);
    }

    #[test]
    fn short_unrealized_sign_follows_net() {
        let mut a = acct();
        a.fill(Side::Sell, Price(1_000), Qty(100_000_000));
        assert_eq!(a.unrealized(Price(900)), 100);
        assert_eq!(a.unrealized(Price(1_100)), -100);
        assert_eq!(a.equity(Price(900)), 1_000_100);
    }

    #[test]
    fn realized_plus_unrealized_is_mark_to_market() {
        let mut a = acct();
        let fills = [
            (Side::Buy, 1_000, 150_000_000),
            (Side::Sell, 1_050, 50_000_000),
            (Side::Sell, 950, 200_000_000),
            (Side::Buy, 900, 100_000_000),
        ];
        let mut cash = 0i128;
        let mut net = 0i128;
        for (s, p, q) in fills {
            a.fill(s, Price(p), Qty(q));
            let sign = if s == Side::Buy { 1 } else { -1 };
            cash -= sign * p as i128 * q as i128;
            net += sign * q as i128;
        }
        let mark = 1_020i128;
        let mtm = (cash + net * mark) / 100_000_000;
        let got = a.position.realized_pnl as i128 + a.unrealized(Price(mark as i64)) as i128;
        assert!((got - mtm).abs() <= 1, "got {got} want {mtm}");
    }

    #[test]
    fn peak_tracks_high_water() {
        let mut a = acct();
        a.fill(Side::Buy, Price(1_000), Qty(100_000_000));
        assert_eq!(a.mark(Price(1_500)), 1_000_500);
        assert_eq!(a.mark(Price(800)), 999_800);
        assert_eq!(a.peak_equity, 1_000_500);
    }
}

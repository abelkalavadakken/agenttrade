use crate::{Price, Qty};

/// Venue instrument definition. Field names follow proto Instrument except
/// `tick` and `lot`, which are the CLAUDE.md names for min_tick_size and min_lot_size.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Instrument {
    pub venue: String,
    pub symbol: String,
    pub price_scale: u32,
    pub qty_scale: u32,
    pub tick: Price,
    pub lot: Qty,
}

impl Instrument {
    pub fn is_tick_aligned(&self, price: Price) -> bool {
        price.0 % self.tick.0 == 0
    }

    pub fn is_lot_aligned(&self, qty: Qty) -> bool {
        qty.0 % self.lot.0 == 0
    }

    /// Round toward negative infinity onto the tick grid.
    pub fn floor_to_tick(&self, price: Price) -> Price {
        Price(price.0.div_euclid(self.tick.0) * self.tick.0)
    }

    /// Round toward positive infinity onto the tick grid.
    pub fn ceil_to_tick(&self, price: Price) -> Price {
        let floored = self.floor_to_tick(price);
        if floored == price {
            floored
        } else {
            floored + self.tick
        }
    }

    /// Round toward zero onto the lot grid. Never rounds a quantity up.
    pub fn floor_to_lot(&self, qty: Qty) -> Qty {
        Qty(qty.0 / self.lot.0 * self.lot.0)
    }

    /// Parse a decimal string like "62345.1" into fixed-point at `price_scale`.
    /// Rejects more fractional digits than the scale carries. No floats involved.
    pub fn parse_price(&self, s: &str) -> Option<Price> {
        parse_fixed(s, self.price_scale).map(Price)
    }

    pub fn parse_qty(&self, s: &str) -> Option<Qty> {
        parse_fixed(s, self.qty_scale).map(Qty)
    }
}

fn parse_fixed(s: &str, scale: u32) -> Option<i64> {
    let (neg, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let (int_part, frac_part) = s.split_once('.').unwrap_or((s, ""));
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    if frac_part.len() > scale as usize {
        return None;
    }
    let mut value: i64 = if int_part.is_empty() {
        0
    } else {
        int_part.parse().ok()?
    };
    let mut frac: i64 = if frac_part.is_empty() {
        0
    } else {
        frac_part.parse().ok()?
    };
    let pow = 10i64.checked_pow(scale)?;
    frac = frac.checked_mul(10i64.checked_pow(scale - frac_part.len() as u32)?)?;
    value = value.checked_mul(pow)?.checked_add(frac)?;
    Some(if neg { -value } else { value })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn btc_usd() -> Instrument {
        Instrument {
            venue: "kraken".into(),
            symbol: "BTC/USD".into(),
            price_scale: 1,
            qty_scale: 8,
            tick: Price(1),
            lot: Qty(1),
        }
    }

    fn coarse() -> Instrument {
        Instrument {
            tick: Price(5),
            lot: Qty(100),
            ..btc_usd()
        }
    }

    #[test]
    fn tick_alignment() {
        let i = coarse();
        assert!(i.is_tick_aligned(Price(10)));
        assert!(i.is_tick_aligned(Price(0)));
        assert!(i.is_tick_aligned(Price(-15)));
        assert!(!i.is_tick_aligned(Price(12)));
    }

    #[test]
    fn floor_and_ceil_to_tick() {
        let i = coarse();
        assert_eq!(i.floor_to_tick(Price(12)), Price(10));
        assert_eq!(i.ceil_to_tick(Price(12)), Price(15));
        assert_eq!(i.floor_to_tick(Price(10)), Price(10));
        assert_eq!(i.ceil_to_tick(Price(10)), Price(10));
        assert_eq!(i.floor_to_tick(Price(-12)), Price(-15));
        assert_eq!(i.ceil_to_tick(Price(-12)), Price(-10));
    }

    #[test]
    fn lot_alignment() {
        let i = coarse();
        assert!(i.is_lot_aligned(Qty(300)));
        assert!(!i.is_lot_aligned(Qty(350)));
        assert_eq!(i.floor_to_lot(Qty(350)), Qty(300));
        assert_eq!(i.floor_to_lot(Qty(99)), Qty(0));
        assert_eq!(i.floor_to_lot(Qty(-350)), Qty(-300));
    }

    #[test]
    fn parse_fixed_point() {
        let i = btc_usd();
        assert_eq!(i.parse_price("62345.1"), Some(Price(623451)));
        assert_eq!(i.parse_price("62345"), Some(Price(623450)));
        assert_eq!(i.parse_price("62345.15"), None);
        assert_eq!(i.parse_qty("0.00012345"), Some(Qty(12345)));
        assert_eq!(i.parse_qty("1.5"), Some(Qty(150_000_000)));
        assert_eq!(i.parse_qty("-0.5"), Some(Qty(-50_000_000)));
        assert_eq!(i.parse_qty(""), None);
        assert_eq!(i.parse_qty("."), None);
        assert_eq!(i.parse_qty("abc"), None);
    }
}

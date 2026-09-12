//! Hardcoded instrument definitions. A config file replaces this later.

use crate::{Instrument, Price, Qty};

/// Kraken BTC/USD: prices quoted to 0.1 USD, quantities to 1e-8 BTC.
pub fn kraken() -> Vec<Instrument> {
    vec![Instrument {
        venue: "kraken".into(),
        symbol: "BTC/USD".into(),
        price_scale: 1,
        qty_scale: 8,
        tick: Price(1),
        lot: Qty(1),
    }]
}

pub fn find(venue: &str, symbol: &str) -> Option<Instrument> {
    match venue {
        "kraken" => kraken().into_iter().find(|i| i.symbol == symbol),
        _ => None,
    }
}

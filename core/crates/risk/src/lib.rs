//! The gate. Every intent passes `check` before exec sees it. See docs/risk.md.
//! Reads a snapshot of inputs, returns a verdict, mutates nothing.

mod arith;

use book::Book;
use types::{Instrument, Intent, IntentEnvelope, Position, Price, Qty, RejectionCode, Side};

pub use arith::{loss_within_budget, notional_within_leverage, pow10};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskConfig {
    pub max_sequence_drift: u64,
    pub price_band_bps: i64,
    pub risk_per_trade_bps: i64,
    pub max_leverage_bps: i64,
    pub max_open_orders: u32,
    pub max_intents_per_window: u32,
    pub window_ns: i64,
    pub max_drawdown_bps: i64,
}

pub struct RiskInputs<'a> {
    pub instrument: &'a Instrument,
    pub book: &'a Book,
    pub book_sequence_id: u64,
    pub venue_connected: bool,
    pub instrument_halted: bool,
    pub position: Position,
    pub equity: i64,
    pub peak_equity: i64,
    pub open_orders: u32,
    pub intents_in_window: u32,
    pub kill_switch: bool,
    pub now_ns: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Approved,
    Rejected {
        code: RejectionCode,
        reason: &'static str,
    },
}

impl Verdict {
    pub fn code(&self) -> RejectionCode {
        match self {
            Verdict::Approved => RejectionCode::None,
            Verdict::Rejected { code, .. } => *code,
        }
    }
}

const BPS: i64 = 10_000;

type Check = fn(&RiskConfig, &RiskInputs, &IntentEnvelope) -> Option<Verdict>;

/// Runs the checks in docs/risk.md order and stops at the first failure.
pub fn check(cfg: &RiskConfig, inp: &RiskInputs, env: &IntentEnvelope) -> Verdict {
    let steps: [Check; 8] = [
        kill_switch,
        market_state,
        staleness,
        alignment,
        price_band,
        stop_and_sizing,
        leverage,
        rate_limit,
    ];
    steps
        .iter()
        .find_map(|step| step(cfg, inp, env))
        .unwrap_or(Verdict::Approved)
}

fn reject(code: RejectionCode, reason: &'static str) -> Option<Verdict> {
    Some(Verdict::Rejected { code, reason })
}

fn kill_switch(cfg: &RiskConfig, inp: &RiskInputs, _: &IntentEnvelope) -> Option<Verdict> {
    if inp.kill_switch {
        return reject(RejectionCode::KillSwitchActive, "operator kill switch");
    }
    let allowed_loss = inp
        .peak_equity
        .checked_mul(cfg.max_drawdown_bps)
        .map(|v| v / BPS);
    match allowed_loss {
        Some(loss) if inp.equity >= inp.peak_equity - loss => None,
        Some(_) => reject(RejectionCode::KillSwitchActive, "drawdown limit"),
        None => reject(RejectionCode::KillSwitchActive, "drawdown overflow"),
    }
}

fn market_state(_: &RiskConfig, inp: &RiskInputs, env: &IntentEnvelope) -> Option<Verdict> {
    if env.venue != inp.instrument.venue || env.symbol != inp.instrument.symbol {
        return reject(RejectionCode::InvalidIntent, "unknown instrument");
    }
    if !inp.venue_connected {
        return reject(RejectionCode::VenueDisconnected, "venue not connected");
    }
    if inp.book.is_stale() {
        return reject(RejectionCode::VenueDisconnected, "book stale");
    }
    if inp.book.best_bid().is_none() || inp.book.best_ask().is_none() {
        return reject(RejectionCode::VenueDisconnected, "book side empty");
    }
    if inp.instrument_halted {
        return reject(RejectionCode::InstrumentHalted, "venue status not online");
    }
    None
}

fn staleness(cfg: &RiskConfig, inp: &RiskInputs, env: &IntentEnvelope) -> Option<Verdict> {
    if env.source_sequence_id > inp.book_sequence_id {
        return reject(RejectionCode::StaleState, "intent sequence ahead of book");
    }
    if inp.book_sequence_id - env.source_sequence_id > cfg.max_sequence_drift {
        return reject(RejectionCode::StaleState, "intent sequence too old");
    }
    None
}

fn place(env: &IntentEnvelope) -> Option<(Side, Price, Price, Qty)> {
    match env.intent {
        Intent::Place {
            side,
            price,
            stop,
            qty,
            ..
        } => Some((side, price, stop, qty)),
        _ => None,
    }
}

fn alignment(_: &RiskConfig, inp: &RiskInputs, env: &IntentEnvelope) -> Option<Verdict> {
    let (_, price, stop, qty) = place(env)?;
    let i = inp.instrument;
    if !i.is_tick_aligned(price) {
        return reject(RejectionCode::InvalidTickSize, "price off tick");
    }
    if !i.is_tick_aligned(stop) {
        return reject(RejectionCode::InvalidTickSize, "stop off tick");
    }
    if qty <= Qty::ZERO {
        return reject(RejectionCode::InvalidTickSize, "qty not positive");
    }
    if !i.is_lot_aligned(qty) {
        return reject(RejectionCode::InvalidTickSize, "qty off lot");
    }
    None
}

fn price_band(cfg: &RiskConfig, inp: &RiskInputs, env: &IntentEnvelope) -> Option<Verdict> {
    let (_, price, _, _) = place(env)?;
    let mid = inp.book.mid()?;
    let diff = (price.0 - mid.0).abs();
    let (Some(lhs), Some(rhs)) = (diff.checked_mul(BPS), mid.0.checked_mul(cfg.price_band_bps))
    else {
        return reject(RejectionCode::PriceOutOfBand, "band overflow");
    };
    if lhs > rhs {
        return reject(RejectionCode::PriceOutOfBand, "price too far from mid");
    }
    None
}

fn stop_and_sizing(cfg: &RiskConfig, inp: &RiskInputs, env: &IntentEnvelope) -> Option<Verdict> {
    let (side, price, stop, qty) = place(env)?;
    if stop.is_zero() {
        return reject(RejectionCode::MissingStop, "place without stop");
    }
    let wrong_side = match side {
        Side::Buy => stop >= price,
        Side::Sell => stop <= price,
    };
    if wrong_side {
        return reject(RejectionCode::InvalidIntent, "stop on wrong side of price");
    }
    match arith::loss_within_budget(
        qty.0,
        (price.0 - stop.0).abs(),
        inp.equity,
        cfg.risk_per_trade_bps,
        inp.instrument.qty_scale,
    ) {
        Some(true) => None,
        Some(false) => reject(
            RejectionCode::ExceedsSingleLossLimit,
            "loss at stop over budget",
        ),
        None => reject(RejectionCode::ExceedsSingleLossLimit, "overflow"),
    }
}

fn leverage(cfg: &RiskConfig, inp: &RiskInputs, env: &IntentEnvelope) -> Option<Verdict> {
    let (side, price, _, qty) = place(env)?;
    let signed = match side {
        Side::Buy => qty.0,
        Side::Sell => -qty.0,
    };
    let Some(new_net) = inp.position.net_qty.0.checked_add(signed) else {
        return reject(RejectionCode::ExceedsMaxLeverage, "overflow");
    };
    match arith::notional_within_leverage(
        new_net.unsigned_abs(),
        price.0,
        inp.equity,
        cfg.max_leverage_bps,
        inp.instrument.qty_scale,
    ) {
        Some(true) => None,
        Some(false) => reject(RejectionCode::ExceedsMaxLeverage, "notional over leverage"),
        None => reject(RejectionCode::ExceedsMaxLeverage, "overflow"),
    }
}

fn rate_limit(cfg: &RiskConfig, inp: &RiskInputs, env: &IntentEnvelope) -> Option<Verdict> {
    let is_place = matches!(env.intent, Intent::Place { .. });
    let counts = is_place || matches!(env.intent, Intent::Flatten);
    if !counts {
        return None;
    }
    if is_place && inp.open_orders >= cfg.max_open_orders {
        return reject(RejectionCode::RateLimitExceeded, "too many open orders");
    }
    if inp.intents_in_window >= cfg.max_intents_per_window {
        return reject(
            RejectionCode::RateLimitExceeded,
            "too many intents in window",
        );
    }
    None
}

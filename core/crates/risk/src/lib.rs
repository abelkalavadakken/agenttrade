//! The gate. Every intent passes `check` before exec sees it. See docs/risk.md.
//! Reads a snapshot of inputs, returns a verdict, mutates nothing.

mod arith;

use book::Book;
use types::{
    Instrument, Intent, IntentEnvelope, Position, Price, Qty, RejectionCode, Side, TimeInForce,
};

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
    /// Addendum: allocation and the discretionary profile.
    pub max_strategies_enabled: u32,
    pub max_size_multiplier_bps: i64,
    pub discretionary_max_qty: Qty,
    pub discretionary_max_intents_per_window: u32,
}

/// Hard bounds of one tunable parameter, as the strategy declares them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamBounds {
    pub name: String,
    pub min: i64,
    pub max: i64,
}

/// What the gate needs to know about each registered strategy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrategySummary {
    pub id: String,
    pub enabled: bool,
    pub setup_active: Option<u64>,
    pub params: Vec<ParamBounds>,
}

/// The reserved strategy id for LLM PLACE orders.
pub const DISCRETIONARY: &str = "discretionary";

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
    pub discretionary_intents_in_window: u32,
    pub kill_switch: bool,
    pub now_ns: i64,
    pub strategies: &'a [StrategySummary],
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
/// FLATTEN_ALL only needs a connected venue: the exit is the safe direction.
pub fn check(cfg: &RiskConfig, inp: &RiskInputs, env: &IntentEnvelope) -> Verdict {
    if matches!(env.intent, Intent::FlattenAll) {
        return if inp.venue_connected {
            Verdict::Approved
        } else {
            Verdict::Rejected {
                code: RejectionCode::VenueDisconnected,
                reason: "venue not connected",
            }
        };
    }
    let steps: [Check; 12] = [
        kill_switch,
        market_state,
        staleness,
        allocate,
        tune,
        setup_decision,
        discretionary_profile,
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
    let counts = is_place || matches!(env.intent, Intent::Flatten | Intent::Tune { .. });
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
    if is_place
        && is_discretionary(inp, env)
        && inp.discretionary_intents_in_window >= cfg.discretionary_max_intents_per_window
    {
        return reject(
            RejectionCode::RateLimitExceeded,
            "discretionary intents in window",
        );
    }
    None
}

fn strategy<'a>(inp: &'a RiskInputs, id: &str) -> Option<&'a StrategySummary> {
    inp.strategies.iter().find(|s| s.id == id)
}

/// A PLACE whose agent is not a registered strategy runs the discretionary profile.
fn is_discretionary(inp: &RiskInputs, env: &IntentEnvelope) -> bool {
    strategy(inp, &env.agent_id).is_none()
}

fn allocate(cfg: &RiskConfig, inp: &RiskInputs, env: &IntentEnvelope) -> Option<Verdict> {
    let Intent::Allocate(a) = &env.intent else {
        return None;
    };
    let Some(target) = strategy(inp, &a.strategy_id) else {
        return reject(RejectionCode::InvalidIntent, "unknown strategy");
    };
    if !(0..=cfg.max_size_multiplier_bps).contains(&a.size_multiplier_bps) {
        return reject(
            RejectionCode::AllocationLimit,
            "size multiplier out of bounds",
        );
    }
    let enabled = inp.strategies.iter().filter(|s| s.enabled).count() as u32;
    if a.enabled && !target.enabled && enabled >= cfg.max_strategies_enabled {
        return reject(
            RejectionCode::AllocationLimit,
            "too many strategies enabled",
        );
    }
    None
}

fn tune(_: &RiskConfig, inp: &RiskInputs, env: &IntentEnvelope) -> Option<Verdict> {
    let Intent::Tune {
        strategy_id,
        param,
        value,
    } = &env.intent
    else {
        return None;
    };
    let Some(target) = strategy(inp, strategy_id) else {
        return reject(RejectionCode::InvalidIntent, "unknown strategy");
    };
    let Some(bounds) = target.params.iter().find(|p| &p.name == param) else {
        return reject(RejectionCode::InvalidIntent, "unknown param");
    };
    if !(bounds.min..=bounds.max).contains(value) {
        return reject(RejectionCode::ParamOutOfBounds, "value outside hard bounds");
    }
    None
}

fn setup_decision(cfg: &RiskConfig, inp: &RiskInputs, env: &IntentEnvelope) -> Option<Verdict> {
    let (strategy_id, setup_id, multiplier) = match &env.intent {
        Intent::ConfirmSetup {
            strategy_id,
            setup_id,
            size_multiplier_bps,
        } => (strategy_id, *setup_id, Some(*size_multiplier_bps)),
        Intent::RejectSetup {
            strategy_id,
            setup_id,
        } => (strategy_id, *setup_id, None),
        _ => return None,
    };
    let Some(target) = strategy(inp, strategy_id) else {
        return reject(RejectionCode::InvalidIntent, "unknown strategy");
    };
    if target.setup_active != Some(setup_id) {
        return reject(RejectionCode::InvalidIntent, "setup not active");
    }
    if let Some(m) = multiplier {
        if !(0..=cfg.max_size_multiplier_bps).contains(&m) {
            return reject(
                RejectionCode::AllocationLimit,
                "size multiplier out of bounds",
            );
        }
    }
    None
}

/// docs/risk.md addendum: smaller size, limit only, stop and take-profit required.
fn discretionary_profile(
    cfg: &RiskConfig,
    inp: &RiskInputs,
    env: &IntentEnvelope,
) -> Option<Verdict> {
    let Intent::Place {
        side,
        price,
        take_profit,
        qty,
        tif,
        ..
    } = &env.intent
    else {
        return None;
    };
    if !is_discretionary(inp, env) {
        return None;
    }
    if *qty > cfg.discretionary_max_qty {
        return reject(
            RejectionCode::ExceedsSingleLossLimit,
            "discretionary max qty",
        );
    }
    if *tif == TimeInForce::Fok {
        return reject(
            RejectionCode::InvalidIntent,
            "discretionary orders are GTC or IOC",
        );
    }
    if take_profit.is_zero() {
        return reject(
            RejectionCode::MissingExitPlan,
            "discretionary place without take_profit",
        );
    }
    let wrong_side = match side {
        Side::Buy => take_profit <= price,
        Side::Sell => take_profit >= price,
    };
    if wrong_side {
        return reject(
            RejectionCode::InvalidIntent,
            "take_profit on wrong side of price",
        );
    }
    None
}

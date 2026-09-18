//! Deterministic strategies on the core loop and the runner that gates them.
//! See docs/strategy.md.

mod breakout;
mod mr_ofi;
mod runner;

pub use breakout::Breakout;
pub use mr_ofi::MrOfi;
pub use runner::{Context, Counters, Runner, RunnerOutput, StrategyView, WakeSetup};

use book::Book;
use types::{Position, Price, Qty, Side, TimeInForce};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Horizon {
    Seconds,
    Minutes,
    Hours,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Autonomous,
    Gated,
}

impl Horizon {
    /// The horizon decides the mode; a strategy does not choose.
    pub fn mode(self) -> Mode {
        match self {
            Horizon::Seconds => Mode::Autonomous,
            Horizon::Minutes | Horizon::Hours => Mode::Gated,
        }
    }
}

/// A tunable with hard bounds compiled into the strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Param {
    pub name: &'static str,
    pub value: i64,
    pub min: i64,
    pub max: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamError {
    Unknown,
    OutOfBounds { min: i64, max: i64 },
}

/// Everything a strategy may look at. Its own position and orders, never
/// the account's.
pub struct State<'a> {
    pub now_ns: i64,
    pub sequence_id: u64,
    pub book: &'a Book,
    pub features: &'a features::Snapshot<'a>,
    pub position: Position,
    pub open_orders: u32,
}

/// A proposed entry. `qty` is the base size before the multiplier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Order {
    pub side: Side,
    pub price: Price,
    pub stop: Price,
    pub qty: Qty,
    pub tif: TimeInForce,
    pub reason: &'static str,
}

/// What `on_event` may ask for. Exits go through Flatten so they never
/// need a stop and never race their own protection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Place(Order),
    Flatten { reason: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupState {
    None,
    Forming,
    Active { setup_id: u64, order: Order },
}

pub trait Strategy: Send {
    fn id(&self) -> &'static str;
    fn horizon(&self) -> Horizon;
    fn params(&self) -> &[Param];
    fn set_param(&mut self, name: &str, value: i64) -> Result<(), ParamError>;
    /// Called after every accepted book event, trade, tick and fill.
    fn on_event(&mut self, state: &State) -> Option<Action>;
    fn setup_state(&self) -> SetupState;
    /// The runner confirmed, rejected or expired the active setup.
    fn on_setup_cleared(&mut self) {}
    fn reset(&mut self);

    fn mode(&self) -> Mode {
        self.horizon().mode()
    }

    fn param(&self, name: &str) -> Option<i64> {
        self.params()
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.value)
    }
}

/// Shared `set_param` for strategies that keep params in a slice.
pub fn set_bounded(params: &mut [Param], name: &str, value: i64) -> Result<(), ParamError> {
    let p = params
        .iter_mut()
        .find(|p| p.name == name)
        .ok_or(ParamError::Unknown)?;
    if value < p.min || value > p.max {
        return Err(ParamError::OutOfBounds {
            min: p.min,
            max: p.max,
        });
    }
    p.value = value;
    Ok(())
}

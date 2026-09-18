//! Order state machine, paper venue, position and equity. See docs/exec.md.
//! Deterministic: every timestamp is tape time passed in by the caller.

mod account;
mod order;
mod paper;

pub use account::{apply_fill, Account};
pub use order::{CancelReason, Order, OrderKind};
pub use paper::{PaperConfig, PaperVenue};

use types::{OrderState, Position, Price, Qty, Side};

/// What exec records on the tape, in the order it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecEvent {
    Transition {
        order_id: u64,
        from: OrderState,
        to: OrderState,
        reason: Option<CancelReason>,
        ns: i64,
    },
    Fill {
        order_id: u64,
        side: Side,
        price: Price,
        qty: Qty,
        /// A stop whose first pass could not complete against displayed depth.
        thin_book: bool,
        strategy_id: String,
        ns: i64,
    },
    Position {
        position: Position,
        equity: i64,
        ns: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecError {
    #[error("unknown order {0}")]
    UnknownOrder(u64),
    #[error("order {0} is not open")]
    NotOpen(u64),
    #[error("book has no {0:?} side")]
    EmptySide(Side),
    #[error("nothing to flatten")]
    Flat,
    #[error("no position for strategy {0}")]
    UnknownStrategy(String),
}

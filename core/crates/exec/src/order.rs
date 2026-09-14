use types::{OrderState, Price, Qty, Side, TimeInForce};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    Requested,
    IocUnfilled,
    FokUnfillable,
    Flatten,
    PositionFlat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderKind {
    Limit,
    /// Protective stop for `parent`. `price` is the trigger until it fires,
    /// after which the order is marketable and walks displayed depth.
    Stop {
        parent: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    pub id: u64,
    pub intent_id: String,
    pub kind: OrderKind,
    pub side: Side,
    pub price: Price,
    pub stop: Price,
    pub qty: Qty,
    pub filled: Qty,
    /// Fill qty scheduled but not yet applied, so a resting order is not
    /// matched twice against the same displayed size.
    pub pending_fill: Qty,
    pub tif: TimeInForce,
    pub state: OrderState,
    pub reason: Option<CancelReason>,
    pub thin_book: bool,
    pub submitted_ns: i64,
    pub updated_ns: i64,
}

impl Order {
    pub fn remaining(&self) -> Qty {
        self.qty - self.filled - self.pending_fill
    }

    pub fn is_terminal(&self) -> bool {
        self.state.is_terminal()
    }

    /// Every legal edge of the table in docs/exec.md.
    pub fn can_transition(from: OrderState, to: OrderState) -> bool {
        use OrderState::*;
        matches!(
            (from, to),
            (
                PendingNew,
                Open | Rejected | Filled | Canceled | PartiallyFilled
            ) | (Open, PartiallyFilled | Filled | PendingCancel)
                | (PartiallyFilled, PartiallyFilled | Filled | PendingCancel)
                | (PendingCancel, Canceled | Filled | PendingCancel)
        )
    }

    /// Applies a transition. Illegal edges panic in debug and are ignored in
    /// release, per docs/exec.md.
    pub(crate) fn transition(
        &mut self,
        to: OrderState,
        reason: Option<CancelReason>,
        ns: i64,
    ) -> bool {
        let from = self.state;
        if !Self::can_transition(from, to) {
            debug_assert!(false, "illegal order transition {from:?} -> {to:?}");
            return false;
        }
        self.state = to;
        if reason.is_some() {
            self.reason = reason;
        }
        self.updated_ns = ns;
        true
    }
}

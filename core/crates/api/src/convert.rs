//! Proto <-> internal. Field name mapping per docs/types.md.

use exec::{CancelReason, ExecEvent};
use features::Bar;
use risk::Verdict;
use types::{
    Instrument, Intent, IntentEnvelope, OrderState, Position, Price, Qty, RejectionCode, Side,
    TimeInForce,
};

use crate::v1;

pub fn instrument(i: &Instrument) -> v1::Instrument {
    v1::Instrument {
        venue: i.venue.clone(),
        symbol: i.symbol.clone(),
        price_scale: i.price_scale as i32,
        qty_scale: i.qty_scale as i32,
        min_tick_size: i.tick.0,
        min_lot_size: i.lot.0,
    }
}

/// Why a request never reached the gate. The reason names the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Invalid(pub &'static str);

pub fn envelope(r: &v1::SubmitIntentRequest) -> Result<IntentEnvelope, Invalid> {
    let intent = match v1::IntentType::try_from(r.intent_type) {
        Ok(v1::IntentType::Place) => Intent::Place {
            side: side(r.side)?,
            price: Price(r.target_price),
            stop: Price(r.stop_loss),
            qty: Qty(r.quantity),
            tif: tif(r.time_in_force)?,
        },
        Ok(v1::IntentType::Cancel) => Intent::Cancel {
            order_id: match r.target_order_id {
                0 => return Err(Invalid("target_order_id missing")),
                id => id,
            },
        },
        Ok(v1::IntentType::Flatten) => Intent::Flatten,
        Ok(v1::IntentType::Noop) => Intent::Noop,
        _ => return Err(Invalid("intent_type unspecified")),
    };
    if r.intent_id.is_empty() {
        return Err(Invalid("intent_id empty"));
    }
    if matches!(intent, Intent::Place { qty, .. } if qty.0 <= 0) {
        return Err(Invalid("quantity not positive"));
    }
    Ok(IntentEnvelope {
        intent_id: r.intent_id.clone(),
        agent_id: r.agent_id.clone(),
        source_sequence_id: r.source_sequence_id,
        generated_time_ns: r.generated_time_ns,
        venue: r.venue.clone(),
        symbol: r.symbol.clone(),
        intent,
    })
}

fn side(v: i32) -> Result<Side, Invalid> {
    match v1::OrderSide::try_from(v) {
        Ok(v1::OrderSide::Buy) => Ok(Side::Buy),
        Ok(v1::OrderSide::Sell) => Ok(Side::Sell),
        _ => Err(Invalid("side unspecified")),
    }
}

fn tif(v: i32) -> Result<TimeInForce, Invalid> {
    match v1::TimeInForce::try_from(v) {
        Ok(v1::TimeInForce::Gtc) => Ok(TimeInForce::Gtc),
        Ok(v1::TimeInForce::Ioc) => Ok(TimeInForce::Ioc),
        Ok(v1::TimeInForce::Fok) => Ok(TimeInForce::Fok),
        _ => Err(Invalid("time_in_force unspecified")),
    }
}

pub fn rejection_code(c: RejectionCode) -> v1::RejectionCode {
    v1::RejectionCode::try_from(c as i32).unwrap_or(v1::RejectionCode::None)
}

pub fn response(
    intent_id: &str,
    verdict: Verdict,
    order_id: u64,
    now_ns: i64,
) -> v1::SubmitIntentResponse {
    let (decision, code, reason) = match verdict {
        Verdict::Approved => (v1::RiskDecision::Approved, v1::RejectionCode::None, ""),
        Verdict::Rejected { code, reason } => {
            (v1::RiskDecision::Rejected, rejection_code(code), reason)
        }
    };
    v1::SubmitIntentResponse {
        intent_id: intent_id.to_string(),
        decision: decision as i32,
        rejection_code: code as i32,
        rejection_reason: reason.to_string(),
        executed_order_id: order_id,
        processed_time_ns: now_ns,
    }
}

pub fn invalid_response(intent_id: &str, why: Invalid, now_ns: i64) -> v1::SubmitIntentResponse {
    v1::SubmitIntentResponse {
        intent_id: intent_id.to_string(),
        decision: v1::RiskDecision::Rejected as i32,
        rejection_code: v1::RejectionCode::InvalidIntent as i32,
        rejection_reason: why.0.to_string(),
        executed_order_id: 0,
        processed_time_ns: now_ns,
    }
}

pub fn position(p: Position, unrealized: i64) -> v1::Position {
    v1::Position {
        net_qty: p.net_qty.0,
        average_entry_price: p.average_entry_price.0,
        unrealized_pnl: unrealized,
        realized_pnl: p.realized_pnl,
    }
}

pub fn bar(b: &Bar) -> v1::Bar {
    v1::Bar {
        open: b.open.0,
        high: b.high.0,
        low: b.low.0,
        close: b.close.0,
        volume: b.volume.0,
        close_ns: b.close_ns,
        gap: b.gap,
    }
}

fn order_state(s: OrderState) -> v1::OrderState {
    match s {
        OrderState::PendingNew => v1::OrderState::OrderPendingNew,
        OrderState::Open => v1::OrderState::OrderOpen,
        OrderState::PartiallyFilled => v1::OrderState::OrderPartiallyFilled,
        OrderState::Filled => v1::OrderState::OrderFilled,
        OrderState::PendingCancel => v1::OrderState::OrderPendingCancel,
        OrderState::Canceled => v1::OrderState::OrderCanceled,
        OrderState::Rejected => v1::OrderState::OrderRejected,
    }
}

fn cancel_reason(r: Option<CancelReason>) -> String {
    r.map(|r| format!("{r:?}")).unwrap_or_default()
}

pub fn exec_event(e: &ExecEvent, unrealized: i64) -> v1::ExecEvent {
    use v1::exec_event::Event;
    let event = match e {
        ExecEvent::Transition {
            order_id,
            from,
            to,
            reason,
            ns,
        } => Event::Transition(v1::OrderTransition {
            order_id: *order_id,
            from: order_state(*from) as i32,
            to: order_state(*to) as i32,
            reason: cancel_reason(*reason),
            ns: *ns,
        }),
        ExecEvent::Fill {
            order_id,
            side,
            price,
            qty,
            thin_book,
            ns,
        } => Event::Fill(v1::Fill {
            order_id: *order_id,
            side: *side as i32,
            price: price.0,
            qty: qty.0,
            thin_book: *thin_book,
            ns: *ns,
            strategy_id: String::new(), // attribution lands with crates/strategy
        }),
        ExecEvent::Position {
            position: p,
            equity,
            ns,
        } => Event::Position(v1::PositionUpdate {
            position: Some(position(*p, unrealized)),
            equity: *equity,
            ns: *ns,
        }),
    };
    v1::ExecEvent { event: Some(event) }
}

//! Owns the strategies, their allocations, held gated setups and counters.
//! Never talks to exec or risk; the core loop does. docs/strategy.md section 4.

use book::Book;
use risk::{ParamBounds, StrategySummary};
use types::{Allocation, Intent, IntentEnvelope, OnDisable, Position, Qty};

use crate::{Action, Mode, Order, Param, ParamError, SetupState, State, Strategy};

const BPS: i64 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    pub setups: u64,
    pub confirmed: u64,
    pub rejected: u64,
    pub expired: u64,
    pub orders_sent: u64,
    pub orders_rejected_by_gate: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeSetup {
    pub strategy_id: &'static str,
    pub setup_id: u64,
    pub order: Order,
    pub ttl_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerOutput {
    /// Through the gate. The multiplier that sized it, for the tape.
    Intent {
        envelope: IntentEnvelope,
        effective_multiplier_bps: i64,
    },
    Wake(WakeSetup),
    Expired {
        strategy_id: &'static str,
        setup_id: u64,
    },
}

/// What GetState reports per strategy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrategyView {
    pub id: &'static str,
    pub mode: Mode,
    pub enabled: bool,
    pub size_multiplier_bps: i64,
    pub on_disable: OnDisable,
    pub pending: Option<(u64, Order)>,
    pub params: Vec<Param>,
    pub counters: Counters,
}

struct Held {
    setup_id: u64,
    order: Order,
    expires_ns: i64,
}

struct Entry {
    strategy: Box<dyn Strategy>,
    enabled: bool,
    size_multiplier_bps: i64,
    on_disable: OnDisable,
    held: Option<Held>,
    /// Set when a setup was asked about; cleared when the strategy's setup
    /// state returns to None or Forming. Prevents re-asking on the next event.
    asked: bool,
    counters: Counters,
}

/// Shared inputs for one runner pass; per-strategy position comes from `positions`.
pub struct Context<'a> {
    pub now_ns: i64,
    pub sequence_id: u64,
    pub book: &'a Book,
    pub features: &'a features::Snapshot<'a>,
    pub venue: &'a str,
    pub symbol: &'a str,
}

pub struct Runner {
    entries: Vec<Entry>,
}

impl Default for Runner {
    fn default() -> Self {
        Self::new()
    }
}

impl Runner {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Registered disabled at 1x with Flatten on disable, per the note.
    pub fn register(&mut self, strategy: Box<dyn Strategy>) {
        self.entries.push(Entry {
            strategy,
            enabled: false,
            size_multiplier_bps: BPS,
            on_disable: OnDisable::Flatten,
            held: None,
            asked: false,
            counters: Counters::default(),
        });
    }

    pub fn ids(&self) -> Vec<&'static str> {
        self.entries.iter().map(|e| e.strategy.id()).collect()
    }

    fn entry(&mut self, id: &str) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.strategy.id() == id)
    }

    /// One pass after a core event. `positions` gives each strategy its own
    /// position and open order count from exec.
    pub fn on_event(
        &mut self,
        ctx: &Context,
        positions: &dyn Fn(&str) -> (Position, u32),
    ) -> Vec<RunnerOutput> {
        let mut out = Vec::new();
        for e in &mut self.entries {
            let id = e.strategy.id();
            // Expiry runs even when disabled, so a held setup never outlives its TTL.
            if let Some(h) = &e.held {
                if ctx.now_ns >= h.expires_ns {
                    let setup_id = h.setup_id;
                    e.held = None;
                    e.counters.expired += 1;
                    e.strategy.on_setup_cleared();
                    out.push(RunnerOutput::Expired {
                        strategy_id: id,
                        setup_id,
                    });
                }
            }
            if !e.enabled {
                continue;
            }
            let (position, open_orders) = positions(id);
            let state = State {
                now_ns: ctx.now_ns,
                sequence_id: ctx.sequence_id,
                book: ctx.book,
                features: ctx.features,
                position,
                open_orders,
            };
            let action = e.strategy.on_event(&state);
            match e.strategy.mode() {
                Mode::Autonomous => {
                    if let Some(a) = action {
                        let m = e.size_multiplier_bps;
                        if let Some(env) = envelope(ctx, id, a, m) {
                            e.counters.orders_sent += 1;
                            out.push(RunnerOutput::Intent {
                                envelope: env,
                                effective_multiplier_bps: m,
                            });
                        }
                    }
                }
                Mode::Gated => {
                    // Gated strategies may still flatten themselves autonomously.
                    if let Some(a @ Action::Flatten { .. }) = action {
                        if let Some(env) = envelope(ctx, id, a, e.size_multiplier_bps) {
                            out.push(RunnerOutput::Intent {
                                envelope: env,
                                effective_multiplier_bps: e.size_multiplier_bps,
                            });
                        }
                    }
                    match e.strategy.setup_state() {
                        SetupState::Active { setup_id, order } if e.held.is_none() && !e.asked => {
                            let ttl_ns = e.strategy.param("ttl_ns").unwrap_or(120_000_000_000);
                            e.held = Some(Held {
                                setup_id,
                                order,
                                expires_ns: ctx.now_ns + ttl_ns,
                            });
                            e.asked = true;
                            e.counters.setups += 1;
                            out.push(RunnerOutput::Wake(WakeSetup {
                                strategy_id: id,
                                setup_id,
                                order,
                                ttl_ns,
                            }));
                        }
                        SetupState::None | SetupState::Forming => e.asked = false,
                        SetupState::Active { .. } => {}
                    }
                }
            }
        }
        out
    }

    /// Applies an allocation the gate approved. Disabling with Flatten emits
    /// the strategy's own flatten intent.
    pub fn allocate(
        &mut self,
        ctx: &Context,
        a: &Allocation,
        position: Position,
    ) -> Vec<RunnerOutput> {
        let mut out = Vec::new();
        let Some(e) = self.entry(&a.strategy_id) else {
            return out;
        };
        let was_enabled = e.enabled;
        e.size_multiplier_bps = a.size_multiplier_bps;
        e.on_disable = a.on_disable;
        e.enabled = a.enabled;
        if was_enabled && !a.enabled {
            let id = e.strategy.id();
            if let Some(h) = e.held.take() {
                e.counters.rejected += 1;
                out.push(RunnerOutput::Expired {
                    strategy_id: id,
                    setup_id: h.setup_id,
                });
            }
            e.strategy.on_setup_cleared();
            e.strategy.reset();
            e.asked = false;
            if a.on_disable == OnDisable::Flatten && !position.net_qty.is_zero() {
                if let Some(env) = envelope(ctx, id, Action::Flatten { reason: "disabled" }, BPS) {
                    out.push(RunnerOutput::Intent {
                        envelope: env,
                        effective_multiplier_bps: BPS,
                    });
                }
            }
        } else if !was_enabled && a.enabled {
            e.strategy.reset();
        }
        out
    }

    pub fn tune(&mut self, id: &str, param: &str, value: i64) -> Result<(), ParamError> {
        self.entry(id)
            .ok_or(ParamError::Unknown)?
            .strategy
            .set_param(param, value)
    }

    /// Releases a held setup. The confirm multiplier overrides the allocation
    /// multiplier for this one order, clamped to [0, allocation]. Zero skips.
    pub fn confirm(
        &mut self,
        ctx: &Context,
        id: &str,
        setup_id: u64,
        multiplier_bps: i64,
    ) -> Option<RunnerOutput> {
        let e = self.entry(id)?;
        let h = e.held.as_ref().filter(|h| h.setup_id == setup_id)?;
        let effective = multiplier_bps.clamp(0, e.size_multiplier_bps);
        let order = h.order;
        e.held = None;
        e.strategy.on_setup_cleared();
        let strategy_id = e.strategy.id();
        if effective == 0 {
            e.counters.rejected += 1;
            return None;
        }
        let env = envelope(ctx, strategy_id, Action::Place(order), effective)?;
        e.counters.confirmed += 1;
        e.counters.orders_sent += 1;
        Some(RunnerOutput::Intent {
            envelope: env,
            effective_multiplier_bps: effective,
        })
    }

    pub fn reject(&mut self, id: &str, setup_id: u64) -> bool {
        let Some(e) = self.entry(id) else {
            return false;
        };
        if e.held.as_ref().is_none_or(|h| h.setup_id != setup_id) {
            return false;
        }
        e.held = None;
        e.counters.rejected += 1;
        e.strategy.on_setup_cleared();
        true
    }

    /// The gate rejected an order this runner sent.
    pub fn on_gate_rejected(&mut self, id: &str) {
        if let Some(e) = self.entry(id) {
            e.counters.orders_rejected_by_gate += 1;
        }
    }

    pub fn summaries(&self) -> Vec<StrategySummary> {
        self.entries
            .iter()
            .map(|e| StrategySummary {
                id: e.strategy.id().to_string(),
                enabled: e.enabled,
                setup_active: e.held.as_ref().map(|h| h.setup_id),
                params: e
                    .strategy
                    .params()
                    .iter()
                    .map(|p| ParamBounds {
                        name: p.name.to_string(),
                        min: p.min,
                        max: p.max,
                    })
                    .collect(),
            })
            .collect()
    }

    pub fn views(&self) -> Vec<StrategyView> {
        self.entries
            .iter()
            .map(|e| StrategyView {
                id: e.strategy.id(),
                mode: e.strategy.mode(),
                enabled: e.enabled,
                size_multiplier_bps: e.size_multiplier_bps,
                on_disable: e.on_disable,
                pending: e.held.as_ref().map(|h| (h.setup_id, h.order)),
                params: e.strategy.params().to_vec(),
                counters: e.counters,
            })
            .collect()
    }

    /// Fixed layout of everything replay must reproduce.
    pub fn hash_into(&self, sink: &mut dyn FnMut(&[u8])) {
        for e in &self.entries {
            sink(e.strategy.id().as_bytes());
            let mut w = |v: i64| sink(&v.to_le_bytes());
            w(e.enabled as i64);
            w(e.size_multiplier_bps);
            w(e.on_disable as i64);
            w(e.asked as i64);
            match &e.held {
                Some(h) => {
                    w(h.setup_id as i64);
                    w(h.expires_ns);
                    w(h.order.price.0);
                    w(h.order.qty.0);
                }
                None => w(-1),
            }
            for p in e.strategy.params() {
                w(p.value);
            }
            let c = e.counters;
            for v in [
                c.setups,
                c.confirmed,
                c.rejected,
                c.expired,
                c.orders_sent,
                c.orders_rejected_by_gate,
            ] {
                w(v as i64);
            }
        }
    }
}

fn envelope(
    ctx: &Context,
    id: &'static str,
    action: Action,
    multiplier_bps: i64,
) -> Option<IntentEnvelope> {
    let intent = match action {
        Action::Place(o) => {
            let qty = Qty(o.qty.0.checked_mul(multiplier_bps)? / BPS);
            if qty.is_zero() {
                return None;
            }
            Intent::Place {
                side: o.side,
                price: o.price,
                stop: o.stop,
                take_profit: types::Price::ZERO,
                qty,
                tif: o.tif,
            }
        }
        Action::Flatten { .. } => Intent::Flatten,
    };
    Some(IntentEnvelope {
        intent_id: format!("{id}-{}", ctx.sequence_id),
        agent_id: id.to_string(),
        source_sequence_id: ctx.sequence_id,
        generated_time_ns: ctx.now_ns,
        venue: ctx.venue.to_string(),
        symbol: ctx.symbol.to_string(),
        intent,
    })
}

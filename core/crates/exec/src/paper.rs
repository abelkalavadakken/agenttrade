//! Paper venue. Reads the book, schedules acks and fills on tape time, never
//! touches a network. See docs/exec.md "Paper venue".

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};

use book::Book;
use types::{Intent, IntentEnvelope, Level, OrderState, Position, Price, Qty, Side, TimeInForce};

use crate::account::{apply_fill, Account};
use crate::order::{CancelReason, Order, OrderKind};
use crate::{ExecError, ExecEvent};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaperConfig {
    pub ack_latency_ns: i64,
    pub fill_latency_ns: i64,
    pub cancel_latency_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    Ack(u64),
    Fill {
        order_id: u64,
        price: Price,
        qty: Qty,
        thin_book: bool,
    },
    Cancel(u64),
}

/// Ordered by due time then insertion, so replays drain identically.
type Scheduled = Reverse<(i64, u64, ActionSlot)>;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ActionSlot(u64);

struct NewOrder<'a> {
    intent_id: &'a str,
    strategy_id: &'a str,
    side: Side,
    price: Price,
    stop: Price,
    take_profit: Price,
    qty: Qty,
    tif: TimeInForce,
    kind: OrderKind,
}

pub struct PaperVenue {
    cfg: PaperConfig,
    qty_unit: i128,
    orders: BTreeMap<u64, Order>,
    queue: BinaryHeap<Scheduled>,
    actions: BTreeMap<u64, Action>,
    next_order_id: u64,
    next_slot: u64,
    account: Account,
    /// One position per `strategy_id`; fills attribute here and to the account.
    strategies: BTreeMap<String, Position>,
}

impl PaperVenue {
    pub fn new(cfg: PaperConfig, starting_cash: i64, qty_scale: u32) -> Self {
        Self {
            cfg,
            qty_unit: 10i128.pow(qty_scale),
            orders: BTreeMap::new(),
            queue: BinaryHeap::new(),
            actions: BTreeMap::new(),
            next_order_id: 1,
            next_slot: 0,
            account: Account::new(starting_cash, qty_scale),
            strategies: BTreeMap::new(),
        }
    }

    pub fn account(&self) -> &Account {
        &self.account
    }

    /// Per-strategy positions, sorted by id.
    pub fn strategies(&self) -> &BTreeMap<String, Position> {
        &self.strategies
    }

    pub fn strategy_position(&self, strategy_id: &str) -> Position {
        self.strategies
            .get(strategy_id)
            .copied()
            .unwrap_or_default()
    }

    pub fn order(&self, id: u64) -> Option<&Order> {
        self.orders.get(&id)
    }

    pub fn orders(&self) -> impl Iterator<Item = &Order> {
        self.orders.values()
    }

    pub fn open_orders(&self) -> usize {
        self.orders
            .values()
            .filter(|o| o.kind == OrderKind::Limit && !o.is_terminal())
            .count()
    }

    pub fn open_orders_of(&self, strategy_id: &str) -> usize {
        self.orders
            .values()
            .filter(|o| {
                o.kind == OrderKind::Limit && !o.is_terminal() && o.strategy_id == strategy_id
            })
            .count()
    }

    /// Feeds every byte of execution state that replay must reproduce to
    /// `sink`, fixed little-endian layout. Terminal orders are skipped: they
    /// no longer influence anything.
    pub fn hash_into(&self, sink: &mut dyn FnMut(&[u8])) {
        let mut w = |v: i64| sink(&v.to_le_bytes());
        for o in self.orders.values().filter(|o| !o.is_terminal()) {
            w(o.id as i64);
            let (tag, parent) = match o.kind {
                OrderKind::Limit => (0, 0),
                OrderKind::Stop { parent } => (1, parent),
                OrderKind::Exit { parent } => (2, parent),
            };
            w(tag);
            w(parent as i64);
            w(o.side as i64);
            w(o.price.0);
            w(o.take_profit.0);
            w(o.qty.0);
            w(o.filled.0);
            w(o.pending_fill.0);
            w(o.state as i64);
            w(o.thin_book as i64);
            w(o.strategy_id.len() as i64);
        }
        w(-1);
        let mut queued: Vec<&Scheduled> = self.queue.iter().collect();
        queued.sort();
        for Reverse((due, slot, _)) in queued {
            w(*due);
            match &self.actions[slot] {
                Action::Ack(id) => {
                    w(1);
                    w(*id as i64);
                }
                Action::Cancel(id) => {
                    w(2);
                    w(*id as i64);
                }
                Action::Fill {
                    order_id,
                    price,
                    qty,
                    thin_book,
                } => {
                    w(3);
                    w(*order_id as i64);
                    w(price.0);
                    w(qty.0);
                    w(*thin_book as i64);
                }
            }
        }
        w(-2);
        let a = &self.account;
        w(a.position.net_qty.0);
        w(a.position.average_entry_price.0);
        w(a.position.realized_pnl);
        w(a.peak_equity);
        w(self.next_order_id as i64);
        for (id, p) in &self.strategies {
            sink(id.as_bytes());
            let mut w = |v: i64| sink(&v.to_le_bytes());
            w(p.net_qty.0);
            w(p.average_entry_price.0);
            w(p.realized_pnl);
        }
    }

    /// Accepts an already risk-approved intent. Returns the new order id for
    /// Place and Flatten, the target id for Cancel, 0 otherwise. The
    /// strategy verbs (Allocate, Tune, setups) are not exec's; they return 0.
    pub fn submit(
        &mut self,
        env: &IntentEnvelope,
        book: &Book,
        now_ns: i64,
        out: &mut Vec<ExecEvent>,
    ) -> Result<u64, ExecError> {
        match &env.intent {
            Intent::Place {
                side,
                price,
                stop,
                take_profit,
                qty,
                tif,
            } => Ok(self.place(
                NewOrder {
                    intent_id: &env.intent_id,
                    strategy_id: &env.agent_id,
                    side: *side,
                    price: *price,
                    stop: *stop,
                    take_profit: *take_profit,
                    qty: *qty,
                    tif: *tif,
                    kind: OrderKind::Limit,
                },
                now_ns,
            )),
            Intent::Cancel { order_id } => {
                self.request_cancel(*order_id, CancelReason::Requested, now_ns, out)?;
                Ok(*order_id)
            }
            Intent::Flatten => {
                self.flatten_strategy(&env.intent_id, &env.agent_id, book, now_ns, out)
            }
            Intent::FlattenAll => {
                self.flatten_all(&env.intent_id, book, now_ns, out)?;
                Ok(0)
            }
            _ => Ok(0),
        }
    }

    fn place(&mut self, n: NewOrder, now_ns: i64) -> u64 {
        let id = self.next_order_id;
        self.next_order_id += 1;
        self.orders.insert(
            id,
            Order {
                id,
                intent_id: n.intent_id.to_string(),
                strategy_id: n.strategy_id.to_string(),
                kind: n.kind,
                side: n.side,
                price: n.price,
                stop: n.stop,
                take_profit: n.take_profit,
                qty: n.qty,
                filled: Qty::ZERO,
                pending_fill: Qty::ZERO,
                tif: n.tif,
                state: OrderState::PendingNew,
                reason: None,
                thin_book: false,
                submitted_ns: now_ns,
                updated_ns: now_ns,
            },
        );
        // Stops arm on their trigger; limits and exits go through the venue ack.
        if !matches!(n.kind, OrderKind::Stop { .. }) {
            self.schedule(now_ns + self.cfg.ack_latency_ns, Action::Ack(id));
        }
        id
    }

    fn request_cancel(
        &mut self,
        id: u64,
        reason: CancelReason,
        now_ns: i64,
        out: &mut Vec<ExecEvent>,
    ) -> Result<(), ExecError> {
        let order = self
            .orders
            .get_mut(&id)
            .ok_or(ExecError::UnknownOrder(id))?;
        match order.state {
            OrderState::Open | OrderState::PartiallyFilled => {}
            _ => return Err(ExecError::NotOpen(id)),
        }
        let from = order.state;
        order.transition(OrderState::PendingCancel, Some(reason), now_ns);
        out.push(ExecEvent::Transition {
            order_id: id,
            from,
            to: OrderState::PendingCancel,
            reason: Some(reason),
            ns: now_ns,
        });
        self.schedule(now_ns + self.cfg.cancel_latency_ns, Action::Cancel(id));
        Ok(())
    }

    fn cancel_open_limits(
        &mut self,
        strategy: Option<&str>,
        reason: CancelReason,
        now_ns: i64,
        out: &mut Vec<ExecEvent>,
    ) -> Result<(), ExecError> {
        let open: Vec<u64> = self
            .orders
            .values()
            .filter(|o| o.kind == OrderKind::Limit)
            .filter(|o| strategy.is_none_or(|s| o.strategy_id == s))
            .filter(|o| matches!(o.state, OrderState::Open | OrderState::PartiallyFilled))
            .map(|o| o.id)
            .collect();
        for id in open {
            self.request_cancel(id, reason, now_ns, out)?;
        }
        Ok(())
    }

    /// Cancels the strategy's open limit orders, then sends an IOC for its
    /// net at the worst displayed level so it walks the depth.
    pub fn flatten_strategy(
        &mut self,
        intent_id: &str,
        strategy_id: &str,
        book: &Book,
        now_ns: i64,
        out: &mut Vec<ExecEvent>,
    ) -> Result<u64, ExecError> {
        self.cancel_open_limits(Some(strategy_id), CancelReason::Flatten, now_ns, out)?;
        let net = self.strategy_position(strategy_id).net_qty.0;
        if net == 0 {
            return Err(ExecError::Flat);
        }
        let side = if net > 0 { Side::Sell } else { Side::Buy };
        // Limit orders already working on the exit side (an earlier flatten
        // still pending its ack) count against the net, so two flattens in
        // flight cannot flip the position. Stops and exits do not count: they
        // are cancelled when the position clears.
        let in_flight: i64 = self
            .orders
            .values()
            .filter(|o| o.strategy_id == strategy_id && o.kind == OrderKind::Limit)
            .filter(|o| !o.is_terminal() && o.side == side)
            .map(|o| o.remaining().0)
            .sum();
        let net = net.abs() - in_flight;
        if net <= 0 {
            return Err(ExecError::Flat);
        }
        let levels = opposite(book, side);
        let worst = levels.last().ok_or(ExecError::EmptySide(side))?.price;
        Ok(self.place(
            NewOrder {
                intent_id,
                strategy_id,
                side,
                price: worst,
                stop: Price::ZERO,
                take_profit: Price::ZERO,
                qty: Qty(net),
                tif: TimeInForce::Ioc,
                kind: OrderKind::Limit,
            },
            now_ns,
        ))
    }

    /// Every open order cancelled, every strategy position flattened.
    pub fn flatten_all(
        &mut self,
        intent_id: &str,
        book: &Book,
        now_ns: i64,
        out: &mut Vec<ExecEvent>,
    ) -> Result<usize, ExecError> {
        self.cancel_open_limits(None, CancelReason::Flatten, now_ns, out)?;
        let ids: Vec<String> = self
            .strategies
            .iter()
            .filter(|(_, p)| !p.net_qty.is_zero())
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            self.flatten_strategy(intent_id, id, book, now_ns, out)?;
        }
        Ok(ids.len())
    }

    /// Drains everything due, matches resting orders and triggered stops
    /// against the displayed book, marks to market.
    pub fn on_book(&mut self, book: &Book, now_ns: i64, out: &mut Vec<ExecEvent>) {
        self.drain(book, now_ns, out);
        self.trigger_stops(book, now_ns, out);
        self.match_resting(book, now_ns);
        if let Some(mid) = book.mid() {
            let equity = self.account.mark(mid);
            out.push(ExecEvent::Position {
                position: self.account.position,
                equity,
                ns: now_ns,
            });
        }
    }

    fn schedule(&mut self, due_ns: i64, action: Action) {
        let slot = ActionSlot(self.next_slot);
        self.next_slot += 1;
        self.actions.insert(slot.0, action);
        self.queue.push(Reverse((due_ns, slot.0, slot)));
    }

    fn drain(&mut self, book: &Book, now_ns: i64, out: &mut Vec<ExecEvent>) {
        while let Some(Reverse((due, slot, _))) = self.queue.peek().cloned() {
            if due > now_ns {
                break;
            }
            self.queue.pop();
            let action = self.actions.remove(&slot).expect("scheduled action");
            match action {
                Action::Ack(id) => self.ack(id, book, due, out),
                Action::Fill {
                    order_id,
                    price,
                    qty,
                    thin_book,
                } => self.apply_fill(order_id, price, qty, thin_book, true, due, out),
                Action::Cancel(id) => self.cancel(id, due, out),
            }
        }
    }

    /// Ack time: immediate matching against displayed depth, applied while
    /// the order is still PendingNew so it lands directly in its final state
    /// per the table in docs/exec.md. IOC and FOK resolve here; GTC rests.
    fn ack(&mut self, id: u64, book: &Book, ns: i64, out: &mut Vec<ExecEvent>) {
        let (side, price, remaining, tif) = {
            let o = &self.orders[&id];
            (o.side, o.price, o.remaining(), o.tif)
        };
        if self.orders[&id].is_terminal() {
            return; // cancelled by a flatten before the ack landed
        }
        let fills = match_levels(opposite(book, side), side, price, remaining, false);
        let total: i64 = fills.iter().map(|(_, q)| q.0).sum();
        if tif == TimeInForce::Fok && total < remaining.0 {
            self.finish(
                id,
                OrderState::Canceled,
                Some(CancelReason::FokUnfillable),
                ns,
                out,
            );
            return;
        }
        for (p, q) in fills {
            self.apply_fill(id, p, q, false, false, ns, out);
        }
        let full = total >= remaining.0;
        let (to, reason) = match (tif, full, total > 0) {
            (_, true, _) => (OrderState::Filled, None),
            (TimeInForce::Ioc, false, _) => (OrderState::Canceled, Some(CancelReason::IocUnfilled)),
            (_, false, true) => (OrderState::PartiallyFilled, None),
            (_, false, false) => (OrderState::Open, None),
        };
        self.finish(id, to, reason, ns, out);
        let strategy = self.orders[&id].strategy_id.clone();
        self.settle_protection(&strategy, ns, out);
    }

    fn cancel(&mut self, id: u64, ns: i64, out: &mut Vec<ExecEvent>) {
        if self.orders[&id].state == OrderState::PendingCancel {
            let reason = self.orders[&id].reason;
            self.finish(id, OrderState::Canceled, reason, ns, out);
        }
    }

    fn finish(
        &mut self,
        id: u64,
        to: OrderState,
        reason: Option<CancelReason>,
        ns: i64,
        out: &mut Vec<ExecEvent>,
    ) {
        let o = self.orders.get_mut(&id).expect("order");
        let from = o.state;
        if o.transition(to, reason, ns) {
            out.push(ExecEvent::Transition {
                order_id: id,
                from,
                to,
                reason,
                ns,
            });
        }
    }

    /// `transition` is false at ack time, when the caller picks the final state.
    #[allow(clippy::too_many_arguments)]
    fn apply_fill(
        &mut self,
        id: u64,
        price: Price,
        qty: Qty,
        thin_book: bool,
        transition: bool,
        ns: i64,
        out: &mut Vec<ExecEvent>,
    ) {
        let (side, kind, full, strategy) = {
            let o = self.orders.get_mut(&id).expect("order");
            o.filled = o.filled + qty;
            if o.pending_fill >= qty {
                o.pending_fill = o.pending_fill - qty;
            }
            (o.side, o.kind, o.filled >= o.qty, o.strategy_id.clone())
        };
        self.account.fill(side, price, qty);
        apply_fill(
            self.strategies.entry(strategy.clone()).or_default(),
            side,
            price,
            qty,
            self.qty_unit,
        );
        out.push(ExecEvent::Fill {
            order_id: id,
            side,
            price,
            qty,
            thin_book,
            strategy_id: strategy.clone(),
            ns,
        });
        let to = if full {
            OrderState::Filled
        } else {
            OrderState::PartiallyFilled
        };
        if transition && self.orders[&id].state != to {
            self.finish(id, to, None, ns, out);
        }
        out.push(ExecEvent::Position {
            position: self.account.position,
            equity: self
                .account
                .last_mark()
                .map_or(self.account.starting_cash, |m| self.account.equity(m)),
            ns,
        });
        if kind == OrderKind::Limit {
            self.grow_protection(id, qty, ns);
        }
        if transition {
            self.settle_protection(&strategy, ns, out);
        }
    }

    /// One stop and, when the parent asks for one, one take-profit per
    /// parent, each sized to what the parent has filled.
    fn grow_protection(&mut self, parent: u64, qty: Qty, ns: i64) {
        let (side, stop, take_profit, intent_id, strategy_id) = {
            let p = &self.orders[&parent];
            (
                p.side,
                p.stop,
                p.take_profit,
                p.intent_id.clone(),
                p.strategy_id.clone(),
            )
        };
        let exit_side = match side {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        };
        let wanted = [
            (OrderKind::Stop { parent }, stop),
            (OrderKind::Exit { parent }, take_profit),
        ];
        for (kind, price) in wanted {
            if price.is_zero() {
                continue;
            }
            let existing = self
                .orders
                .values_mut()
                .find(|o| o.kind == kind && !o.is_terminal());
            match existing {
                Some(o) => o.qty = o.qty + qty,
                None => {
                    self.place(
                        NewOrder {
                            intent_id: &intent_id,
                            strategy_id: &strategy_id,
                            side: exit_side,
                            price,
                            stop: Price::ZERO,
                            take_profit: Price::ZERO,
                            qty,
                            tif: TimeInForce::Gtc,
                            kind,
                        },
                        ns,
                    );
                }
            }
        }
    }

    /// When a strategy is flat its stops and exits have nothing to protect.
    fn settle_protection(&mut self, strategy_id: &str, ns: i64, out: &mut Vec<ExecEvent>) {
        if !self.strategy_position(strategy_id).net_qty.is_zero() {
            return;
        }
        let ids: Vec<u64> = self
            .orders
            .values()
            .filter(|o| {
                o.kind.parent().is_some() && !o.is_terminal() && o.strategy_id == strategy_id
            })
            .map(|o| o.id)
            .collect();
        for id in ids {
            let reason = Some(CancelReason::PositionFlat);
            match self.orders[&id].state {
                OrderState::PendingNew | OrderState::PendingCancel => {
                    self.finish(id, OrderState::Canceled, reason, ns, out)
                }
                OrderState::Open | OrderState::PartiallyFilled => {
                    self.finish(id, OrderState::PendingCancel, reason, ns, out);
                    self.finish(id, OrderState::Canceled, reason, ns, out);
                }
                _ => {}
            }
        }
    }

    /// A stop fires when the touch reaches its trigger, then walks all depth.
    fn trigger_stops(&mut self, book: &Book, ns: i64, out: &mut Vec<ExecEvent>) {
        let (Some(bid), Some(ask)) = (book.best_bid(), book.best_ask()) else {
            return;
        };
        let armed: Vec<u64> = self
            .orders
            .values()
            .filter(|o| {
                matches!(o.kind, OrderKind::Stop { .. }) && o.state == OrderState::PendingNew
            })
            .filter(|o| match o.side {
                Side::Sell => bid.price <= o.price,
                Side::Buy => ask.price >= o.price,
            })
            .map(|o| o.id)
            .collect();
        for id in armed {
            self.finish(id, OrderState::Open, None, ns, out);
        }
    }

    /// Resting orders match against displayed size; fills land after fill_latency.
    fn match_resting(&mut self, book: &Book, ns: i64) {
        let ids: Vec<u64> = self
            .orders
            .values()
            .filter(|o| matches!(o.state, OrderState::Open | OrderState::PartiallyFilled))
            .filter(|o| o.remaining() > Qty::ZERO)
            .map(|o| o.id)
            .collect();
        for id in ids {
            let (side, price, remaining, is_stop, was_thin) = {
                let o = &self.orders[&id];
                (
                    o.side,
                    o.price,
                    o.remaining(),
                    matches!(o.kind, OrderKind::Stop { .. }),
                    o.thin_book,
                )
            };
            let fills = match_levels(opposite(book, side), side, price, remaining, is_stop);
            let total: i64 = fills.iter().map(|(_, q)| q.0).sum();
            let thin = is_stop && (was_thin || total < remaining.0);
            if is_stop && total < remaining.0 {
                self.orders.get_mut(&id).expect("order").thin_book = true;
            }
            for (p, q) in fills {
                {
                    let o = self.orders.get_mut(&id).expect("order");
                    o.pending_fill = o.pending_fill + q;
                }
                self.schedule(
                    ns + self.cfg.fill_latency_ns,
                    Action::Fill {
                        order_id: id,
                        price: p,
                        qty: q,
                        thin_book: thin,
                    },
                );
            }
        }
    }
}

fn opposite(book: &Book, side: Side) -> &[Level] {
    match side {
        Side::Buy => book.asks(),
        Side::Sell => book.bids(),
    }
}

/// Walks levels best first. `market` ignores the limit and takes everything.
fn match_levels(
    levels: &[Level],
    side: Side,
    limit: Price,
    want: Qty,
    market: bool,
) -> Vec<(Price, Qty)> {
    let mut left = want.0;
    let mut fills = Vec::new();
    for l in levels {
        if left <= 0 {
            break;
        }
        let crosses = match side {
            Side::Buy => l.price <= limit,
            Side::Sell => l.price >= limit,
        };
        if !market && !crosses {
            break;
        }
        let q = left.min(l.qty.0);
        if q > 0 {
            fills.push((l.price, Qty(q)));
            left -= q;
        }
    }
    fills
}

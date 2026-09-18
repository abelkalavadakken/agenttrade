//! The core loop. One owner of feed tracker, book, features, risk state, exec
//! and tape, on a dedicated OS thread. See docs/api.md sections 1 to 4.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;

use book::Book;
use exec::{ExecError, ExecEvent, PaperConfig, PaperVenue};
use features::{Bar, Features, FeaturesConfig};
use feed::kraken::Tracker;
use feed::FeedMsg;
use prost::Message;
use risk::{RiskConfig, RiskInputs, Verdict};
use strategy::{Breakout, Context, MrOfi, Runner, RunnerOutput, StrategyView};
use tokio::sync::{broadcast, oneshot, watch};
use types::{FeedEvent, Instrument, Intent, IntentEnvelope, Level, Position, RejectionCode};

use crate::convert;
use crate::hash::{CoreState, StateHasher};
use crate::v1;

/// Tape source table. Index is the source id.
pub const SOURCES: [&str; 8] = [
    "kraken.ws",
    "record.ctl",
    "core.intent",
    "core.verdict",
    "core.exec",
    "core.hash",
    "core.strategy",
    "core.wake",
];
pub const SRC_WS: u16 = 0;
pub const SRC_CTL: u16 = 1;
pub const SRC_INTENT: u16 = 2;
pub const SRC_VERDICT: u16 = 3;
pub const SRC_EXEC: u16 = 4;
pub const SRC_HASH: u16 = 5;
/// Strategy-originated intents. Replay ignores them: the runner regenerates.
pub const SRC_STRATEGY: u16 = 6;
pub const SRC_WAKE: u16 = 7;

/// Bars handed to agents. The ring holds 512; the observer reads 15.
pub const SNAPSHOT_BARS: usize = 15;

#[derive(Debug, Clone)]
pub struct CoreConfig {
    pub instrument: Instrument,
    pub depth: u32,
    pub risk: RiskConfig,
    pub paper: PaperConfig,
    pub features: FeaturesConfig,
    pub starting_cash: i64,
    /// StateHash tape record every this many core events, besides verdicts and fills.
    pub hash_every: u64,
    /// mid_price_update coalescing window on the core clock.
    pub mid_coalesce_ns: i64,
    pub channel_depth: usize,
    /// Wake::Timer cadence, the agents' clock.
    pub timer_interval_ns: i64,
}

impl CoreConfig {
    pub fn paper_defaults(instrument: Instrument) -> Self {
        Self {
            instrument,
            depth: 10,
            risk: RiskConfig {
                max_sequence_drift: 1_000,
                price_band_bps: 50,
                risk_per_trade_bps: 100,
                max_leverage_bps: 10_000,
                max_open_orders: 5,
                max_intents_per_window: 60,
                window_ns: 60_000_000_000,
                max_drawdown_bps: 1_000,
                max_strategies_enabled: 2,
                max_size_multiplier_bps: 20_000,
                discretionary_max_qty: types::Qty(1_000_000),
                discretionary_max_intents_per_window: 5,
            },
            paper: PaperConfig {
                ack_latency_ns: 50_000_000,
                fill_latency_ns: 20_000_000,
                cancel_latency_ns: 30_000_000,
            },
            features: FeaturesConfig::default(),
            starting_cash: 1_000_000,
            hash_every: 1_000,
            mid_coalesce_ns: 250_000_000,
            channel_depth: 4_096,
            timer_interval_ns: 60_000_000_000,
        }
    }
}

pub enum CoreInput {
    Feed(FeedMsg),
    Intent {
        request: Box<v1::SubmitIntentRequest>,
        now_ns: i64,
        reply: Option<oneshot::Sender<v1::SubmitIntentResponse>>,
    },
    Tick(i64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FeatureView {
    pub rsi: Option<f64>,
    pub ema_fast: Option<f64>,
    pub ema_slow: Option<f64>,
    pub order_flow_imbalance: i64,
    pub bars: [Bar; SNAPSHOT_BARS],
    pub bar_count: usize,
    pub forming: Bar,
    pub bars_since_update: u32,
    pub volume_available: bool,
    pub book_stale: bool,
}

/// What GetState reads. Published by the loop after every input.
#[derive(Debug, Clone, PartialEq)]
pub struct StateSnapshot {
    pub sequence_id: u64,
    pub timestamp_ns: i64,
    pub bid: Option<Level>,
    pub ask: Option<Level>,
    pub position: Position,
    pub unrealized_pnl: i64,
    pub equity: i64,
    pub available_equity: i64,
    pub features: Option<FeatureView>,
    pub hash_hex: String,
    pub event_count: u64,
    pub channel_full_events: u64,
    pub venue_connected: bool,
    pub book_stale: bool,
    pub strategies: Vec<(StrategyView, Position)>,
}

impl StateSnapshot {
    fn empty(starting_cash: i64) -> Self {
        Self {
            sequence_id: 0,
            timestamp_ns: 0,
            bid: None,
            ask: None,
            position: Position::default(),
            unrealized_pnl: 0,
            equity: starting_cash,
            available_equity: starting_cash,
            features: None,
            hash_hex: String::new(),
            event_count: 0,
            channel_full_events: 0,
            venue_connected: false,
            book_stale: true,
            strategies: Vec::new(),
        }
    }
}

pub struct Core<W: Write> {
    cfg: CoreConfig,
    tape: tape::Writer<W>,
    tracker: Tracker,
    features: Features,
    venue: PaperVenue,
    runner: Runner,
    last_timer_ns: i64,
    hasher: StateHasher,
    sequence_id: u64,
    now_ns: i64,
    venue_connected: bool,
    instrument_halted: bool,
    kill_switch: bool,
    window_start_ns: i64,
    intents_in_window: u32,
    discretionary_intents_in_window: u32,
    last_mid_emit_ns: i64,
    last_mid_emitted: Option<i64>,
    last_hash_record_at: u64,
    exec_out: Vec<ExecEvent>,
    encode_buf: Vec<u8>,
    state_tx: watch::Sender<StateSnapshot>,
    events_tx: broadcast::Sender<v1::MarketEvent>,
    channel_full: Arc<AtomicU64>,
}

pub struct CoreStats {
    pub sequence_id: u64,
    pub events: u64,
    pub hash_hex: String,
    pub tracker: feed::kraken::TrackerStats,
    pub tape_records: u64,
}

impl<W: Write> Core<W> {
    pub fn new(
        cfg: CoreConfig,
        writer: W,
        channel_full: Arc<AtomicU64>,
    ) -> Result<
        (
            Self,
            watch::Receiver<StateSnapshot>,
            broadcast::Sender<v1::MarketEvent>,
        ),
        tape::Error,
    > {
        let tape = tape::Writer::new(writer, &SOURCES)?;
        let (state_tx, state_rx) = watch::channel(StateSnapshot::empty(cfg.starting_cash));
        let (events_tx, _) = broadcast::channel(1_024);
        let mut runner = Runner::new();
        runner.register(Box::new(MrOfi::new()));
        runner.register(Box::new(Breakout::new()));
        let core = Self {
            runner,
            last_timer_ns: i64::MIN / 2,
            tracker: Tracker::new(cfg.instrument.clone(), cfg.depth as usize),
            features: Features::new(cfg.features),
            venue: PaperVenue::new(
                cfg.paper.clone(),
                cfg.starting_cash,
                cfg.instrument.qty_scale,
            ),
            hasher: StateHasher::new(),
            sequence_id: 0,
            now_ns: 0,
            venue_connected: false,
            instrument_halted: false,
            kill_switch: false,
            window_start_ns: 0,
            intents_in_window: 0,
            discretionary_intents_in_window: 0,
            last_mid_emit_ns: i64::MIN / 2,
            last_mid_emitted: None,
            last_hash_record_at: 0,
            exec_out: Vec::with_capacity(64),
            encode_buf: Vec::with_capacity(4_096),
            state_tx,
            events_tx: events_tx.clone(),
            channel_full,
            tape,
            cfg,
        };
        Ok((core, state_rx, events_tx))
    }

    pub fn book(&self) -> &Book {
        self.tracker.book()
    }

    pub fn hasher(&self) -> &StateHasher {
        &self.hasher
    }

    pub fn venue(&self) -> &PaperVenue {
        &self.venue
    }

    pub fn runner(&self) -> &Runner {
        &self.runner
    }

    pub fn sequence_id(&self) -> u64 {
        self.sequence_id
    }

    pub fn set_kill_switch(&mut self, on: bool) {
        self.kill_switch = on;
    }

    pub fn stats(&self) -> CoreStats {
        CoreStats {
            sequence_id: self.sequence_id,
            events: self.hasher.events(),
            hash_hex: self.hasher.hex(),
            tracker: self.tracker.stats(),
            tape_records: self.tape.records(),
        }
    }

    pub fn flush(&mut self) -> Result<(), tape::Error> {
        self.tape.flush()
    }

    pub fn into_writer(self) -> tape::Writer<W> {
        self.tape
    }

    /// Processes one input to completion. Every tape write precedes the
    /// state change it explains.
    pub fn handle(&mut self, input: CoreInput) -> Result<(), tape::Error> {
        let mut fill = false;
        match input {
            CoreInput::Feed(FeedMsg::Raw { recv_ns, bytes }) => {
                self.now_ns = recv_ns;
                self.tape.append(recv_ns, SRC_WS, &bytes)?;
                let handled = self.tracker.on_frame(&bytes);
                for e in handled.events {
                    match e {
                        FeedEvent::Book(_) => {
                            self.sequence_id += 1;
                            self.features.on_book(self.tracker.book(), recv_ns);
                            self.venue
                                .on_book(self.tracker.book(), recv_ns, &mut self.exec_out);
                            self.emit_mid();
                        }
                        FeedEvent::Trade(t) => self.features.on_trade(&t, recv_ns),
                        FeedEvent::Resync(_) => self.features.on_stale(),
                    }
                }
                fill |= self.drain_exec()?;
                fill |= self.run_strategies()?;
            }
            CoreInput::Feed(FeedMsg::Event { recv_ns, event }) => {
                // The client's own tracker decided a Silent or Disconnected
                // resync; book and trade events it derived are ignored, the
                // core derives its own from raw frames.
                if let FeedEvent::Resync(_) = event {
                    self.now_ns = recv_ns;
                    self.tracker.expect_snapshot();
                    self.features.on_stale();
                }
            }
            CoreInput::Feed(FeedMsg::Control { recv_ns, text }) => {
                self.now_ns = recv_ns;
                self.tape.append(recv_ns, SRC_CTL, text.as_bytes())?;
                if text == "tick" {
                    fill |= self.tick(recv_ns)?;
                } else if text == "connected" {
                    self.venue_connected = true;
                    self.tracker.expect_snapshot();
                } else if text.starts_with("disconnected") {
                    self.venue_connected = false;
                    self.tracker.expect_snapshot();
                    self.features.on_stale();
                }
            }
            CoreInput::Intent {
                request,
                now_ns,
                reply,
            } => {
                self.now_ns = now_ns;
                let response = self.intent(request, now_ns)?;
                let _ = fill;
                self.drain_exec()?;
                self.advance_hash();
                self.write_hash()?;
                if let Some(reply) = reply {
                    let _ = reply.send(response);
                }
                self.publish();
                return Ok(());
            }
            CoreInput::Tick(now_ns) => {
                // Recorded so replay sees the same ticks at the same tape positions.
                self.tape.append(now_ns, SRC_CTL, b"tick")?;
                fill |= self.tick(now_ns)?;
            }
        }
        self.advance_hash();
        if fill || self.hasher.events() - self.last_hash_record_at >= self.cfg.hash_every {
            self.write_hash()?;
        }
        self.publish();
        Ok(())
    }

    fn intent(
        &mut self,
        request: Box<v1::SubmitIntentRequest>,
        now_ns: i64,
    ) -> Result<v1::SubmitIntentResponse, tape::Error> {
        self.encode_buf.clear();
        request.encode(&mut self.encode_buf).expect("encode intent");
        self.tape.append(now_ns, SRC_INTENT, &self.encode_buf)?;
        self.roll_window(now_ns);
        let response = match convert::envelope(&request) {
            Err(why) => {
                let r = convert::invalid_response(&request.intent_id, why, now_ns);
                self.write_verdict(&r, now_ns)?;
                r
            }
            Ok(env) => self.process_envelope(&env, now_ns)?,
        };
        Ok(response)
    }

    /// Bars close, due acks and fills drain, strategies run, the timer wakes.
    fn tick(&mut self, now_ns: i64) -> Result<bool, tape::Error> {
        let mut fill = false;
        self.now_ns = self.now_ns.max(now_ns);
        self.features.advance(now_ns);
        if self.tracker.book().best_bid().is_some() {
            self.venue
                .on_book(self.tracker.book(), now_ns, &mut self.exec_out);
            fill |= self.drain_exec()?;
        }
        self.emit_mid();
        fill |= self.run_strategies()?;
        if self.now_ns - self.last_timer_ns >= self.cfg.timer_interval_ns {
            self.last_timer_ns = self.now_ns;
            self.wake(v1::Wake {
                reason: v1::WakeReason::Timer as i32,
                setup: None,
                detail: String::new(),
            })?;
        }
        Ok(fill)
    }

    fn roll_window(&mut self, now_ns: i64) {
        if now_ns - self.window_start_ns >= self.cfg.risk.window_ns {
            self.window_start_ns = now_ns;
            self.intents_in_window = 0;
            self.discretionary_intents_in_window = 0;
        }
    }

    fn write_verdict(
        &mut self,
        r: &v1::SubmitIntentResponse,
        now_ns: i64,
    ) -> Result<(), tape::Error> {
        self.encode_buf.clear();
        r.encode(&mut self.encode_buf).expect("encode verdict");
        self.tape.append(now_ns, SRC_VERDICT, &self.encode_buf)
    }

    /// Gate, then act. Strategy-originated envelopes come here too, after
    /// their own tape record under core.strategy.
    fn process_envelope(
        &mut self,
        env: &IntentEnvelope,
        now_ns: i64,
    ) -> Result<v1::SubmitIntentResponse, tape::Error> {
        let summaries = self.runner.summaries();
        let verdict = {
            let inputs = self.risk_inputs(now_ns, &summaries);
            risk::check(&self.cfg.risk, &inputs, env)
        };
        let mut order_id = 0;
        let verdict = match verdict {
            Verdict::Approved => {
                if matches!(
                    env.intent,
                    Intent::Place { .. } | Intent::Flatten | Intent::FlattenAll
                ) {
                    self.intents_in_window += 1;
                    if env.agent_id == risk::DISCRETIONARY {
                        self.discretionary_intents_in_window += 1;
                    }
                }
                self.act(env, now_ns, &mut order_id)
            }
            rejected => {
                self.runner.on_gate_rejected(&env.agent_id);
                rejected
            }
        };
        let response = convert::response(&env.intent_id, verdict, order_id, now_ns);
        self.write_verdict(&response, now_ns)?;
        Ok(response)
    }

    /// The approved intent's effect. Verbs go to the runner, orders to exec.
    fn act(&mut self, env: &IntentEnvelope, now_ns: i64, order_id: &mut u64) -> Verdict {
        let outputs = match &env.intent {
            Intent::Allocate(a) => {
                let position = self.venue.strategy_position(&a.strategy_id);
                let outs = {
                    let snap = self.features.snapshot();
                    let ctx = context(&self.tracker, &self.cfg, self.sequence_id, &snap, now_ns);
                    self.runner.allocate(&ctx, a, position)
                };
                outs
            }
            Intent::Tune {
                strategy_id,
                param,
                value,
            } => {
                if self.runner.tune(strategy_id, param, *value).is_err() {
                    return Verdict::Rejected {
                        code: RejectionCode::InvalidIntent,
                        reason: "unknown strategy or param",
                    };
                }
                Vec::new()
            }
            Intent::ConfirmSetup {
                strategy_id,
                setup_id,
                size_multiplier_bps,
            } => {
                let snap = self.features.snapshot();
                let ctx = context(&self.tracker, &self.cfg, self.sequence_id, &snap, now_ns);
                self.runner
                    .confirm(&ctx, strategy_id, *setup_id, *size_multiplier_bps)
                    .into_iter()
                    .collect()
            }
            Intent::RejectSetup {
                strategy_id,
                setup_id,
            } => {
                self.runner.reject(strategy_id, *setup_id);
                Vec::new()
            }
            _ => {
                return match self
                    .venue
                    .submit(env, self.tracker.book(), now_ns, &mut self.exec_out)
                {
                    Ok(id) => {
                        *order_id = id;
                        Verdict::Approved
                    }
                    Err(e) => Verdict::Rejected {
                        code: RejectionCode::InvalidIntent,
                        reason: exec_reason(e),
                    },
                };
            }
        };
        // Runner outputs from a verb (a disable's flatten, a confirmed order)
        // go through the gate like any strategy order. Their tape writes and
        // hash updates are handled by the caller's normal path.
        if let Err(e) = self.handle_runner_outputs(outputs, now_ns) {
            tracing::error!(error = %e, "tape write during verb");
        }
        Verdict::Approved
    }

    /// One runner pass after a core event. Returns whether a fill happened
    /// while acting on its outputs.
    fn run_strategies(&mut self) -> Result<bool, tape::Error> {
        let now_ns = self.now_ns;
        let outputs = {
            let snap = self.features.snapshot();
            let ctx = context(&self.tracker, &self.cfg, self.sequence_id, &snap, now_ns);
            let venue = &self.venue;
            self.runner.on_event(&ctx, &|id| {
                (venue.strategy_position(id), venue.open_orders_of(id) as u32)
            })
        };
        self.handle_runner_outputs(outputs, now_ns)
    }

    fn handle_runner_outputs(
        &mut self,
        outputs: Vec<RunnerOutput>,
        now_ns: i64,
    ) -> Result<bool, tape::Error> {
        let mut fill = false;
        for o in outputs {
            match o {
                RunnerOutput::Intent {
                    mut envelope,
                    effective_multiplier_bps,
                } => {
                    // The effective multiplier rides on the intent id so the
                    // tape record carries it without a proto field.
                    envelope.intent_id =
                        format!("{}@m{effective_multiplier_bps}", envelope.intent_id);
                    self.encode_buf.clear();
                    convert::request(&envelope)
                        .encode(&mut self.encode_buf)
                        .expect("encode strategy intent");
                    self.tape.append(now_ns, SRC_STRATEGY, &self.encode_buf)?;
                    self.roll_window(now_ns);
                    self.process_envelope(&envelope, now_ns)?;
                    fill |= self.drain_exec()?;
                }
                RunnerOutput::Wake(w) => {
                    let snapshot = crate::server::state_response(
                        &self.state_tx.borrow(),
                        &self.cfg.instrument.venue,
                        &self.cfg.instrument.symbol,
                    );
                    self.wake(v1::Wake {
                        reason: v1::WakeReason::SetupActive as i32,
                        setup: Some(v1::SetupActive {
                            strategy_id: w.strategy_id.to_string(),
                            setup_id: w.setup_id,
                            order: Some(convert::proposed_order(&w.order)),
                            snapshot: Some(snapshot),
                            ttl_ms: w.ttl_ns / 1_000_000,
                        }),
                        detail: String::new(),
                    })?;
                }
                RunnerOutput::Expired {
                    strategy_id,
                    setup_id,
                } => {
                    let text = format!("expired {strategy_id} {setup_id}");
                    self.tape.append(now_ns, SRC_STRATEGY, text.as_bytes())?;
                }
            }
        }
        Ok(fill)
    }

    /// Recorded on the tape, then streamed. Never coalesced.
    fn wake(&mut self, wake: v1::Wake) -> Result<(), tape::Error> {
        self.encode_buf.clear();
        wake.encode(&mut self.encode_buf).expect("encode wake");
        self.tape.append(self.now_ns, SRC_WAKE, &self.encode_buf)?;
        let _ = self
            .events_tx
            .send(self.market_event(v1::market_event::Event::Wake(wake)));
        Ok(())
    }

    fn risk_inputs<'a>(
        &'a self,
        now_ns: i64,
        strategies: &'a [risk::StrategySummary],
    ) -> RiskInputs<'a> {
        let acct = self.venue.account();
        let mark = self.tracker.book().mid();
        RiskInputs {
            instrument: &self.cfg.instrument,
            book: self.tracker.book(),
            book_sequence_id: self.sequence_id,
            venue_connected: self.venue_connected,
            instrument_halted: self.instrument_halted,
            position: acct.position,
            equity: mark.map_or(self.cfg.starting_cash, |m| acct.equity(m)),
            peak_equity: acct.peak_equity,
            open_orders: self.venue.open_orders() as u32,
            intents_in_window: self.intents_in_window,
            discretionary_intents_in_window: self.discretionary_intents_in_window,
            kill_switch: self.kill_switch,
            now_ns,
            strategies,
        }
    }

    /// Writes exec events to the tape and streams position updates. Returns
    /// whether a fill happened.
    fn drain_exec(&mut self) -> Result<bool, tape::Error> {
        let mut fill = false;
        let unrealized = self.unrealized();
        let mut after_fill = false;
        let mut out = std::mem::take(&mut self.exec_out);
        for e in out.drain(..) {
            let is_position = matches!(e, ExecEvent::Position { .. });
            if is_position && !after_fill {
                continue; // periodic marks are derivable; only post-fill positions go on tape
            }
            after_fill = matches!(e, ExecEvent::Fill { .. });
            fill |= after_fill;
            self.encode_buf.clear();
            convert::exec_event(&e, unrealized)
                .encode(&mut self.encode_buf)
                .expect("encode exec event");
            self.tape.append(self.now_ns, SRC_EXEC, &self.encode_buf)?;
            if let ExecEvent::Position { position, .. } = e {
                let _ = self.events_tx.send(self.market_event(
                    v1::market_event::Event::PositionUpdate(convert::position(
                        position, unrealized,
                    )),
                ));
            }
        }
        Ok(fill)
    }

    fn emit_mid(&mut self) {
        let Some(mid) = self.tracker.book().mid() else {
            return;
        };
        if self.last_mid_emitted == Some(mid.0) {
            return;
        }
        if self.now_ns - self.last_mid_emit_ns < self.cfg.mid_coalesce_ns {
            return;
        }
        self.last_mid_emit_ns = self.now_ns;
        self.last_mid_emitted = Some(mid.0);
        let _ = self
            .events_tx
            .send(self.market_event(v1::market_event::Event::MidPriceUpdate(mid.0)));
    }

    fn market_event(&self, event: v1::market_event::Event) -> v1::MarketEvent {
        v1::MarketEvent {
            sequence_id: self.sequence_id,
            timestamp_ns: self.now_ns,
            venue: self.cfg.instrument.venue.clone(),
            symbol: self.cfg.instrument.symbol.clone(),
            event: Some(event),
        }
    }

    fn advance_hash(&mut self) {
        let next = self.hasher.compute(&self.state());
        self.hasher.commit(next);
    }

    fn state(&self) -> CoreState<'_> {
        let snap = self.features.snapshot();
        CoreState {
            sequence_id: self.sequence_id,
            book: self.tracker.book(),
            venue: &self.venue,
            runner: &self.runner,
            bar_count: snap.bars.len(),
            last_bar: snap.bars.last().copied(),
            order_flow_imbalance: snap.order_flow_imbalance,
            open_orders: self.venue.open_orders() as u32,
            intents_in_window: self.intents_in_window,
            kill_switch: self.kill_switch,
        }
    }

    fn write_hash(&mut self) -> Result<(), tape::Error> {
        self.last_hash_record_at = self.hasher.events();
        let rec = v1::StateHash {
            sequence_id: self.sequence_id,
            event_count: self.hasher.events(),
            hash: self.hasher.current().to_vec(),
        };
        self.encode_buf.clear();
        rec.encode(&mut self.encode_buf).expect("encode hash");
        self.tape.append(self.now_ns, SRC_HASH, &self.encode_buf)
    }

    fn unrealized(&self) -> i64 {
        self.tracker
            .book()
            .mid()
            .map_or(0, |m| self.venue.account().unrealized(m))
    }

    fn publish(&mut self) {
        let book = self.tracker.book();
        let acct = self.venue.account();
        let mid = book.mid();
        let equity = mid.map_or(self.cfg.starting_cash, |m| acct.equity(m));
        let qty_unit = 10i128.pow(self.cfg.instrument.qty_scale);
        let committed: i128 = self
            .venue
            .orders()
            .filter(|o| o.kind == exec::OrderKind::Limit && !o.is_terminal())
            .map(|o| o.remaining().0 as i128 * o.price.0 as i128 / qty_unit)
            .sum();
        let snap = self.features.snapshot();
        let mut bars = [Bar::default(); SNAPSHOT_BARS];
        let tail = snap.bars.len().saturating_sub(SNAPSHOT_BARS);
        let shown = &snap.bars[tail..];
        bars[..shown.len()].copy_from_slice(shown);
        let state = StateSnapshot {
            sequence_id: self.sequence_id,
            timestamp_ns: self.now_ns,
            bid: book.best_bid(),
            ask: book.best_ask(),
            position: acct.position,
            unrealized_pnl: self.unrealized(),
            equity,
            available_equity: i64::try_from(equity as i128 - committed).unwrap_or(0),
            features: Some(FeatureView {
                rsi: snap.rsi,
                ema_fast: snap.ema_fast,
                ema_slow: snap.ema_slow,
                order_flow_imbalance: snap.order_flow_imbalance,
                bars,
                bar_count: shown.len(),
                forming: snap.forming,
                bars_since_update: snap.bars_since_update,
                volume_available: snap.volume_available,
                book_stale: snap.book_stale,
            }),
            hash_hex: self.hasher.hex(),
            event_count: self.hasher.events(),
            channel_full_events: self.channel_full.load(Ordering::Relaxed),
            venue_connected: self.venue_connected,
            book_stale: book.is_stale(),
            strategies: self
                .runner
                .views()
                .into_iter()
                .map(|v| {
                    let p = self.venue.strategy_position(v.id);
                    (v, p)
                })
                .collect(),
        };
        self.state_tx.send_replace(state);
    }
}

/// Borrows only what the runner needs, so the runner itself can be mutable.
fn context<'a>(
    tracker: &'a Tracker,
    cfg: &'a CoreConfig,
    sequence_id: u64,
    snap: &'a features::Snapshot<'a>,
    now_ns: i64,
) -> Context<'a> {
    Context {
        now_ns,
        sequence_id,
        book: tracker.book(),
        features: snap,
        venue: &cfg.instrument.venue,
        symbol: &cfg.instrument.symbol,
    }
}

fn exec_reason(e: ExecError) -> &'static str {
    match e {
        ExecError::UnknownOrder(_) => "unknown order",
        ExecError::NotOpen(_) => "order not open",
        ExecError::EmptySide(_) => "book side empty",
        ExecError::Flat => "nothing to flatten",
        ExecError::UnknownStrategy(_) => "unknown strategy",
    }
}

/// The live side of the core: senders for feed and gRPC, readers for state.
pub struct CoreHandle {
    tx: SyncSender<CoreInput>,
    pub state: watch::Receiver<StateSnapshot>,
    pub events: broadcast::Sender<v1::MarketEvent>,
    pub channel_full: Arc<AtomicU64>,
    pub join: JoinHandle<CoreStats>,
}

impl CoreHandle {
    /// Blocks when the channel is full, counts the block, never drops.
    /// Call from `spawn_blocking` on tokio. Err carries the input back only
    /// when the core has stopped.
    #[allow(clippy::result_large_err)]
    pub fn send(&self, input: CoreInput) -> Result<(), CoreInput> {
        match self.tx.try_send(input) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(input)) => {
                self.channel_full.fetch_add(1, Ordering::Relaxed);
                self.tx.send(input).map_err(|e| e.0)
            }
            Err(TrySendError::Disconnected(input)) => Err(input),
        }
    }

    pub fn sender(&self) -> SyncSender<CoreInput> {
        self.tx.clone()
    }

    /// Drops this handle's sender and waits for the loop to flush and exit.
    /// Every other sender clone must already be gone.
    pub fn shutdown(self) -> CoreStats {
        drop(self.tx);
        self.join.join().expect("core thread")
    }
}

/// Starts the loop on its own OS thread. Dropping every sender ends it; the
/// thread flushes the tape and returns its stats.
pub fn spawn_core<W: Write + Send + 'static>(
    cfg: CoreConfig,
    writer: W,
) -> Result<CoreHandle, tape::Error> {
    let channel_full = Arc::new(AtomicU64::new(0));
    let depth = cfg.channel_depth;
    let (mut core, state, events) = Core::new(cfg, writer, channel_full.clone())?;
    let (tx, rx): (SyncSender<CoreInput>, Receiver<CoreInput>) = sync_channel(depth);
    let join = std::thread::Builder::new()
        .name("agenttrade-core".into())
        .spawn(move || {
            for input in rx {
                if let Err(e) = core.handle(input) {
                    tracing::error!(error = %e, "tape write failed; stopping core");
                    break;
                }
            }
            if let Err(e) = core.flush() {
                tracing::error!(error = %e, "tape flush failed");
            }
            core.stats()
        })
        .expect("spawn core thread");
    Ok(CoreHandle {
        tx,
        state,
        events,
        channel_full,
        join,
    })
}

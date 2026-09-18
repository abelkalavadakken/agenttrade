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
use tokio::sync::{broadcast, oneshot, watch};
use types::{FeedEvent, Instrument, Intent, Level, Position, RejectionCode};

use crate::convert;
use crate::hash::{CoreState, StateHasher};
use crate::v1;

/// Tape source table. Index is the source id.
pub const SOURCES: [&str; 6] = [
    "kraken.ws",
    "record.ctl",
    "core.intent",
    "core.verdict",
    "core.exec",
    "core.hash",
];
pub const SRC_WS: u16 = 0;
pub const SRC_CTL: u16 = 1;
pub const SRC_INTENT: u16 = 2;
pub const SRC_VERDICT: u16 = 3;
pub const SRC_EXEC: u16 = 4;
pub const SRC_HASH: u16 = 5;

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
        }
    }
}

pub struct Core<W: Write> {
    cfg: CoreConfig,
    tape: tape::Writer<W>,
    tracker: Tracker,
    features: Features,
    venue: PaperVenue,
    hasher: StateHasher,
    sequence_id: u64,
    now_ns: i64,
    venue_connected: bool,
    instrument_halted: bool,
    kill_switch: bool,
    window_start_ns: i64,
    intents_in_window: u32,
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
        let core = Self {
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
                if text == "connected" {
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
                self.now_ns = self.now_ns.max(now_ns);
                self.features.advance(now_ns);
                if self.tracker.book().best_bid().is_some() {
                    self.venue
                        .on_book(self.tracker.book(), now_ns, &mut self.exec_out);
                    fill |= self.drain_exec()?;
                }
                self.emit_mid();
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

        if now_ns - self.window_start_ns >= self.cfg.risk.window_ns {
            self.window_start_ns = now_ns;
            self.intents_in_window = 0;
        }
        let response = match convert::envelope(&request) {
            Err(why) => convert::invalid_response(&request.intent_id, why, now_ns),
            Ok(env) => {
                let inputs = self.risk_inputs(now_ns);
                let verdict = risk::check(&self.cfg.risk, &inputs, &env);
                let mut order_id = 0;
                let verdict = match verdict {
                    Verdict::Approved => {
                        if matches!(env.intent, Intent::Place { .. } | Intent::Flatten) {
                            self.intents_in_window += 1;
                        }
                        match self.venue.submit(
                            &env,
                            self.tracker.book(),
                            now_ns,
                            &mut self.exec_out,
                        ) {
                            Ok(id) => {
                                order_id = id;
                                Verdict::Approved
                            }
                            Err(e) => Verdict::Rejected {
                                code: RejectionCode::InvalidIntent,
                                reason: exec_reason(e),
                            },
                        }
                    }
                    rejected => rejected,
                };
                convert::response(&env.intent_id, verdict, order_id, now_ns)
            }
        };
        self.encode_buf.clear();
        response
            .encode(&mut self.encode_buf)
            .expect("encode verdict");
        self.tape.append(now_ns, SRC_VERDICT, &self.encode_buf)?;
        Ok(response)
    }

    fn risk_inputs(&self, now_ns: i64) -> RiskInputs<'_> {
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
            kill_switch: self.kill_switch,
            now_ns,
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
        };
        self.state_tx.send_replace(state);
    }
}

fn exec_reason(e: ExecError) -> &'static str {
    match e {
        ExecError::UnknownOrder(_) => "unknown order",
        ExecError::NotOpen(_) => "order not open",
        ExecError::EmptySide(_) => "book side empty",
        ExecError::Flat => "nothing to flatten",
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

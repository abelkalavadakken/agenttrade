//! The runner over the tape fixtures: gating, expiry, confirm, disable, determinism.

use book::Book;
use features::{Features, FeaturesConfig};
use feed::kraken::Tracker;
use strategy::{
    Action, Breakout, Context, Horizon, MrOfi, Order, Param, ParamError, Runner, RunnerOutput,
    SetupState, State, Strategy,
};
use types::{
    instruments, Allocation, FeedEvent, Intent, OnDisable, Position, Price, Qty, Side, TimeInForce,
};

const BOOK: &str = include_str!("../../../../tests/fixtures/kraken_book_btcusd.jsonl");
const S: i64 = 1_000_000_000;

/// Gated test double: proposes one buy on the first event and holds it.
struct AlwaysSetup {
    setup: SetupState,
    cleared: u32,
    params: [Param; 1],
}

impl AlwaysSetup {
    fn new() -> Self {
        Self {
            setup: SetupState::None,
            cleared: 0,
            params: [Param {
                name: "ttl_ns",
                value: 10 * S,
                min: S,
                max: 100 * S,
            }],
        }
    }
}

impl Strategy for AlwaysSetup {
    fn id(&self) -> &'static str {
        "gated"
    }
    fn horizon(&self) -> Horizon {
        Horizon::Minutes
    }
    fn params(&self) -> &[Param] {
        &self.params
    }
    fn set_param(&mut self, name: &str, value: i64) -> Result<(), ParamError> {
        strategy::set_bounded(&mut self.params, name, value)
    }
    fn on_event(&mut self, st: &State) -> Option<Action> {
        if self.setup == SetupState::None && self.cleared == 0 {
            let ask = st.book.best_ask()?.price;
            self.setup = SetupState::Active {
                setup_id: st.sequence_id,
                order: Order {
                    side: Side::Buy,
                    price: ask,
                    stop: Price(ask.0 - 1_000),
                    qty: Qty(2_000_000),
                    tif: TimeInForce::Gtc,
                    reason: "test",
                },
            };
        }
        None
    }
    fn setup_state(&self) -> SetupState {
        self.setup
    }
    fn on_setup_cleared(&mut self) {
        self.setup = SetupState::None;
        self.cleared += 1;
    }
    fn reset(&mut self) {
        self.setup = SetupState::None;
    }
}

/// Autonomous test double: one buy at the ask on the first event, then nothing.
struct OneShot(bool);

impl Strategy for OneShot {
    fn id(&self) -> &'static str {
        "oneshot"
    }
    fn horizon(&self) -> Horizon {
        Horizon::Seconds
    }
    fn params(&self) -> &[Param] {
        &[]
    }
    fn set_param(&mut self, _: &str, _: i64) -> Result<(), ParamError> {
        Err(ParamError::Unknown)
    }
    fn on_event(&mut self, st: &State) -> Option<Action> {
        if self.0 {
            return None;
        }
        self.0 = true;
        let ask = st.book.best_ask()?.price;
        Some(Action::Place(Order {
            side: Side::Buy,
            price: ask,
            stop: Price(ask.0 - 1_000),
            qty: Qty(1_000_000),
            tif: TimeInForce::Gtc,
            reason: "test",
        }))
    }
    fn setup_state(&self) -> SetupState {
        SetupState::None
    }
    fn reset(&mut self) {
        self.0 = false;
    }
}

struct Harness {
    tracker: Tracker,
    features: Features,
    lines: Vec<&'static str>,
    i: usize,
}

impl Harness {
    fn new() -> Self {
        Self {
            tracker: Tracker::new(instruments::find("kraken", "BTC/USD").unwrap(), 10),
            features: Features::new(FeaturesConfig::default()),
            lines: BOOK.lines().filter(|l| !l.is_empty()).collect(),
            i: 0,
        }
    }

    /// Feeds frames until one produces a book event. Returns (now_ns, seq) or None at the end.
    fn step(&mut self, seq: &mut u64) -> Option<i64> {
        while self.i < self.lines.len() {
            let ns = self.i as i64 * S;
            let h = self.tracker.on_frame(self.lines[self.i].as_bytes());
            self.i += 1;
            if h.events.iter().any(|e| matches!(e, FeedEvent::Book(_))) {
                *seq += 1;
                self.features.on_book(self.tracker.book(), ns);
                return Some(ns);
            }
        }
        None
    }

    fn book(&self) -> &Book {
        self.tracker.book()
    }
}

fn flat(_: &str) -> (Position, u32) {
    (Position::default(), 0)
}

fn enable(runner: &mut Runner, ctx: &Context, id: &str, bps: i64) {
    runner.allocate(
        ctx,
        &Allocation {
            strategy_id: id.into(),
            enabled: true,
            size_multiplier_bps: bps,
            on_disable: OnDisable::Flatten,
        },
        Position::default(),
    );
}

fn ctx<'a>(h: &'a Harness, f: &'a features::Snapshot<'a>, now_ns: i64, seq: u64) -> Context<'a> {
    Context {
        now_ns,
        sequence_id: seq,
        book: h.book(),
        features: f,
        venue: "kraken",
        symbol: "BTC/USD",
    }
}

#[test]
fn autonomous_order_becomes_an_intent_with_the_multiplier_applied() {
    let mut h = Harness::new();
    let mut r = Runner::new();
    r.register(Box::new(OneShot(false)));
    let mut seq = 0;
    let now = h.step(&mut seq).unwrap();
    let f = h.features.snapshot();
    let c = ctx(&h, &f, now, seq);
    assert!(
        r.on_event(&c, &flat).is_empty(),
        "disabled strategies do not run"
    );
    enable(&mut r, &c, "oneshot", 5_000);
    let out = r.on_event(&c, &flat);
    let [RunnerOutput::Intent {
        envelope,
        effective_multiplier_bps,
    }] = out.as_slice()
    else {
        panic!("{out:?}");
    };
    assert_eq!(*effective_multiplier_bps, 5_000);
    assert_eq!(envelope.agent_id, "oneshot");
    assert_eq!(envelope.source_sequence_id, seq);
    let Intent::Place { qty, price, .. } = envelope.intent else {
        panic!()
    };
    assert_eq!(qty, Qty(500_000), "1_000_000 at 0.5x");
    assert_eq!(price, h.book().best_ask().unwrap().price);
    assert_eq!(r.views()[0].counters.orders_sent, 1);
    r.on_gate_rejected("oneshot");
    assert_eq!(r.views()[0].counters.orders_rejected_by_gate, 1);
}

#[test]
fn gated_setup_wakes_once_expires_after_ttl_and_is_not_re_asked() {
    let mut h = Harness::new();
    let mut r = Runner::new();
    r.register(Box::new(AlwaysSetup::new()));
    let mut seq = 0;
    let now = h.step(&mut seq).unwrap();
    let f = h.features.snapshot();
    let c = ctx(&h, &f, now, seq);
    enable(&mut r, &c, "gated", 10_000);
    let out = r.on_event(&c, &flat);
    let [RunnerOutput::Wake(w)] = out.as_slice() else {
        panic!("{out:?}")
    };
    assert_eq!(
        (w.strategy_id, w.setup_id, w.ttl_ns),
        ("gated", seq, 10 * S)
    );
    assert_eq!(r.summaries()[0].setup_active, Some(seq));
    // Next events: nothing until the TTL passes.
    let mut expired = Vec::new();
    for _ in 0..12 {
        let Some(now) = h.step(&mut seq) else { break };
        let f = h.features.snapshot();
        let c = ctx(&h, &f, now, seq);
        for o in r.on_event(&c, &flat) {
            match o {
                RunnerOutput::Wake(_) => panic!("re-asked while held"),
                RunnerOutput::Intent { .. } => panic!("silence never trades"),
                RunnerOutput::Expired { setup_id, .. } => expired.push(setup_id),
            }
        }
    }
    assert_eq!(expired, vec![1]);
    let v = &r.views()[0];
    assert_eq!((v.counters.setups, v.counters.expired), (1, 1));
    assert_eq!(v.pending, None);
    assert_eq!(r.summaries()[0].setup_active, None);
}

#[test]
fn confirm_clamps_to_the_allocation_and_wrong_ids_do_nothing() {
    let mut h = Harness::new();
    let mut r = Runner::new();
    r.register(Box::new(AlwaysSetup::new()));
    let mut seq = 0;
    let now = h.step(&mut seq).unwrap();
    let f = h.features.snapshot();
    let c = ctx(&h, &f, now, seq);
    enable(&mut r, &c, "gated", 5_000);
    r.on_event(&c, &flat);
    assert!(
        r.confirm(&c, "gated", 999, 10_000).is_none(),
        "wrong setup id"
    );
    assert!(
        r.confirm(&c, "nope", seq, 10_000).is_none(),
        "unknown strategy"
    );
    assert!(!r.reject("gated", 999), "wrong setup id");
    let Some(RunnerOutput::Intent {
        envelope,
        effective_multiplier_bps,
    }) = r.confirm(&c, "gated", seq, 20_000)
    else {
        panic!("expected intent");
    };
    assert_eq!(effective_multiplier_bps, 5_000, "clamped to the allocation");
    let Intent::Place { qty, .. } = envelope.intent else {
        panic!()
    };
    assert_eq!(qty, Qty(1_000_000));
    let v = &r.views()[0];
    assert_eq!((v.counters.confirmed, v.counters.orders_sent), (1, 1));
    assert!(
        r.confirm(&c, "gated", seq, 10_000).is_none(),
        "already released"
    );
}

#[test]
fn confirm_with_zero_skips_and_reject_clears() {
    let mut h = Harness::new();
    let mut r = Runner::new();
    r.register(Box::new(AlwaysSetup::new()));
    let mut seq = 0;
    let now = h.step(&mut seq).unwrap();
    let f = h.features.snapshot();
    let c = ctx(&h, &f, now, seq);
    enable(&mut r, &c, "gated", 10_000);
    r.on_event(&c, &flat);
    assert!(r.confirm(&c, "gated", seq, 0).is_none());
    assert_eq!(r.views()[0].counters.rejected, 1);
    assert_eq!(r.views()[0].pending, None);
    // A fresh strategy, reject path.
    let mut r = Runner::new();
    r.register(Box::new(AlwaysSetup::new()));
    enable(&mut r, &c, "gated", 10_000);
    r.on_event(&c, &flat);
    assert!(r.reject("gated", seq));
    assert!(!r.reject("gated", seq), "already cleared");
    assert_eq!(r.views()[0].counters.rejected, 1);
}

#[test]
fn disable_flatten_emits_flatten_and_hold_does_not() {
    let mut h = Harness::new();
    let mut r = Runner::new();
    r.register(Box::new(OneShot(false)));
    let mut seq = 0;
    let now = h.step(&mut seq).unwrap();
    let f = h.features.snapshot();
    let c = ctx(&h, &f, now, seq);
    enable(&mut r, &c, "oneshot", 10_000);
    let long = Position {
        net_qty: Qty(1_000_000),
        ..Default::default()
    };
    let disable = |on_disable| Allocation {
        strategy_id: "oneshot".into(),
        enabled: false,
        size_multiplier_bps: 10_000,
        on_disable,
    };
    let out = r.allocate(&c, &disable(OnDisable::Flatten), long);
    let [RunnerOutput::Intent { envelope, .. }] = out.as_slice() else {
        panic!("{out:?}")
    };
    assert_eq!(envelope.intent, Intent::Flatten);
    assert_eq!(envelope.agent_id, "oneshot");
    enable(&mut r, &c, "oneshot", 10_000);
    assert!(r.allocate(&c, &disable(OnDisable::Hold), long).is_empty());
    assert!(
        r.allocate(&c, &disable(OnDisable::Flatten), Position::default())
            .is_empty(),
        "flat: nothing to flatten"
    );
}

#[test]
fn tune_goes_through_bounds() {
    let mut r = Runner::new();
    r.register(Box::new(MrOfi::new()));
    r.register(Box::new(Breakout::new()));
    assert_eq!(r.tune("mr_ofi", "stop_ticks", 100), Ok(()));
    assert!(matches!(
        r.tune("mr_ofi", "stop_ticks", 4),
        Err(ParamError::OutOfBounds { .. })
    ));
    assert_eq!(r.tune("mr_ofi", "nope", 1), Err(ParamError::Unknown));
    assert_eq!(r.tune("nope", "size", 1), Err(ParamError::Unknown));
    let s = r.summaries();
    assert_eq!(s.len(), 2);
    assert_eq!(
        s[0].params
            .iter()
            .find(|p| p.name == "stop_ticks")
            .map(|p| (p.min, p.max)),
        Some((5, 5_000))
    );
}

#[test]
fn runner_output_and_hash_are_identical_across_runs() {
    fn run() -> (Vec<String>, Vec<u8>) {
        let mut h = Harness::new();
        let mut r = Runner::new();
        r.register(Box::new(MrOfi::new()));
        r.register(Box::new(Breakout::new()));
        r.register(Box::new(AlwaysSetup::new()));
        r.tune("mr_ofi", "ofi_threshold", 10_000_000).unwrap();
        r.tune("mr_ofi", "min_spread_ticks", 1).unwrap();
        let mut seq = 0;
        let mut log = Vec::new();
        let mut first = true;
        while let Some(now) = h.step(&mut seq) {
            let f = h.features.snapshot();
            let c = ctx(&h, &f, now, seq);
            if first {
                enable(&mut r, &c, "mr_ofi", 10_000);
                enable(&mut r, &c, "breakout", 10_000);
                enable(&mut r, &c, "gated", 10_000);
                first = false;
            }
            for o in r.on_event(&c, &flat) {
                log.push(format!("{o:?}"));
            }
        }
        let mut bytes = Vec::new();
        r.hash_into(&mut |b| bytes.extend_from_slice(b));
        (log, bytes)
    }
    let (a, ha) = run();
    let (b, hb) = run();
    assert_eq!(a, b);
    assert_eq!(ha, hb);
    assert!(
        a.iter().any(|l| l.starts_with("Intent")),
        "mr_ofi fired at least once on the fixture"
    );
    assert!(
        a.iter().any(|l| l.starts_with("Wake")),
        "the gated double asked once"
    );
}

//! The gate for unfreezing agents: a tape recorded by the entrypoint replays
//! with the autonomous strategy firing and filling, the gated strategy waking
//! and expiring unconfirmed, the gate passing and rejecting, and zero hash
//! mismatches. tests/fixtures/acceptance.tape was recorded live on Kraken.

use std::fs::File;

use api::v1;
use api::{Core, CoreConfig, CoreInput, SOURCES};
use feed::FeedMsg;
use prost::Message;
use tape::{Mode, Reader};
use types::instruments;

const TAPE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/acceptance.tape"
);

#[test]
fn acceptance_tape_replays_end_to_end() {
    let cfg = CoreConfig::paper_defaults(instruments::find("kraken", "BTC/USD").unwrap());
    let (mut core, _s, _e) = Core::new(cfg, std::io::sink(), Default::default()).unwrap();
    let mut reader = Reader::new(File::open(TAPE).unwrap(), Mode::Fast).unwrap();
    assert_eq!(reader.sources(), &SOURCES.map(String::from));
    let (mut verified, mut mismatches) = (0u64, 0u64);
    let (mut approved, mut rejected) = (0u64, 0u64);
    let mut strategy_fills = std::collections::BTreeMap::<String, u64>::new();
    let mut wakes = std::collections::BTreeMap::<i32, u64>::new();
    let mut expiries = 0u64;
    let mut records = 0u64;
    while let Some(rec) = reader.next_record().unwrap() {
        records += 1;
        match rec.source_id {
            0 => core
                .handle(CoreInput::Feed(FeedMsg::Raw {
                    recv_ns: rec.recv_ns,
                    bytes: rec.bytes,
                }))
                .unwrap(),
            1 => core
                .handle(CoreInput::Feed(FeedMsg::Control {
                    recv_ns: rec.recv_ns,
                    text: String::from_utf8(rec.bytes).unwrap(),
                }))
                .unwrap(),
            2 => core
                .handle(CoreInput::Intent {
                    request: Box::new(
                        v1::SubmitIntentRequest::decode(rec.bytes.as_slice()).unwrap(),
                    ),
                    now_ns: rec.recv_ns,
                    reply: None,
                })
                .unwrap(),
            3 => {
                let v = v1::SubmitIntentResponse::decode(rec.bytes.as_slice()).unwrap();
                if v.decision == v1::RiskDecision::Approved as i32 {
                    approved += 1
                } else {
                    rejected += 1
                }
            }
            4 => {
                if let Some(v1::exec_event::Event::Fill(f)) =
                    v1::ExecEvent::decode(rec.bytes.as_slice()).unwrap().event
                {
                    *strategy_fills.entry(f.strategy_id).or_default() += 1;
                }
            }
            5 => {
                let h = v1::StateHash::decode(rec.bytes.as_slice()).unwrap();
                if h.hash.as_slice() == core.hasher().current() {
                    verified += 1
                } else {
                    mismatches += 1
                }
            }
            6 => {
                if rec.bytes.starts_with(b"expired ") {
                    expiries += 1;
                }
            }
            7 => {
                let w = v1::Wake::decode(rec.bytes.as_slice()).unwrap();
                *wakes.entry(w.reason).or_default() += 1;
            }
            _ => {}
        }
    }
    let views = core.runner().views();
    let mr = views.iter().find(|v| v.id == "mr_ofi").unwrap();
    let br = views.iter().find(|v| v.id == "breakout").unwrap();
    println!("records {records} approved {approved} rejected {rejected} fills {strategy_fills:?} wakes {wakes:?} expiries {expiries} verified {verified} mismatches {mismatches}");
    println!("mr_ofi {:?}\nbreakout {:?}", mr.counters, br.counters);

    assert!(mr.counters.orders_sent >= 1, "autonomous strategy fired");
    assert!(
        strategy_fills.get("mr_ofi").copied().unwrap_or(0) >= 1,
        "and filled"
    );
    assert!(br.counters.setups >= 1, "gated strategy emitted a setup");
    assert!(
        *wakes
            .get(&(v1::WakeReason::SetupActive as i32))
            .unwrap_or(&0)
            >= 1,
        "as a Wake"
    );
    assert!(
        br.counters.expired >= 1 && expiries >= 1,
        "and it expired unconfirmed"
    );
    assert_eq!(br.counters.confirmed, 0);
    assert!(
        *wakes.get(&(v1::WakeReason::Timer as i32)).unwrap_or(&0) >= 1,
        "timer wakes"
    );
    assert!(
        approved >= 1 && rejected >= 1,
        "gate passed {approved} and rejected {rejected}"
    );
    assert_eq!(mismatches, 0, "hash chain over {verified} records");
    assert!(verified >= 100, "verified {verified}");
}

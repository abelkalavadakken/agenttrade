//! The first 30 s of the acceptance tape, in git for fast local runs.
//! Weaker than the full test: the gated strategy needs minutes of bars.

use std::fs::File;

use api::v1;
use api::{Core, CoreConfig, CoreInput};
use feed::FeedMsg;
use prost::Message;
use tape::{Mode, Reader};
use types::instruments;

const TAPE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/acceptance-30s.tape"
);

#[test]
fn thirty_second_cut_replays_with_matching_hashes() {
    let cfg = CoreConfig::paper_defaults(instruments::find("kraken", "BTC/USD").unwrap());
    let (mut core, _s, _e) = Core::new(cfg, std::io::sink(), Default::default()).unwrap();
    let (mut verified, mut mismatches, mut approved, mut fills) = (0u64, 0u64, 0u64, 0u64);
    for rec in Reader::new(File::open(TAPE).unwrap(), Mode::Fast).unwrap() {
        let rec = rec.unwrap();
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
                    approved += 1;
                }
            }
            4 => {
                if let Some(v1::exec_event::Event::Fill(_)) =
                    v1::ExecEvent::decode(rec.bytes.as_slice()).unwrap().event
                {
                    fills += 1;
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
            _ => {}
        }
    }
    assert_eq!(mismatches, 0, "verified {verified}");
    assert!(verified >= 5, "verified {verified}");
    assert!(approved >= 9, "startup intents approved: {approved}");
    assert!(fills >= 1, "mr_ofi filled in the first 30 s");
}

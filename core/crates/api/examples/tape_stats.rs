//! Verdicts by code and agent, fills by strategy, first strategy intents. Diagnostic.
use std::collections::BTreeMap;
use std::fs::File;

use api::v1;
use prost::Message;
use tape::{Mode, Reader};

fn main() {
    let path = std::env::args().nth(1).expect("tape");
    let mut reader = Reader::new(File::open(&path).unwrap(), Mode::Fast).unwrap();
    let mut verdicts: BTreeMap<(String, i32, String), u64> = BTreeMap::new();
    let mut fills: BTreeMap<(String, i32), (u64, i64)> = BTreeMap::new();
    let mut shown = 0;
    let mut last_intent: Option<v1::SubmitIntentRequest> = None;
    let mut records = 0;
    loop {
        let rec = match reader.next_record() {
            Ok(Some(r)) => r,
            Ok(None) => break,
            Err(e) => {
                println!("tape end: {e}");
                break;
            }
        };
        records += 1;
        match rec.source_id {
            2 | 6 => {
                if let Ok(r) = v1::SubmitIntentRequest::decode(rec.bytes.as_slice()) {
                    last_intent = Some(r);
                }
            }
            3 => {
                let v = v1::SubmitIntentResponse::decode(rec.bytes.as_slice()).unwrap();
                let agent = last_intent
                    .as_ref()
                    .map(|i| i.agent_id.clone())
                    .unwrap_or_default();
                *verdicts
                    .entry((agent.clone(), v.rejection_code, v.rejection_reason.clone()))
                    .or_default() += 1;
                if shown < 25 {
                    if let Some(i) = &last_intent {
                        println!(
                            "{:>6}  {:<14} type {} side {} px {} stop {} qty {} -> code {} {}",
                            records,
                            i.intent_id.chars().take(14).collect::<String>(),
                            i.intent_type,
                            i.side,
                            i.target_price,
                            i.stop_loss,
                            i.quantity,
                            v.rejection_code,
                            v.rejection_reason
                        );
                        shown += 1;
                    }
                }
            }
            4 => {
                if let Some(v1::exec_event::Event::Fill(f)) =
                    v1::ExecEvent::decode(rec.bytes.as_slice()).unwrap().event
                {
                    let e = fills.entry((f.strategy_id, f.side)).or_default();
                    e.0 += 1;
                    e.1 += f.qty;
                }
            }
            _ => {}
        }
    }
    println!("records {records}");
    println!("verdicts (agent, code, reason) -> count");
    for (k, v) in &verdicts {
        println!("  {k:?} {v}");
    }
    println!("fills (strategy, side) -> (count, qty)");
    for (k, v) in &fills {
        println!("  {k:?} {v:?}");
    }
}

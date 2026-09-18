//! Sends one intent to a running agenttrade and prints the verdict.
//! cargo run -p api --example submit -- <addr> <place|bad|flatten-all|state>

use api::v1;
use api::v1::agent_core_service_client::AgentCoreServiceClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let addr = args
        .next()
        .unwrap_or_else(|| "http://127.0.0.1:50051".into());
    let kind = args.next().unwrap_or_else(|| "state".into());
    let mut client = AgentCoreServiceClient::connect(addr).await?;
    let state = client
        .get_state(v1::GetStateRequest {
            venue: "kraken".into(),
            symbol: "BTC/USD".into(),
        })
        .await?
        .into_inner();
    println!(
        "seq {} bid {} ask {} equity {} strategies {}",
        state.sequence_id,
        state.bid,
        state.ask,
        state.available_equity,
        state.strategies.len()
    );
    for s in &state.strategies {
        println!(
            "  {} enabled={} net={} sent={} setups={} expired={}",
            s.id,
            s.enabled,
            s.net_qty,
            s.counters.as_ref().map_or(0, |c| c.orders_sent),
            s.counters.as_ref().map_or(0, |c| c.setups),
            s.counters.as_ref().map_or(0, |c| c.expired),
        );
    }
    let base = |id: &str, t: v1::IntentType| v1::SubmitIntentRequest {
        intent_id: id.into(),
        agent_id: "discretionary".into(),
        source_sequence_id: state.sequence_id,
        generated_time_ns: 0,
        intent_type: t as i32,
        venue: "kraken".into(),
        symbol: "BTC/USD".into(),
        ..Default::default()
    };
    let request = match kind.as_str() {
        "place" => v1::SubmitIntentRequest {
            side: v1::OrderSide::Buy as i32,
            time_in_force: v1::TimeInForce::Gtc as i32,
            target_price: state.ask,
            stop_loss: state.ask - 5_000,
            take_profit: state.ask + 5_000,
            quantity: 100_000,
            ..base("llm-place", v1::IntentType::Place)
        },
        "bad" => v1::SubmitIntentRequest {
            side: v1::OrderSide::Buy as i32,
            time_in_force: v1::TimeInForce::Gtc as i32,
            target_price: state.ask,
            stop_loss: 0,
            take_profit: state.ask + 5_000,
            quantity: 100_000,
            ..base("llm-bad", v1::IntentType::Place)
        },
        "flatten-all" => base("llm-flatten-all", v1::IntentType::FlattenAll),
        _ => return Ok(()),
    };
    let r = client.submit_intent(request).await?.into_inner();
    println!(
        "{} -> {:?} {:?} {} order {}",
        r.intent_id,
        v1::RiskDecision::try_from(r.decision).unwrap_or_default(),
        v1::RejectionCode::try_from(r.rejection_code).unwrap_or_default(),
        r.rejection_reason,
        r.executed_order_id
    );
    Ok(())
}

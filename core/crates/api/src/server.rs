//! tonic service over the core handle. Handlers never touch core state.

// tonic::Status is 176 bytes; returning it by value is the tonic convention.
#![allow(clippy::result_large_err)]

use std::pin::Pin;
use std::sync::mpsc::SyncSender;

use tokio::sync::{broadcast, oneshot, watch};
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::{Stream, StreamExt};
use tonic::{Request, Response, Status};
use types::Instrument;

use crate::convert;
use crate::core::{CoreInput, StateSnapshot};
use crate::v1;
use crate::v1::agent_core_service_server::{AgentCoreService, AgentCoreServiceServer};

pub struct Service {
    tx: SyncSender<CoreInput>,
    state: watch::Receiver<StateSnapshot>,
    events: broadcast::Sender<v1::MarketEvent>,
    instruments: Vec<Instrument>,
}

impl Service {
    pub fn new(
        tx: SyncSender<CoreInput>,
        state: watch::Receiver<StateSnapshot>,
        events: broadcast::Sender<v1::MarketEvent>,
        instruments: Vec<Instrument>,
    ) -> Self {
        Self {
            tx,
            state,
            events,
            instruments,
        }
    }

    pub fn into_server(self) -> AgentCoreServiceServer<Self> {
        AgentCoreServiceServer::new(self)
    }

    fn known(&self, venue: &str, symbol: &str) -> Result<(), Status> {
        if self
            .instruments
            .iter()
            .any(|i| i.venue == venue && i.symbol == symbol)
        {
            Ok(())
        } else {
            Err(Status::not_found(format!("{venue} {symbol}")))
        }
    }
}

pub fn now_ns() -> i64 {
    feed::now_ns()
}

pub fn state_response(s: &StateSnapshot, venue: &str, symbol: &str) -> v1::GetStateResponse {
    let features = s.features.as_ref().map(|f| v1::MarketFeatures {
        rsi: f.rsi.unwrap_or(0.0),
        ema_fast: f.ema_fast.unwrap_or(0.0),
        ema_slow: f.ema_slow.unwrap_or(0.0),
        order_flow_imbalance: f.order_flow_imbalance,
        bars: f.bars[..f.bar_count].iter().map(convert::bar).collect(),
        forming: Some(convert::bar(&f.forming)),
        bars_since_update: f.bars_since_update,
        volume_available: f.volume_available,
        book_stale: f.book_stale,
    });
    v1::GetStateResponse {
        sequence_id: s.sequence_id,
        timestamp_ns: s.timestamp_ns,
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        bid: s.bid.map_or(0, |l| l.price.0),
        ask: s.ask.map_or(0, |l| l.price.0),
        current_position: Some(convert::position(s.position, s.unrealized_pnl)),
        features,
        available_equity: s.available_equity,
    }
}

type EventStream = Pin<Box<dyn Stream<Item = Result<v1::MarketEvent, Status>> + Send>>;

#[tonic::async_trait]
impl AgentCoreService for Service {
    async fn get_state(
        &self,
        request: Request<v1::GetStateRequest>,
    ) -> Result<Response<v1::GetStateResponse>, Status> {
        let r = request.into_inner();
        self.known(&r.venue, &r.symbol)?;
        let s = self.state.borrow().clone();
        Ok(Response::new(state_response(&s, &r.venue, &r.symbol)))
    }

    async fn submit_intent(
        &self,
        request: Request<v1::SubmitIntentRequest>,
    ) -> Result<Response<v1::SubmitIntentResponse>, Status> {
        let request = request.into_inner();
        let (reply_tx, reply_rx) = oneshot::channel();
        let input = CoreInput::Intent {
            request,
            now_ns: now_ns(),
            reply: Some(reply_tx),
        };
        let tx = self.tx.clone();
        // Blocking send on purpose: a full channel blocks this task, never the runtime.
        let sent = tokio::task::spawn_blocking(move || tx.send(input).is_ok())
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        if !sent {
            return Err(Status::unavailable("core stopped"));
        }
        reply_rx
            .await
            .map(Response::new)
            .map_err(|_| Status::internal("core dropped the reply"))
    }

    type StreamEventsStream = EventStream;

    async fn stream_events(
        &self,
        request: Request<v1::StreamEventsRequest>,
    ) -> Result<Response<Self::StreamEventsStream>, Status> {
        let r = request.into_inner();
        self.known(&r.venue, &r.symbol)?;
        let rx = self.events.subscribe();
        let stream = tokio_stream::wrappers::BroadcastStream::new(rx).map(|item| match item {
            Ok(e) => Ok(e),
            Err(BroadcastStreamRecvError::Lagged(n)) => {
                Err(Status::data_loss(format!("subscriber lagged {n} events")))
            }
        });
        Ok(Response::new(Box::pin(stream)))
    }

    async fn list_instruments(
        &self,
        _: Request<v1::ListInstrumentsRequest>,
    ) -> Result<Response<v1::ListInstrumentsResponse>, Status> {
        Ok(Response::new(v1::ListInstrumentsResponse {
            instruments: self.instruments.iter().map(convert::instrument).collect(),
        }))
    }
}

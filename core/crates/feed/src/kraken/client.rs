//! Socket owner for the Kraken book channel. Reconnects with exponential
//! backoff, resubscribes on tracker demand, and watches for venue silence.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::time::{Instant, MissedTickBehavior};
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};
use types::{FeedEvent, Instrument, ResyncReason};

use super::tracker::{MsgKind, Tracker};
use super::wire;
use crate::{now_ns, FeedMsg};

#[derive(Debug, Clone)]
pub struct Config {
    pub url: String,
    pub instrument: Instrument,
    pub depth: u32,
    /// Resync if the book is silent this long while heartbeats still arrive.
    pub silence: Duration,
    /// Reconnect if nothing at all arrives for this long.
    pub dead: Duration,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
}

impl Config {
    pub fn kraken(instrument: Instrument, depth: u32) -> Self {
        Self {
            url: super::WS_URL.to_string(),
            instrument,
            depth,
            silence: Duration::from_secs(10),
            dead: Duration::from_secs(10),
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(30),
        }
    }
}

/// Runs until the receiver is dropped or the task is aborted.
pub async fn run(cfg: Config, tx: mpsc::Sender<FeedMsg>) {
    // tokio-tungstenite enables rustls without a crypto backend; pick ring once.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut tracker = Tracker::new(cfg.instrument.clone(), cfg.depth as usize);
    let mut backoff = cfg.backoff_min;
    loop {
        match session(&cfg, &mut tracker, &tx).await {
            Ok(()) => return,
            Err(e) => {
                warn!(error = %e, "kraken session ended");
                let recv_ns = now_ns();
                let handled = tracker.force_resync(ResyncReason::Disconnected);
                if send_all(&tx, recv_ns, handled.events).await.is_err() {
                    return;
                }
                if control(&tx, format!("disconnected {e}")).await.is_err() {
                    return;
                }
            }
        }
        info!(?backoff, "reconnecting");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(cfg.backoff_max);
    }
}

#[derive(Debug, thiserror::Error)]
enum SessionError {
    #[error("websocket: {0}")]
    Ws(#[from] Box<tokio_tungstenite::tungstenite::Error>),
    #[error("socket closed by peer")]
    Closed,
    #[error("no frames for {0:?}")]
    Dead(Duration),
}

fn ws_err(e: tokio_tungstenite::tungstenite::Error) -> SessionError {
    SessionError::Ws(Box::new(e))
}

/// One connection. Ok means the receiver went away; Err means reconnect.
async fn session(
    cfg: &Config,
    tracker: &mut Tracker,
    tx: &mpsc::Sender<FeedMsg>,
) -> Result<(), SessionError> {
    let (mut ws, _) = tokio_tungstenite::connect_async(&cfg.url)
        .await
        .map_err(ws_err)?;
    control(tx, "connected".into())
        .await
        .map_err(|_| SessionError::Closed)?;
    let symbol = &cfg.instrument.symbol;
    tracker.expect_snapshot();
    ws.send(Message::Text(wire::subscribe(symbol, cfg.depth)))
        .await
        .map_err(ws_err)?;

    let mut last_frame = Instant::now();
    let mut last_book = Instant::now();
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            frame = ws.next() => {
                let recv_ns = now_ns();
                let msg = frame.ok_or(SessionError::Closed)?.map_err(ws_err)?;
                last_frame = Instant::now();
                let bytes = match msg {
                    Message::Text(t) => t.into_bytes(),
                    Message::Binary(b) => b,
                    Message::Ping(p) => { ws.send(Message::Pong(p)).await.map_err(ws_err)?; continue; }
                    Message::Pong(_) | Message::Frame(_) => continue,
                    Message::Close(_) => return Err(SessionError::Closed),
                };
                if tx.send(FeedMsg::Raw { recv_ns, bytes: bytes.clone() }).await.is_err() {
                    return Ok(());
                }
                let handled = tracker.on_frame(&bytes);
                if matches!(handled.kind, Some(MsgKind::Snapshot | MsgKind::Update)) {
                    last_book = Instant::now();
                }
                if handled.kind == Some(MsgKind::Error) {
                    warn!(frame = %String::from_utf8_lossy(&bytes), "venue error frame");
                }
                let resubscribe = handled.resubscribe;
                if send_all(tx, recv_ns, handled.events).await.is_err() {
                    return Ok(());
                }
                if resubscribe {
                    resubscribe_book(&mut ws, cfg, tracker, tx).await?;
                    last_book = Instant::now();
                }
            }
            _ = ticker.tick() => {
                let now = Instant::now();
                if now - last_frame > cfg.dead {
                    return Err(SessionError::Dead(cfg.dead));
                }
                if tracker.has_book() && now - last_book > cfg.silence {
                    let recv_ns = now_ns();
                    let handled = tracker.force_resync(ResyncReason::Silent);
                    if send_all(tx, recv_ns, handled.events).await.is_err() {
                        return Ok(());
                    }
                    resubscribe_book(&mut ws, cfg, tracker, tx).await?;
                    last_book = now;
                }
            }
        }
    }
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn resubscribe_book(
    ws: &mut Ws,
    cfg: &Config,
    tracker: &mut Tracker,
    tx: &mpsc::Sender<FeedMsg>,
) -> Result<(), SessionError> {
    let symbol = &cfg.instrument.symbol;
    let _ = control(tx, "resubscribe".into()).await;
    ws.send(Message::Text(wire::unsubscribe(symbol, cfg.depth)))
        .await
        .map_err(ws_err)?;
    ws.send(Message::Text(wire::subscribe(symbol, cfg.depth)))
        .await
        .map_err(ws_err)?;
    tracker.expect_snapshot();
    Ok(())
}

async fn send_all(
    tx: &mpsc::Sender<FeedMsg>,
    recv_ns: i64,
    events: Vec<FeedEvent>,
) -> Result<(), ()> {
    for event in events {
        tx.send(FeedMsg::Event { recv_ns, event })
            .await
            .map_err(|_| ())?;
    }
    Ok(())
}

async fn control(tx: &mpsc::Sender<FeedMsg>, text: String) -> Result<(), ()> {
    tx.send(FeedMsg::Control {
        recv_ns: now_ns(),
        text,
    })
    .await
    .map_err(|_| ())
}

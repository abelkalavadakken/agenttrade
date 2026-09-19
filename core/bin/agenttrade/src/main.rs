//! The entrypoint. Feed client, core thread, 1 s ticks and the gRPC server in
//! one process, writing a tape with hashes. See docs/api.md section 1.
//!
//! `--tape <file>` replaces the venue with a recorded tape read at its
//! recorded pace: live mode in docs/api.md section 4. The core, the gRPC
//! server and the wakes are the same; only the frames come from disk.
//! Ticks come from the tape too, so the core clock is tape time.

use std::fs::File;
use std::path::PathBuf;
use std::time::Duration;

use api::v1;
use api::{spawn_core, Clock, CoreConfig, CoreInput, Service};
use clap::Parser;
use feed::kraken;
use tape::{Mode, Reader};
use tokio::sync::mpsc;
use tracing::{error, info};
use types::instruments;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value = "kraken")]
    venue: String,
    #[arg(long, default_value = "BTC/USD")]
    symbol: String,
    /// Run this long, then stop. Omit to run until Ctrl-C.
    #[arg(long)]
    seconds: Option<u64>,
    #[arg(long, default_value = "tapes")]
    out: PathBuf,
    #[arg(long, default_value = "127.0.0.1:50051")]
    listen: String,
    /// Enable a strategy at 1x, e.g. --enable mr_ofi. Repeatable.
    #[arg(long = "enable")]
    enable: Vec<String>,
    /// Tune a param at start, e.g. --tune mr_ofi.stop_ticks=100. Repeatable.
    #[arg(long = "tune")]
    tune: Vec<String>,
    /// Wake::Timer interval in seconds.
    #[arg(long, default_value_t = 60)]
    timer_secs: u64,
    #[arg(long, default_value_t = 10)]
    depth: u32,
    /// Replay this tape at recorded pace instead of connecting to the venue.
    #[arg(long)]
    tape: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    let instrument = instruments::find(&args.venue, &args.symbol)
        .ok_or_else(|| format!("unknown instrument {} {}", args.venue, args.symbol))?;

    std::fs::create_dir_all(&args.out)?;
    let started = feed::now_ns();
    let path = args.out.join(format!(
        "{}_{}_{}.tape",
        args.venue,
        args.symbol.replace('/', "-"),
        started / 1_000_000_000
    ));
    let mut cfg = CoreConfig::paper_defaults(instrument.clone());
    cfg.depth = args.depth;
    cfg.timer_interval_ns = args.timer_secs as i64 * 1_000_000_000;
    let handle = spawn_core(cfg, File::create(&path)?)?;
    info!(path = %path.display(), listen = %args.listen, "agenttrade up");

    let clock = if args.tape.is_some() {
        Clock::Core
    } else {
        Clock::Wall
    };
    let service = Service::new(
        handle.sender(),
        handle.state.clone(),
        handle.events.clone(),
        instruments::kraken(),
    )
    .with_clock(clock);
    let addr = args.listen.parse()?;
    let server = tokio::spawn(async move {
        if let Err(e) = tonic::transport::Server::builder()
            .add_service(service.into_server())
            .serve(addr)
            .await
        {
            error!(error = %e, "grpc server stopped");
        }
    });

    let (feed_tx, mut feed_rx) = mpsc::channel(4_096);
    let client = match &args.tape {
        Some(tape) => {
            let reader = Reader::new(File::open(tape)?, Mode::Paced)?;
            info!(tape = %tape.display(), "replaying at recorded pace");
            tokio::task::spawn_blocking(move || pace_tape(reader, feed_tx))
        }
        None => tokio::spawn(kraken::run(
            kraken::Config::kraken(instrument, args.depth),
            feed_tx,
        )),
    };
    let tick_tx = handle.sender();
    let from_tape = args.tape.is_some();
    let ticker = tokio::spawn(async move {
        // A tape carries its own ticks; wall-clock ticks would run the core
        // clock forward past the tape.
        if from_tape {
            return;
        }
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let tx = tick_tx.clone();
            let ok = tokio::task::spawn_blocking(move || {
                tx.send(CoreInput::Tick(feed::now_ns())).is_ok()
            })
            .await
            .unwrap_or(false);
            if !ok {
                break;
            }
        }
    });

    let deadline = async {
        match args.seconds {
            Some(s) => tokio::time::sleep(Duration::from_secs(s)).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(deadline);
    let core_tx = handle.sender();
    // Startup allocations go through the gate, which needs a connected venue
    // and a verified book, so they wait for the first snapshot.
    let mut startup = Some(tokio::spawn(wait_then_apply(
        handle.state.clone(),
        handle.sender(),
        args.enable.clone(),
        args.tune.clone(),
        args.venue.clone(),
        args.symbol.clone(),
        clock,
    )));
    loop {
        if let Some(task) = startup.as_mut() {
            if task.is_finished() {
                if let Err(e) = task.await? {
                    error!(error = %e, "startup intent rejected; stopping");
                    break;
                }
                startup = None;
            }
        }
        tokio::select! {
            _ = &mut deadline => break,
            _ = tokio::signal::ctrl_c() => { info!("interrupted"); break; }
            msg = feed_rx.recv() => {
                let Some(msg) = msg else { break };
                let tx = core_tx.clone();
                // Blocking send on purpose: a full channel blocks this task, never the runtime.
                let ok = tokio::task::spawn_blocking(move || tx.send(CoreInput::Feed(msg)).is_ok()).await?;
                if !ok { break; }
            }
        }
    }
    client.abort();
    ticker.abort();
    server.abort();
    drop(core_tx);
    drop(feed_rx);
    let channel_full = handle.channel_full.clone();
    let stats = handle.shutdown();
    let size = std::fs::metadata(&path)?.len();
    println!("tape              {}", path.display());
    println!("size bytes        {size}");
    println!("tape records      {}", stats.tape_records);
    println!("sequence id       {}", stats.sequence_id);
    println!("core events       {}", stats.events);
    println!("updates           {}", stats.tracker.updates);
    println!("trades            {}", stats.tracker.trades);
    println!("checksum failures {}", stats.tracker.checksum_failures);
    println!(
        "channel full      {}",
        channel_full.load(std::sync::atomic::Ordering::Relaxed)
    );
    println!("final hash        {}", stats.hash_hex);
    Ok(())
}

async fn wait_then_apply(
    mut state: tokio::sync::watch::Receiver<api::StateSnapshot>,
    tx: std::sync::mpsc::SyncSender<CoreInput>,
    enable: Vec<String>,
    tune: Vec<String>,
    venue: String,
    symbol: String,
    clock: Clock,
) -> Result<(), String> {
    let ready = async {
        loop {
            {
                let s = state.borrow();
                if s.venue_connected && !s.book_stale {
                    break;
                }
            }
            if state.changed().await.is_err() {
                return Err("core stopped".to_string());
            }
        }
        Ok(())
    };
    tokio::time::timeout(Duration::from_secs(60), ready)
        .await
        .map_err(|_| "no verified book within 60 s".to_string())??;
    let now_ns = match clock {
        Clock::Wall => feed::now_ns(),
        Clock::Core => state.borrow().timestamp_ns,
    };
    apply_startup_intents(&tx, now_ns, &enable, &tune, &venue, &symbol).await
}

/// Feed a tape into the core the way the venue would: raw frames and control
/// records, in order, at recorded gaps. Recorded intents, verdicts, exec
/// events, hashes, strategy records and wakes are skipped; the core
/// regenerates them from the frames, and live agents supply the intents.
fn pace_tape(mut reader: Reader<File>, tx: mpsc::Sender<feed::FeedMsg>) {
    let raw = reader
        .sources()
        .iter()
        .position(|s| s == "kraken.ws")
        .map(|i| i as u16);
    let ctl = reader
        .sources()
        .iter()
        .position(|s| s == "record.ctl")
        .map(|i| i as u16);
    loop {
        let rec = match reader.next_record() {
            Ok(Some(rec)) => rec,
            Ok(None) => {
                info!("tape exhausted");
                break;
            }
            Err(e) => {
                error!(error = %e, "tape read stopped");
                break;
            }
        };
        let msg = if Some(rec.source_id) == raw {
            feed::FeedMsg::Raw {
                recv_ns: rec.recv_ns,
                bytes: rec.bytes,
            }
        } else if Some(rec.source_id) == ctl {
            match String::from_utf8(rec.bytes) {
                Ok(text) => feed::FeedMsg::Control {
                    recv_ns: rec.recv_ns,
                    text,
                },
                Err(_) => continue,
            }
        } else {
            continue;
        };
        if tx.blocking_send(msg).is_err() {
            break; // the core stopped first: deadline or Ctrl-C
        }
    }
}

/// Startup allocations and tunes go through the gate like any intent, so
/// the tape shows them and replay reproduces them.
async fn apply_startup_intents(
    tx: &std::sync::mpsc::SyncSender<CoreInput>,
    now_ns: i64,
    enable: &[String],
    tune: &[String],
    venue: &str,
    symbol: &str,
) -> Result<(), String> {
    let base = |intent_id: String, intent_type: v1::IntentType| v1::SubmitIntentRequest {
        intent_id,
        agent_id: "operator".into(),
        intent_type: intent_type as i32,
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        ..Default::default()
    };
    let mut requests = Vec::new();
    for t in tune {
        let (target, value) = t
            .split_once('=')
            .ok_or("--tune wants strategy.param=value")?;
        let (strategy_id, param) = target
            .split_once('.')
            .ok_or("--tune wants strategy.param=value")?;
        let mut r = base(format!("startup-tune-{target}"), v1::IntentType::Tune);
        r.tune = Some(v1::Tune {
            strategy_id: strategy_id.into(),
            param: param.into(),
            value: value.parse().map_err(|_| format!("bad value in {t}"))?,
        });
        requests.push(r);
    }
    for id in enable {
        let mut r = base(format!("startup-enable-{id}"), v1::IntentType::Allocate);
        r.allocation = Some(v1::Allocation {
            strategy_id: id.clone(),
            enabled: true,
            size_multiplier_bps: 10_000,
            on_disable: v1::OnDisable::Flatten as i32,
        });
        requests.push(r);
    }
    for r in requests {
        let id = r.intent_id.clone();
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let input = CoreInput::Intent {
            request: Box::new(r),
            now_ns,
            reply: Some(reply_tx),
        };
        let tx = tx.clone();
        tokio::task::spawn_blocking(move || tx.send(input))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|_| "core stopped".to_string())?;
        let resp = reply_rx.await.map_err(|_| "no reply".to_string())?;
        if resp.decision != v1::RiskDecision::Approved as i32 {
            return Err(format!(
                "{id}: {} ({})",
                resp.rejection_reason, resp.rejection_code
            ));
        }
        info!(intent = %id, "startup intent approved");
    }
    Ok(())
}

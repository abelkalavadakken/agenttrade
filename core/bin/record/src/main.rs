//! Connect to a venue, record every raw frame for N seconds, exit.

use std::fs::File;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use feed::{kraken, FeedMsg};
use tokio::sync::mpsc;
use tracing::info;
use types::{FeedEvent, ResyncReason};

/// Source ids in the tape header. Order is the id.
const SOURCES: [&str; 2] = ["kraken.ws", "record.ctl"];
const SRC_WS: u16 = 0;
const SRC_CTL: u16 = 1;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long)]
    venue: String,
    #[arg(long)]
    symbol: String,
    #[arg(long)]
    seconds: u64,
    /// Output directory. File name is <venue>_<symbol>_<unix seconds>.tape.
    #[arg(long, default_value = "tapes")]
    out: PathBuf,
    #[arg(long, default_value_t = 10)]
    depth: u32,
}

#[derive(Default)]
struct Counts {
    raw: u64,
    raw_bytes: u64,
    snapshots: u64,
    deltas: u64,
    trades: u64,
    resync_checksum: u64,
    resync_unsolicited: u64,
    resync_silent: u64,
    resync_disconnect: u64,
    control: u64,
}

impl Counts {
    fn count(&mut self, msg: &FeedMsg) {
        match msg {
            FeedMsg::Raw { bytes, .. } => {
                self.raw += 1;
                self.raw_bytes += bytes.len() as u64;
            }
            FeedMsg::Control { .. } => self.control += 1,
            FeedMsg::Event { event, .. } => match event {
                FeedEvent::Book(types::BookEvent::Snapshot { .. }) => self.snapshots += 1,
                FeedEvent::Book(types::BookEvent::Delta { .. }) => self.deltas += 1,
                FeedEvent::Trade(_) => self.trades += 1,
                FeedEvent::Resync(ResyncReason::ChecksumMismatch | ResyncReason::Crossed) => {
                    self.resync_checksum += 1
                }
                FeedEvent::Resync(ResyncReason::UnsolicitedSnapshot) => {
                    self.resync_unsolicited += 1
                }
                FeedEvent::Resync(ResyncReason::Silent) => self.resync_silent += 1,
                FeedEvent::Resync(ResyncReason::Disconnected) => self.resync_disconnect += 1,
            },
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    if args.venue != "kraken" {
        return Err(format!("unsupported venue {}", args.venue).into());
    }
    let instrument = types::instruments::find(&args.venue, &args.symbol)
        .ok_or_else(|| format!("unknown instrument {} {}", args.venue, args.symbol))?;

    std::fs::create_dir_all(&args.out)?;
    let started = feed::now_ns();
    let name = format!(
        "{}_{}_{}.tape",
        args.venue,
        args.symbol.replace('/', "-"),
        started / 1_000_000_000
    );
    let path = args.out.join(name);
    let mut writer = tape::Writer::new(File::create(&path)?, &SOURCES)?;
    info!(path = %path.display(), seconds = args.seconds, "recording");

    let (tx, mut rx) = mpsc::channel::<FeedMsg>(4096);
    let cfg = kraken::Config::kraken(instrument, args.depth);
    let mut client = tokio::spawn(kraken::run(cfg, tx));
    let deadline = tokio::time::sleep(Duration::from_secs(args.seconds));
    tokio::pin!(deadline);

    let mut counts = Counts::default();
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            _ = tokio::signal::ctrl_c() => { info!("interrupted"); break; }
            res = &mut client => {
                writer.flush()?;
                return Err(format!("feed client stopped early: {res:?}").into());
            }
            msg = rx.recv() => {
                let Some(msg) = msg else { break };
                counts.count(&msg);
                match &msg {
                    FeedMsg::Raw { recv_ns, bytes } => writer.append(*recv_ns, SRC_WS, bytes)?,
                    FeedMsg::Control { recv_ns, text } => writer.append(*recv_ns, SRC_CTL, text.as_bytes())?,
                    FeedMsg::Event { .. } => {}
                }
            }
        }
    }
    client.abort();
    writer.flush()?;
    let size = std::fs::metadata(&path)?.len();

    println!("tape            {}", path.display());
    println!("size bytes      {size}");
    println!("records         {}", writer.records());
    println!(
        "raw frames      {}  ({} bytes)",
        counts.raw, counts.raw_bytes
    );
    println!("control         {}", counts.control);
    println!("snapshots       {}", counts.snapshots);
    println!("deltas          {}", counts.deltas);
    println!("trades          {}", counts.trades);
    println!("resync checksum {}", counts.resync_checksum);
    println!("resync unsolicited {}", counts.resync_unsolicited);
    println!("resync silent   {}", counts.resync_silent);
    println!("resync disconn  {}", counts.resync_disconnect);
    Ok(())
}

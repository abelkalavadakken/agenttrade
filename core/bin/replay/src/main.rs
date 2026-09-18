//! Read a tape, run the venue tracker over it, print what happened.
//!
//! `recorded` mode reads as fast as possible. `live` mode paces records at
//! their recorded gaps so downstream consumers see original timing.

use std::fs::File;
use std::path::PathBuf;
use std::time::Instant;

use api::{Core, CoreConfig, CoreInput, SOURCES};
use book::{ApplyError, Book};
use feed::FeedMsg;
use prost::Message;

use clap::{Parser, ValueEnum};
use feed::kraken::Tracker;
use tape::{Mode, Reader};
use types::{instruments, FeedEvent, ResyncReason};

#[derive(Parser, Debug)]
struct Args {
    #[arg(long)]
    tape: PathBuf,
    #[arg(long, value_enum, default_value_t = ReplayMode::Recorded)]
    mode: ReplayMode,
    #[arg(long, default_value = "kraken")]
    venue: String,
    #[arg(long, default_value = "BTC/USD")]
    symbol: String,
    #[arg(long, default_value_t = 10)]
    depth: u32,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ReplayMode {
    Recorded,
    Live,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let instrument = instruments::find(&args.venue, &args.symbol)
        .ok_or_else(|| format!("unknown instrument {} {}", args.venue, args.symbol))?;
    let mode = match args.mode {
        ReplayMode::Recorded => Mode::Fast,
        ReplayMode::Live => Mode::Paced,
    };
    let mut reader = Reader::new(File::open(&args.tape)?, mode)?;
    let ws_id = reader
        .sources()
        .iter()
        .position(|s| s == "kraken.ws")
        .ok_or("tape has no kraken.ws source")? as u16;
    let mut tracker = Tracker::new(instrument, args.depth as usize);
    let mut book = Book::new(args.depth as usize);
    let mut applied = 0u64;
    let mut crossed = 0u64;
    let mut book_mismatches = 0u64;
    let mut latencies: Vec<u64> = Vec::new();

    let mut records = 0u64;
    let mut bytes = 0u64;
    let mut first_ns = None;
    let mut last_ns = 0i64;
    let mut resyncs = [0u64; 4];
    let mut controls = Vec::new();
    let mut tape_error = None;

    loop {
        let rec = match reader.next_record() {
            Ok(Some(r)) => r,
            Ok(None) => break,
            Err(e) => {
                tape_error = Some(e);
                break;
            }
        };
        records += 1;
        bytes += rec.bytes.len() as u64;
        first_ns.get_or_insert(rec.recv_ns);
        last_ns = rec.recv_ns;
        if rec.source_id != ws_id {
            let text = String::from_utf8_lossy(&rec.bytes).into_owned();
            if text == "connected" {
                // The recorder subscribed on connect; the next snapshot is expected.
                tracker.expect_snapshot();
            }
            controls.push(text);
            continue;
        }
        let handled = tracker.on_frame(&rec.bytes);
        for e in handled.events {
            match e {
                FeedEvent::Resync(r) => {
                    resyncs[resync_index(r)] += 1;
                    book.mark_stale();
                }
                FeedEvent::Book(ev) => {
                    let t0 = Instant::now();
                    let res = book.apply(&ev);
                    latencies.push(t0.elapsed().as_nanos() as u64);
                    match res {
                        Ok(()) => applied += 1,
                        Err(ApplyError::Crossed) => crossed += 1,
                        Err(ApplyError::Checksum { .. }) => book_mismatches += 1,
                        Err(ApplyError::NoSnapshot) => {}
                    }
                }
                FeedEvent::Trade(_) => {}
            }
        }
        if handled.resubscribe {
            // The recorder resubscribed at this point; the next snapshot is expected.
            tracker.expect_snapshot();
        }
    }

    let stats = tracker.stats();
    let duration_ns = first_ns.map_or(0, |f| last_ns - f);
    println!("tape              {}", args.tape.display());
    println!("records           {records}");
    println!("payload bytes     {bytes}");
    println!("duration          {:.3} s", duration_ns as f64 / 1e9);
    println!("snapshots         {}", stats.snapshots);
    println!("updates           {}", stats.updates);
    println!("trades            {}", stats.trades);
    println!("heartbeats        {}", stats.heartbeats);
    println!("unparsed          {}", stats.unparsed);
    println!("unsolicited snaps {}", stats.unsolicited_snapshots);
    println!("tracker crossed   {}", stats.crossed);
    println!("checksum failures {}", stats.checksum_failures);
    println!(
        "resync silent     {}",
        resyncs[resync_index(ResyncReason::Silent)]
    );
    latencies.sort_unstable();
    let pct = |p: f64| -> u64 {
        if latencies.is_empty() {
            return 0;
        }
        let i = ((latencies.len() as f64 - 1.0) * p).round() as usize;
        latencies[i]
    };
    println!("book applied      {applied}");
    println!("book crossed      {crossed}");
    println!("book mismatches   {book_mismatches}");
    println!("apply p50 ns      {}", pct(0.50));
    println!("apply p99 ns      {}", pct(0.99));
    println!(
        "apply max ns      {}",
        latencies.last().copied().unwrap_or(0)
    );
    println!("control records   {}", controls.len());
    for c in controls.iter().filter(|c| c.as_str() != "connected") {
        println!("  {c}");
    }
    if let Some(e) = tape_error {
        println!("tape error        {e}");
        std::process::exit(2);
    }
    if matches!(args.mode, ReplayMode::Recorded) {
        let v = verify_hashes(&args)?;
        println!("core events       {}", v.events);
        println!("intents replayed  {}", v.intents);
        println!("hashes verified   {}", v.verified);
        println!("hash mismatches   {}", v.mismatches);
        println!("final hash        {}", v.final_hash);
        if v.mismatches > 0 {
            std::process::exit(2);
        }
    }
    Ok(())
}

struct Verified {
    events: u64,
    intents: u64,
    verified: u64,
    mismatches: u64,
    final_hash: String,
}

/// Recorded mode: drive the same core the recorder ran, re-inject recorded
/// intents at their tape positions, compare every StateHash record.
fn verify_hashes(args: &Args) -> Result<Verified, Box<dyn std::error::Error>> {
    let instrument = instruments::find(&args.venue, &args.symbol).ok_or("unknown instrument")?;
    let mut cfg = CoreConfig::paper_defaults(instrument);
    cfg.depth = args.depth;
    let (mut core, _state, _events) = Core::new(cfg, std::io::sink(), Default::default())?;
    let mut reader = Reader::new(File::open(&args.tape)?, Mode::Fast)?;
    let id = |name: &str| {
        reader
            .sources()
            .iter()
            .position(|s| s == name)
            .map(|i| i as u16)
    };
    let (ws, ctl, intent, hash) = (
        id(SOURCES[0]),
        id(SOURCES[1]),
        id(SOURCES[2]),
        id(SOURCES[5]),
    );
    let mut v = Verified {
        events: 0,
        intents: 0,
        verified: 0,
        mismatches: 0,
        final_hash: String::new(),
    };
    let mut next_tick = None;
    while let Some(rec) = reader.next_record()? {
        let boundary = rec.recv_ns.div_euclid(1_000_000_000) * 1_000_000_000;
        let tick = *next_tick.get_or_insert(boundary);
        if boundary > tick {
            for t in (tick..boundary).step_by(1_000_000_000) {
                core.handle(CoreInput::Tick(t + 1_000_000_000))?;
            }
            next_tick = Some(boundary);
        }
        let src = Some(rec.source_id);
        if src == ws {
            core.handle(CoreInput::Feed(FeedMsg::Raw {
                recv_ns: rec.recv_ns,
                bytes: rec.bytes,
            }))?;
        } else if src == ctl {
            core.handle(CoreInput::Feed(FeedMsg::Control {
                recv_ns: rec.recv_ns,
                text: String::from_utf8_lossy(&rec.bytes).into_owned(),
            }))?;
        } else if src == intent {
            let request = api::v1::SubmitIntentRequest::decode(rec.bytes.as_slice())?;
            v.intents += 1;
            core.handle(CoreInput::Intent {
                request,
                now_ns: rec.recv_ns,
                reply: None,
            })?;
        } else if src == hash {
            let recorded = api::v1::StateHash::decode(rec.bytes.as_slice())?;
            if recorded.hash.as_slice() == core.hasher().current() {
                v.verified += 1;
            } else {
                v.mismatches += 1;
                if v.mismatches <= 5 {
                    println!(
                        "hash mismatch     seq {} event {} (recorded seq {} event {})",
                        core.sequence_id(),
                        core.hasher().events(),
                        recorded.sequence_id,
                        recorded.event_count
                    );
                }
            }
        }
    }
    v.events = core.hasher().events();
    v.final_hash = core.hasher().hex();
    Ok(v)
}

fn resync_index(r: ResyncReason) -> usize {
    match r {
        ResyncReason::ChecksumMismatch | ResyncReason::Crossed => 0,
        ResyncReason::UnsolicitedSnapshot => 1,
        ResyncReason::Silent => 2,
        ResyncReason::Disconnected => 3,
    }
}

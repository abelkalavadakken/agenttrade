//! Read a tape, run the venue tracker over it, print what happened.
//!
//! `recorded` mode reads as fast as possible. `live` mode paces records at
//! their recorded gaps so downstream consumers see original timing.

use std::fs::File;
use std::path::PathBuf;

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
            if let FeedEvent::Resync(r) = e {
                resyncs[resync_index(r)] += 1;
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
    println!("heartbeats        {}", stats.heartbeats);
    println!("unparsed          {}", stats.unparsed);
    println!("gap count         {}", stats.gaps);
    println!("checksum failures {}", stats.checksum_failures);
    println!(
        "resync silent     {}",
        resyncs[resync_index(ResyncReason::Silent)]
    );
    println!("control records   {}", controls.len());
    for c in controls.iter().filter(|c| c.as_str() != "connected") {
        println!("  {c}");
    }
    if let Some(e) = tape_error {
        println!("tape error        {e}");
        std::process::exit(2);
    }
    Ok(())
}

fn resync_index(r: ResyncReason) -> usize {
    match r {
        ResyncReason::ChecksumMismatch => 0,
        ResyncReason::UnsolicitedSnapshot => 1,
        ResyncReason::Silent => 2,
        ResyncReason::Disconnected => 3,
    }
}

//! Copies the first N seconds of a tape into a new tape with the same header.
//! cargo run -p api --example tape_cut -- <in> <out> <seconds>

use std::fs::File;

use tape::{Mode, Reader, Writer};

fn main() {
    let mut args = std::env::args().skip(1);
    let (input, output, seconds) = (
        args.next().expect("in"),
        args.next().expect("out"),
        args.next()
            .expect("seconds")
            .parse::<i64>()
            .expect("seconds"),
    );
    let mut reader = Reader::new(File::open(&input).unwrap(), Mode::Fast).unwrap();
    let sources: Vec<String> = reader.sources().to_vec();
    let names: Vec<&str> = sources.iter().map(String::as_str).collect();
    let mut writer = Writer::new(File::create(&output).unwrap(), &names).unwrap();
    let mut first = None;
    let mut kept = 0u64;
    while let Some(rec) = reader.next_record().unwrap() {
        let t0 = *first.get_or_insert(rec.recv_ns);
        if rec.recv_ns - t0 > seconds * 1_000_000_000 {
            break;
        }
        writer
            .append(rec.recv_ns, rec.source_id, &rec.bytes)
            .unwrap();
        kept += 1;
    }
    writer.flush().unwrap();
    println!("kept {kept} records, {seconds} s, into {output}");
}

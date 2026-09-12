use std::io::Cursor;
use std::time::Instant;

use tape::{Error, Mode, Reader, Record, Writer};

fn write(records: &[Record]) -> Vec<u8> {
    let mut w = Writer::new(Vec::new(), &["kraken.ws", "record.ctl"]).unwrap();
    for r in records {
        w.append(r.recv_ns, r.source_id, &r.bytes).unwrap();
    }
    assert_eq!(w.records(), records.len() as u64);
    w.into_inner().unwrap()
}

fn sample() -> Vec<Record> {
    vec![
        Record {
            recv_ns: 1_000,
            source_id: 0,
            bytes: b"{\"channel\":\"heartbeat\"}".to_vec(),
        },
        Record {
            recv_ns: 2_000,
            source_id: 1,
            bytes: b"connected".to_vec(),
        },
        Record {
            recv_ns: 3_000,
            source_id: 0,
            bytes: vec![],
        },
        Record {
            recv_ns: -5,
            source_id: 0,
            bytes: vec![0xff; 70_000],
        },
    ]
}

#[test]
fn round_trip() {
    let bytes = write(&sample());
    let mut r = Reader::new(Cursor::new(&bytes), Mode::Fast).unwrap();
    assert_eq!(
        r.sources(),
        &["kraken.ws".to_string(), "record.ctl".to_string()]
    );
    assert_eq!(r.source_name(1), Some("record.ctl"));
    let got: Vec<Record> = r.by_ref().map(Result::unwrap).collect();
    assert_eq!(got, sample());
}

#[test]
fn empty_tape() {
    let bytes = write(&[]);
    let mut r = Reader::new(Cursor::new(&bytes), Mode::Fast).unwrap();
    assert!(r.next().is_none());
}

#[test]
fn crc_mismatch_is_reported_with_index() {
    let mut bytes = write(&sample());
    let last = bytes.len() - 100;
    bytes[last] ^= 0x01;
    let r = Reader::new(Cursor::new(&bytes), Mode::Fast).unwrap();
    let results: Vec<_> = r.collect();
    assert_eq!(results.len(), 4);
    assert!(results[..3].iter().all(Result::is_ok));
    assert!(matches!(results[3], Err(Error::Crc { index: 3 })));
}

#[test]
fn truncated_tail_is_reported() {
    let mut bytes = write(&sample());
    bytes.truncate(bytes.len() - 10);
    let r = Reader::new(Cursor::new(&bytes), Mode::Fast).unwrap();
    let results: Vec<_> = r.collect();
    assert_eq!(results.len(), 4);
    assert!(matches!(
        results[3],
        Err(Error::TruncatedTail { index: 3, .. })
    ));
}

#[test]
fn rejects_wrong_magic_and_version() {
    assert!(matches!(
        Reader::new(Cursor::new(b"NOPE\x01\x00\x00\x00"), Mode::Fast),
        Err(Error::BadMagic)
    ));
    assert!(matches!(
        Reader::new(Cursor::new(b"ATTP\x09\x00\x00\x00"), Mode::Fast),
        Err(Error::Version(9))
    ));
}

#[test]
fn unknown_source_rejected_on_write() {
    let mut w = Writer::new(Vec::new(), &["only"]).unwrap();
    assert!(matches!(w.append(0, 1, b"x"), Err(Error::UnknownSource(1))));
}

#[test]
fn paced_mode_waits_for_recorded_gaps() {
    let records = vec![
        Record {
            recv_ns: 0,
            source_id: 0,
            bytes: vec![],
        },
        Record {
            recv_ns: 60_000_000,
            source_id: 0,
            bytes: vec![],
        },
    ];
    let bytes = write(&records);
    let start = Instant::now();
    let r = Reader::new(Cursor::new(&bytes), Mode::Paced).unwrap();
    assert_eq!(r.count(), 2);
    assert!(start.elapsed().as_millis() >= 60);
}

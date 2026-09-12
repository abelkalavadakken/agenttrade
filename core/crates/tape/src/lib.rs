//! Append-only binary log of raw frames.
//!
//! File layout, all integers little-endian:
//!
//! ```text
//! header:  magic "ATTP" | u16 version | u16 source_count | (u16 len, utf8)*
//! frame:   u32 payload_len | payload | u32 crc32(payload)
//! payload: i64 recv_ns | u16 source_id | bytes
//! ```
//!
//! A frame cut short by a crash is reported as `TruncatedTail`, not silently dropped.

mod reader;
mod writer;

pub use reader::{Mode, Reader};
pub use writer::Writer;

pub const MAGIC: &[u8; 4] = b"ATTP";
pub const VERSION: u16 = 1;
const PAYLOAD_HEADER: usize = 8 + 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub recv_ns: i64,
    pub source_id: u16,
    pub bytes: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("bad magic")]
    BadMagic,
    #[error("unsupported tape version {0}")]
    Version(u16),
    #[error("source table is not utf8")]
    SourceName,
    #[error("frame {index}: crc mismatch")]
    Crc { index: u64 },
    #[error("frame {index}: truncated tail, {have} of {want} bytes")]
    TruncatedTail {
        index: u64,
        have: usize,
        want: usize,
    },
    #[error("frame {index}: payload shorter than header")]
    ShortPayload { index: u64 },
    #[error("unknown source id {0}")]
    UnknownSource(u16),
}

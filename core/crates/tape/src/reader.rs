use std::io::{BufReader, ErrorKind, Read};
use std::time::{Duration, Instant};

use crate::{Error, Record, MAGIC, PAYLOAD_HEADER, VERSION};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Yield records as fast as the disk allows.
    Fast,
    /// Sleep so records come out with their original inter-arrival gaps.
    Paced,
}

pub struct Reader<R: Read> {
    input: BufReader<R>,
    sources: Vec<String>,
    mode: Mode,
    index: u64,
    first: Option<(Instant, i64)>,
}

impl<R: Read> Reader<R> {
    pub fn new(inner: R, mode: Mode) -> Result<Self, Error> {
        let mut input = BufReader::new(inner);
        let mut magic = [0u8; 4];
        input.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(Error::BadMagic);
        }
        let version = read_u16(&mut input)?;
        if version != VERSION {
            return Err(Error::Version(version));
        }
        let count = read_u16(&mut input)?;
        let mut sources = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let len = read_u16(&mut input)? as usize;
            let mut name = vec![0u8; len];
            input.read_exact(&mut name)?;
            sources.push(String::from_utf8(name).map_err(|_| Error::SourceName)?);
        }
        Ok(Self {
            input,
            sources,
            mode,
            index: 0,
            first: None,
        })
    }

    pub fn sources(&self) -> &[String] {
        &self.sources
    }

    pub fn source_name(&self, id: u16) -> Option<&str> {
        self.sources.get(id as usize).map(String::as_str)
    }

    /// Next record, `None` at a clean end of file.
    pub fn next_record(&mut self) -> Result<Option<Record>, Error> {
        let index = self.index;
        let mut len_buf = [0u8; 4];
        match self.input.read_exact(&mut len_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        let len = u32::from_le_bytes(len_buf) as usize;
        if len < PAYLOAD_HEADER {
            return Err(Error::ShortPayload { index });
        }
        let mut payload = vec![0u8; len + 4];
        let have = read_fill(&mut self.input, &mut payload)?;
        if have != payload.len() {
            return Err(Error::TruncatedTail {
                index,
                have: 4 + have,
                want: 4 + payload.len(),
            });
        }
        let (body, crc) = payload.split_at(len);
        let stored = u32::from_le_bytes(crc.try_into().expect("4 bytes"));
        if crc32fast::hash(body) != stored {
            return Err(Error::Crc { index });
        }
        let recv_ns = i64::from_le_bytes(body[..8].try_into().expect("8 bytes"));
        let source_id = u16::from_le_bytes(body[8..10].try_into().expect("2 bytes"));
        let bytes = body[PAYLOAD_HEADER..].to_vec();
        self.index += 1;
        self.pace(recv_ns);
        Ok(Some(Record {
            recv_ns,
            source_id,
            bytes,
        }))
    }

    fn pace(&mut self, recv_ns: i64) {
        if self.mode != Mode::Paced {
            return;
        }
        let (start, first_ns) = *self.first.get_or_insert((Instant::now(), recv_ns));
        let offset = recv_ns.saturating_sub(first_ns).max(0) as u64;
        let due = start + Duration::from_nanos(offset);
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        }
    }
}

impl<R: Read> Iterator for Reader<R> {
    type Item = Result<Record, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_record().transpose()
    }
}

fn read_u16<R: Read>(r: &mut R) -> Result<u16, Error> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    Ok(u16::from_le_bytes(b))
}

/// Like read_exact but returns how much arrived instead of failing on EOF.
fn read_fill<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<usize, std::io::Error> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

use std::io::{BufWriter, Write};

use crate::{Error, MAGIC, PAYLOAD_HEADER, VERSION};

pub struct Writer<W: Write> {
    out: BufWriter<W>,
    sources: usize,
    buf: Vec<u8>,
    records: u64,
    bytes: u64,
}

impl<W: Write> Writer<W> {
    /// Writes the header. `sources` names index into source_id.
    pub fn new(inner: W, sources: &[&str]) -> Result<Self, Error> {
        let mut out = BufWriter::new(inner);
        out.write_all(MAGIC)?;
        out.write_all(&VERSION.to_le_bytes())?;
        out.write_all(&(sources.len() as u16).to_le_bytes())?;
        for s in sources {
            out.write_all(&(s.len() as u16).to_le_bytes())?;
            out.write_all(s.as_bytes())?;
        }
        Ok(Self {
            out,
            sources: sources.len(),
            buf: Vec::with_capacity(4096),
            records: 0,
            bytes: 0,
        })
    }

    pub fn append(&mut self, recv_ns: i64, source_id: u16, bytes: &[u8]) -> Result<(), Error> {
        if source_id as usize >= self.sources {
            return Err(Error::UnknownSource(source_id));
        }
        self.buf.clear();
        self.buf.extend_from_slice(&recv_ns.to_le_bytes());
        self.buf.extend_from_slice(&source_id.to_le_bytes());
        self.buf.extend_from_slice(bytes);
        let len = (PAYLOAD_HEADER + bytes.len()) as u32;
        self.out.write_all(&len.to_le_bytes())?;
        self.out.write_all(&self.buf)?;
        self.out
            .write_all(&crc32fast::hash(&self.buf).to_le_bytes())?;
        self.records += 1;
        self.bytes += 4 + len as u64 + 4;
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), Error> {
        Ok(self.out.flush()?)
    }

    /// Flushes and returns the underlying writer.
    pub fn into_inner(self) -> Result<W, Error> {
        self.out.into_inner().map_err(|e| Error::Io(e.into_error()))
    }

    pub fn records(&self) -> u64 {
        self.records
    }

    /// Bytes written for frames, excluding the header.
    pub fn frame_bytes(&self) -> u64 {
        self.bytes
    }
}

//! On-disk format for recorded PTY sessions (`tests/corpus/*.ptyrec`).
//!
//! Exact child bytes, length-prefixed — never re-encoded, because the golden
//! tests must feed `GfxSplitter`/vt100 the same stream a live pane would see.
//!
//! Layout:
//!   header  = b"PTYREC1\n"  rows:u16le  cols:u16le
//!   chunk   = kind:u8  millis:u32le  len:u32le  payload:[u8; len]
//!   kind    = 0 child output bytes | 1 resize (payload rows:u16le cols:u16le)
//!
//! Shared three ways: the `ptyrec` recorder bin, `tests/golden.rs`, and
//! in-crate characterization tests, via `#[path]` includes — keep this file
//! dependency-free beyond std.

// Each including target uses only half of this API (the bin writes, the
// tests read), so per-target dead-code lints are expected noise.
#![allow(dead_code)]

use std::io::{self, Read, Write};
use std::time::Instant;

pub const MAGIC: &[u8; 8] = b"PTYREC1\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkKind {
    Output,
    Resize,
}

#[derive(Debug, Clone)]
pub struct Chunk {
    pub millis: u32,
    pub kind: ChunkKind,
    pub payload: Vec<u8>,
}

impl Chunk {
    /// Resize payload decoded, if this is a resize chunk.
    pub fn resize(&self) -> Option<(u16, u16)> {
        if self.kind != ChunkKind::Resize || self.payload.len() != 4 {
            return None;
        }
        let rows = u16::from_le_bytes([self.payload[0], self.payload[1]]);
        let cols = u16::from_le_bytes([self.payload[2], self.payload[3]]);
        Some((rows, cols))
    }
}

#[derive(Debug, Clone)]
pub struct Recording {
    pub rows: u16,
    pub cols: u16,
    pub chunks: Vec<Chunk>,
}

pub struct Writer<W: Write> {
    out: W,
    start: Instant,
}

impl<W: Write> Writer<W> {
    pub fn new(mut out: W, rows: u16, cols: u16) -> io::Result<Self> {
        out.write_all(MAGIC)?;
        out.write_all(&rows.to_le_bytes())?;
        out.write_all(&cols.to_le_bytes())?;
        Ok(Self {
            out,
            start: Instant::now(),
        })
    }

    fn chunk(&mut self, kind: u8, payload: &[u8]) -> io::Result<()> {
        let millis = self.start.elapsed().as_millis().min(u32::MAX as u128) as u32;
        self.out.write_all(&[kind])?;
        self.out.write_all(&millis.to_le_bytes())?;
        self.out.write_all(&(payload.len() as u32).to_le_bytes())?;
        self.out.write_all(payload)
    }

    pub fn output(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.chunk(0, bytes)
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> io::Result<()> {
        let mut p = [0u8; 4];
        p[..2].copy_from_slice(&rows.to_le_bytes());
        p[2..].copy_from_slice(&cols.to_le_bytes());
        self.chunk(1, &p)
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

pub fn read(mut r: impl Read) -> io::Result<Recording> {
    let mut magic = [0u8; 8];
    r.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a PTYREC1 file",
        ));
    }
    let mut sz = [0u8; 4];
    r.read_exact(&mut sz)?;
    let rows = u16::from_le_bytes([sz[0], sz[1]]);
    let cols = u16::from_le_bytes([sz[2], sz[3]]);
    let mut chunks = Vec::new();
    loop {
        let mut hdr = [0u8; 9];
        match r.read_exact(&mut hdr[..1]) {
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            other => other?,
        }
        r.read_exact(&mut hdr[1..])?;
        let kind = match hdr[0] {
            0 => ChunkKind::Output,
            1 => ChunkKind::Resize,
            k => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unknown chunk kind {k}"),
                ))
            }
        };
        let millis = u32::from_le_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]);
        let len = u32::from_le_bytes([hdr[5], hdr[6], hdr[7], hdr[8]]) as usize;
        let mut payload = vec![0u8; len];
        r.read_exact(&mut payload)?;
        chunks.push(Chunk {
            millis,
            kind,
            payload,
        });
    }
    Ok(Recording { rows, cols, chunks })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_output_and_resize() {
        let mut buf = Vec::new();
        {
            let mut w = Writer::new(&mut buf, 50, 120).unwrap();
            w.output(b"hello \x1b[1mworld\x1b[m").unwrap();
            w.resize(24, 80).unwrap();
            w.output(&[0xff, 0x00, 0x1b]).unwrap();
        }
        let rec = read(&buf[..]).unwrap();
        assert_eq!((rec.rows, rec.cols), (50, 120));
        assert_eq!(rec.chunks.len(), 3);
        assert_eq!(rec.chunks[0].payload, b"hello \x1b[1mworld\x1b[m");
        assert_eq!(rec.chunks[1].resize(), Some((24, 80)));
        assert_eq!(rec.chunks[2].kind, ChunkKind::Output);
    }

    #[test]
    fn truncated_or_foreign_files_are_rejected() {
        assert!(read(&b"CASTv2\n\x00\x00\x00"[..]).is_err());
        let mut buf = Vec::new();
        Writer::new(&mut buf, 10, 10)
            .unwrap()
            .output(b"abc")
            .unwrap();
        buf.truncate(buf.len() - 1);
        assert!(read(&buf[..]).is_err());
    }
}

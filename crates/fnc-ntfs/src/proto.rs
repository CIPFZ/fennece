//! Wire protocol between Fennec and `fennec-service` (SPEC ch.1 §2.4, ch.4 §8).
//!
//! Frames are `len u32 LE | payload` (payload ≤ [`MAX_FRAME`]). The service accepts
//! only three read-only requests — `QueryJournal`, `EnumMft`, `ReadUsn` — plus a
//! version handshake; everything else is rejected. Record data is passed through
//! as raw `USN_RECORD_V2/V3` buffers and parsed by the client ([`crate::record`]).
//!
//! ```text
//! Request  1 Hello { version u32 }
//!          2 QueryJournal { letter u16, create u8, max_size u64, delta u64 }
//!          3 EnumMft { letter u16 }
//!          4 ReadUsn { letter u16, journal_id u64, start i64 }
//! Response 1 Hello { version u32 }
//!          2 Journal { present u8, id u64, first u64, next u64, root_frn u64 }
//!          3 Chunk { raw records }                (EnumMft, repeated)
//!          4 End { records u64 }                  (EnumMft)
//!          5 Usn { next i64, raw records }        (ReadUsn)
//!          6 Error { kind u8, message utf8 }
//! ```

use std::io::{self, Read, Write};

/// Protocol version (bumped on any change).
pub const VERSION: u32 = 1;
/// Largest accepted frame.
pub const MAX_FRAME: usize = 16 << 20;
/// Largest journal the service creates on request (4 GiB).
pub const MAX_JOURNAL_SIZE: u64 = 4 << 30;

/// Pipe (local socket) name of the service for `instance`.
pub fn pipe_name(instance: &str) -> String {
    format!("Fennec.{instance}.svc")
}

/// Client request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Version handshake.
    Hello {
        /// Client protocol version.
        version: u32,
    },
    /// Journal state (optionally creating the journal).
    QueryJournal {
        /// Drive letter.
        letter: char,
        /// `Some((max_size, delta))`: create the journal if missing.
        create: Option<(u64, u64)>,
    },
    /// All MFT records.
    EnumMft {
        /// Drive letter.
        letter: char,
    },
    /// One journal read from `start` (blocks up to ~1 s for new records).
    ReadUsn {
        /// Drive letter.
        letter: char,
        /// Expected journal id.
        journal_id: u64,
        /// First USN to read.
        start: i64,
    },
}

/// Journal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct JournalInfo {
    /// Journal id.
    pub id: u64,
    /// First USN still in the journal.
    pub first_usn: i64,
    /// Next USN to be written.
    pub next_usn: i64,
    /// FRN of the root folder.
    pub root_frn: u64,
}

/// Why a request failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Journal wrapped or recreated.
    Reset,
    /// Volume or journal unavailable.
    Unavailable,
    /// Access denied.
    AccessDenied,
    /// Other I/O error.
    Io,
    /// Request not allowed or malformed.
    Rejected,
}

impl ErrorKind {
    fn code(self) -> u8 {
        match self {
            ErrorKind::Reset => 1,
            ErrorKind::Unavailable => 2,
            ErrorKind::AccessDenied => 3,
            ErrorKind::Io => 4,
            ErrorKind::Rejected => 5,
        }
    }

    fn from_code(c: u8) -> Self {
        match c {
            1 => ErrorKind::Reset,
            2 => ErrorKind::Unavailable,
            3 => ErrorKind::AccessDenied,
            4 => ErrorKind::Io,
            _ => ErrorKind::Rejected,
        }
    }
}

/// Service response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// Handshake reply.
    Hello {
        /// Service protocol version.
        version: u32,
    },
    /// Journal state (`None`: no journal).
    Journal(Option<JournalInfo>),
    /// A block of raw MFT records.
    Chunk(Vec<u8>),
    /// End of MFT enumeration.
    End {
        /// Records sent.
        records: u64,
    },
    /// Result of one journal read.
    Usn {
        /// Next USN to read.
        next: i64,
        /// Raw records.
        data: Vec<u8>,
    },
    /// Failure.
    Error {
        /// Category.
        kind: ErrorKind,
        /// Description.
        message: String,
    },
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("service protocol: {what}"))
}

struct Cur<'a>(&'a [u8]);

impl Cur<'_> {
    fn take<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        let (h, t) = self.0.split_first_chunk::<N>().ok_or_else(|| bad("truncated"))?;
        self.0 = t;
        Ok(*h)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take::<1>()?[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn letter(&mut self) -> io::Result<char> {
        let c = char::from_u32(u32::from(self.u16()?)).filter(char::is_ascii_alphabetic).ok_or_else(|| bad("drive"))?;
        Ok(c.to_ascii_uppercase())
    }
    fn end(&self) -> io::Result<()> {
        if self.0.is_empty() { Ok(()) } else { Err(bad("trailing bytes")) }
    }
}

impl Request {
    /// Serialised payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(32);
        match self {
            Request::Hello { version } => {
                b.push(1);
                b.extend_from_slice(&version.to_le_bytes());
            }
            Request::QueryJournal { letter, create } => {
                b.push(2);
                b.extend_from_slice(&(*letter as u16).to_le_bytes());
                let (max, delta) = create.unwrap_or((0, 0));
                b.push(u8::from(create.is_some()));
                b.extend_from_slice(&max.to_le_bytes());
                b.extend_from_slice(&delta.to_le_bytes());
            }
            Request::EnumMft { letter } => {
                b.push(3);
                b.extend_from_slice(&(*letter as u16).to_le_bytes());
            }
            Request::ReadUsn { letter, journal_id, start } => {
                b.push(4);
                b.extend_from_slice(&(*letter as u16).to_le_bytes());
                b.extend_from_slice(&journal_id.to_le_bytes());
                b.extend_from_slice(&start.to_le_bytes());
            }
        }
        b
    }

    /// Parses a payload; anything unknown or malformed is an error (and rejected).
    pub fn decode(p: &[u8]) -> io::Result<Self> {
        let mut c = Cur(p);
        let r = match c.u8()? {
            1 => Request::Hello { version: c.u32()? },
            2 => {
                let letter = c.letter()?;
                let create = c.u8()? != 0;
                let (max, delta) = (c.u64()?, c.u64()?);
                if create && (max == 0 || max > MAX_JOURNAL_SIZE || delta > max) {
                    return Err(bad("journal size"));
                }
                Request::QueryJournal { letter, create: create.then_some((max, delta)) }
            }
            3 => Request::EnumMft { letter: c.letter()? },
            4 => Request::ReadUsn { letter: c.letter()?, journal_id: c.u64()?, start: c.u64()? as i64 },
            _ => return Err(bad("unknown request")),
        };
        c.end()?;
        Ok(r)
    }
}

impl Response {
    /// Serialised payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        match self {
            Response::Hello { version } => {
                b.push(1);
                b.extend_from_slice(&version.to_le_bytes());
            }
            Response::Journal(j) => {
                b.push(2);
                b.push(u8::from(j.is_some()));
                let j = j.unwrap_or_default();
                for v in [j.id, j.first_usn as u64, j.next_usn as u64, j.root_frn] {
                    b.extend_from_slice(&v.to_le_bytes());
                }
            }
            Response::Chunk(d) => {
                b.reserve(d.len() + 1);
                b.push(3);
                b.extend_from_slice(d);
            }
            Response::End { records } => {
                b.push(4);
                b.extend_from_slice(&records.to_le_bytes());
            }
            Response::Usn { next, data } => {
                b.reserve(data.len() + 9);
                b.push(5);
                b.extend_from_slice(&next.to_le_bytes());
                b.extend_from_slice(data);
            }
            Response::Error { kind, message } => {
                b.push(6);
                b.push(kind.code());
                b.extend_from_slice(message.as_bytes());
            }
        }
        b
    }

    /// Parses a payload.
    pub fn decode(p: &[u8]) -> io::Result<Self> {
        let mut c = Cur(p);
        Ok(match c.u8()? {
            1 => Response::Hello { version: c.u32()? },
            2 => {
                let present = c.u8()? != 0;
                let j = JournalInfo {
                    id: c.u64()?,
                    first_usn: c.u64()? as i64,
                    next_usn: c.u64()? as i64,
                    root_frn: c.u64()?,
                };
                Response::Journal(present.then_some(j))
            }
            3 => Response::Chunk(c.0.to_vec()),
            4 => Response::End { records: c.u64()? },
            5 => {
                let next = c.u64()? as i64;
                Response::Usn { next, data: c.0.to_vec() }
            }
            6 => {
                let kind = ErrorKind::from_code(c.u8()?);
                Response::Error { kind, message: String::from_utf8_lossy(c.0).into_owned() }
            }
            _ => return Err(bad("unknown response")),
        })
    }
}

/// Writes one frame.
pub fn write_frame(w: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_FRAME {
        return Err(bad("frame too large"));
    }
    w.write_all(&(payload.len() as u32).to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

/// Reads one frame (`None` on a clean end of stream).
pub fn read_frame(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let n = u32::from_le_bytes(len) as usize;
    if n > MAX_FRAME {
        return Err(bad("frame too large"));
    }
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_roundtrip() {
        for r in [
            Request::Hello { version: VERSION },
            Request::QueryJournal { letter: 'C', create: None },
            Request::QueryJournal { letter: 'D', create: Some((32 << 20, 4 << 20)) },
            Request::EnumMft { letter: 'E' },
            Request::ReadUsn { letter: 'C', journal_id: 0x01DC_0000_1234, start: 987_654_321 },
        ] {
            assert_eq!(Request::decode(&r.encode()).unwrap(), r);
        }
    }

    #[test]
    fn responses_roundtrip() {
        for r in [
            Response::Hello { version: VERSION },
            Response::Journal(None),
            Response::Journal(Some(JournalInfo { id: 7, first_usn: 1, next_usn: 99, root_frn: 0x0005_0000_0000_0005 })),
            Response::Chunk(vec![1, 2, 3]),
            Response::End { records: 42 },
            Response::Usn { next: 1234, data: vec![9; 100] },
            Response::Error { kind: ErrorKind::Reset, message: "journal wrapped".into() },
        ] {
            assert_eq!(Response::decode(&r.encode()).unwrap(), r);
        }
    }

    #[test]
    fn hostile_requests_are_rejected() {
        assert!(Request::decode(&[]).is_err());
        assert!(Request::decode(&[9, 0, 0]).is_err(), "unknown op");
        assert!(Request::decode(&[3, b'1', 0]).is_err(), "not a drive letter");
        assert!(Request::decode(&[3, b'C', 0, 0]).is_err(), "trailing bytes");
        let mut too_big = Request::QueryJournal { letter: 'C', create: Some((MAX_JOURNAL_SIZE + 1, 0)) }.encode();
        assert!(Request::decode(&too_big).is_err());
        too_big.truncate(5);
        assert!(Request::decode(&too_big).is_err());
        // Lower-case letters are normalised.
        assert_eq!(Request::decode(&[3, b'c', 0]).unwrap(), Request::EnumMft { letter: 'C' });
    }

    #[test]
    fn frames() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"abc").unwrap();
        write_frame(&mut buf, b"").unwrap();
        let mut r = &buf[..];
        assert_eq!(read_frame(&mut r).unwrap().unwrap(), b"abc");
        assert_eq!(read_frame(&mut r).unwrap().unwrap(), b"");
        assert!(read_frame(&mut r).unwrap().is_none());
        let huge = ((MAX_FRAME + 1) as u32).to_le_bytes();
        assert!(read_frame(&mut &huge[..]).is_err());
    }
}

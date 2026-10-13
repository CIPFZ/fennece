//! How an [`crate::NtfsSource`] reaches its volume: directly (elevated process with a
//! volume handle) or through `fennec-service` (SPEC ch.1 §2.4).

use std::io;
use std::time::{Duration, Instant};

use fnc_index::Meta;
use fnc_platform::SourceError;
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, Stream};
use parking_lot::Mutex;

use crate::proto::{self, ErrorKind, JournalInfo, Request, Response};
use crate::record::{UsnRecord, records};
use crate::win::{self, AlignedBuf, Handle, ReadError};

/// Read-only access to one volume's MFT and journal.
pub trait VolumeAccess: Send + Sync {
    /// Journal state, creating the journal first when `create` is given and none exists.
    fn query_journal(&self, create: Option<(u64, u64)>) -> Result<Option<JournalInfo>, SourceError>;
    /// Calls `f` for every MFT record until it returns `false`; returns the count.
    fn enum_mft(&self, f: &mut dyn FnMut(&UsnRecord<'_>) -> bool) -> Result<u64, SourceError>;
    /// One journal read from `start` (blocks up to ~1 s); raw records go to `out`.
    fn read_journal(&self, journal_id: u64, start: i64, out: &mut Vec<u8>) -> Result<i64, ReadError>;
    /// Size/dates/attributes of a file by FRN.
    fn stat(&self, frn: u64) -> io::Result<Meta>;
    /// `"direct"` or `"service"` (logging).
    fn mode(&self) -> &'static str;
}

fn io_err(ctx: String) -> impl FnOnce(io::Error) -> SourceError {
    move |e| SourceError::io(ctx, e)
}

/// Volume handle opened by this (elevated) process.
pub struct DirectAccess {
    letter: char,
    handle: Handle,
    buf: Mutex<AlignedBuf>,
}

impl DirectAccess {
    /// Opens `X:` (needs administrator rights).
    pub fn open(letter: char) -> Result<Self, SourceError> {
        let handle = win::open_volume(letter).map_err(io_err(format!("open volume {letter}:")))?;
        Ok(Self { letter, handle, buf: Mutex::new(AlignedBuf::new(64 * 1024)) })
    }
}

/// Shared by [`DirectAccess`] and the service: journal state (optionally creating it).
pub fn query_journal_on(
    h: &Handle,
    letter: char,
    create: Option<(u64, u64)>,
) -> Result<Option<JournalInfo>, SourceError> {
    let ctx = || format!("query journal {letter}:");
    let to_info = |j: win::Journal| JournalInfo { id: j.id, first_usn: j.first_usn, next_usn: j.next_usn, root_frn: 0 };
    match win::query_journal(h).map_err(io_err(ctx()))? {
        Some(j) => Ok(Some(to_info(j))),
        None => match create {
            Some((max, delta)) => {
                tracing::info!(volume = %letter, "creating USN journal");
                win::create_journal(h, max, delta).map_err(io_err(format!("create journal {letter}:")))?;
                Ok(win::query_journal(h).map_err(io_err(ctx()))?.map(to_info))
            }
            None => Ok(None),
        },
    }
}

impl VolumeAccess for DirectAccess {
    fn query_journal(&self, create: Option<(u64, u64)>) -> Result<Option<JournalInfo>, SourceError> {
        query_journal_on(&self.handle, self.letter, create)
    }

    fn enum_mft(&self, f: &mut dyn FnMut(&UsnRecord<'_>) -> bool) -> Result<u64, SourceError> {
        // HighUsn = MAX: records modified while enumerating are included (not skipped);
        // replaying the journal from the position captured before is idempotent.
        win::enum_mft(&self.handle, i64::MAX, f).map_err(io_err(format!("enumerate MFT {}:", self.letter)))
    }

    fn read_journal(&self, journal_id: u64, start: i64, out: &mut Vec<u8>) -> Result<i64, ReadError> {
        let mut buf = self.buf.lock();
        let (next, len) = win::read_journal(&self.handle, journal_id, start, true, &mut buf)?;
        out.clear();
        out.extend_from_slice(&buf.bytes(len + 8)[8..]);
        Ok(next)
    }

    fn stat(&self, frn: u64) -> io::Result<Meta> {
        win::stat_frn(&self.handle, frn)
    }

    fn mode(&self) -> &'static str {
        "direct"
    }
}

/// Access through `fennec-service` (non-elevated process).
pub struct ServiceAccess {
    letter: char,
    instance: String,
    conn: Mutex<Option<Stream>>,
    /// Root folder handle: the volume hint for `OpenFileById`, which only needs the
    /// caller's own access to the file.
    hint: Handle,
}

fn connect(instance: &str) -> io::Result<Stream> {
    let name = proto::pipe_name(instance).to_ns_name::<GenericNamespaced>()?;
    let mut s = Stream::connect(name)?;
    match call(&mut s, &Request::Hello { version: proto::VERSION })? {
        Response::Hello { version } if version == proto::VERSION => Ok(s),
        Response::Hello { version } => {
            Err(io::Error::other(format!("fennec-service speaks protocol {version}, expected {}", proto::VERSION)))
        }
        other => Err(io::Error::other(format!("unexpected handshake reply {other:?}"))),
    }
}

fn call(s: &mut Stream, r: &Request) -> io::Result<Response> {
    proto::write_frame(s, &r.encode())?;
    receive(s)
}

fn receive(s: &mut Stream) -> io::Result<Response> {
    match proto::read_frame(s)? {
        Some(p) => Response::decode(&p),
        None => Err(io::Error::new(io::ErrorKind::UnexpectedEof, "fennec-service closed the connection")),
    }
}

fn service_error(letter: char, kind: ErrorKind, message: String) -> SourceError {
    match kind {
        ErrorKind::Reset => SourceError::JournalReset(format!("{letter}:\\")),
        ErrorKind::Unavailable => SourceError::Offline(format!("{letter}:\\")),
        ErrorKind::AccessDenied => SourceError::AccessDenied(message),
        ErrorKind::Io | ErrorKind::Rejected => {
            SourceError::io(format!("fennec-service ({letter}:)"), io::Error::other(message))
        }
    }
}

impl ServiceAccess {
    /// Connects to the service of `instance` for volume `letter`.
    pub fn connect(instance: &str, letter: char) -> Result<Self, SourceError> {
        let hint = win::open_root(letter).map_err(io_err(format!("open {letter}:\\")))?;
        let conn = connect(instance).map_err(io_err("connect to fennec-service".into()))?;
        Ok(Self { letter, instance: instance.to_string(), conn: Mutex::new(Some(conn)), hint })
    }

    /// Runs `r` on the shared connection, reconnecting for up to 10 s if the service
    /// restarted (SPEC ch.4 §8).
    fn request(&self, r: &Request) -> io::Result<Response> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut guard = self.conn.lock();
        loop {
            let res = match guard.as_mut() {
                Some(s) => call(s, r),
                None => connect(&self.instance).and_then(|mut s| {
                    let res = call(&mut s, r);
                    *guard = Some(s);
                    res
                }),
            };
            match res {
                Ok(resp) => return Ok(resp),
                Err(e) if Instant::now() >= deadline => return Err(e),
                Err(e) => {
                    tracing::warn!(error = %e, "fennec-service connection lost; reconnecting");
                    *guard = None;
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
    }
}

impl VolumeAccess for ServiceAccess {
    fn query_journal(&self, create: Option<(u64, u64)>) -> Result<Option<JournalInfo>, SourceError> {
        let ctx = format!("fennec-service ({}:)", self.letter);
        match self.request(&Request::QueryJournal { letter: self.letter, create }).map_err(io_err(ctx.clone()))? {
            Response::Journal(j) => Ok(j),
            Response::Error { kind, message } => Err(service_error(self.letter, kind, message)),
            other => Err(SourceError::io(ctx, io::Error::other(format!("unexpected reply {other:?}")))),
        }
    }

    fn enum_mft(&self, f: &mut dyn FnMut(&UsnRecord<'_>) -> bool) -> Result<u64, SourceError> {
        // A dedicated connection: stopping early just drops it.
        let ctx = format!("fennec-service ({}:)", self.letter);
        let mut s = connect(&self.instance).map_err(io_err(ctx.clone()))?;
        proto::write_frame(&mut s, &Request::EnumMft { letter: self.letter }.encode()).map_err(io_err(ctx.clone()))?;
        let mut count = 0u64;
        loop {
            match receive(&mut s).map_err(io_err(ctx.clone()))? {
                Response::Chunk(data) => {
                    for r in records(&data) {
                        match r {
                            Ok(r) => {
                                count += 1;
                                if !f(&r) {
                                    return Ok(count);
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "malformed MFT record from fennec-service");
                                break;
                            }
                        }
                    }
                }
                Response::End { .. } => return Ok(count),
                Response::Error { kind, message } => return Err(service_error(self.letter, kind, message)),
                other => return Err(SourceError::io(ctx, io::Error::other(format!("unexpected reply {other:?}")))),
            }
        }
    }

    fn read_journal(&self, journal_id: u64, start: i64, out: &mut Vec<u8>) -> Result<i64, ReadError> {
        match self.request(&Request::ReadUsn { letter: self.letter, journal_id, start }) {
            Ok(Response::Usn { next, data }) => {
                *out = data;
                Ok(next)
            }
            Ok(Response::Error { kind: ErrorKind::Reset, .. }) => Err(ReadError::Reset),
            Ok(Response::Error { kind: ErrorKind::Unavailable, .. }) => Err(ReadError::Unavailable),
            Ok(Response::Error { message, .. }) => Err(ReadError::Io(io::Error::other(message))),
            Ok(other) => Err(ReadError::Io(io::Error::other(format!("unexpected reply {other:?}")))),
            Err(e) => Err(ReadError::Io(e)),
        }
    }

    fn stat(&self, frn: u64) -> io::Result<Meta> {
        win::stat_frn(&self.hint, frn)
    }

    fn mode(&self) -> &'static str {
        "service"
    }
}

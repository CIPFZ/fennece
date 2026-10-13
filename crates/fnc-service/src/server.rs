//! The pipe server: one thread per connection, read-only volume operations only.

use std::collections::HashMap;
use std::io;
use std::time::Instant;

use fnc_ntfs::access::query_journal_on;
use fnc_ntfs::proto::{self, ErrorKind, Request, Response};
use fnc_ntfs::win::{self, AlignedBuf, Handle, ReadError};
use fnc_platform::{SourceError, StopFlag};
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Stream};
use interprocess::os::windows::local_socket::ListenerOptionsExt;
use interprocess::os::windows::security_descriptor::SecurityDescriptor;
use widestring::U16CString;

/// Protected DACL: full access for LocalSystem and Administrators, read/write for
/// interactive users; nobody else (network logons, services) can connect.
const PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)";

/// Serves until `stop` (checked between connections).
pub fn serve(instance: &str, stop: &StopFlag) -> io::Result<()> {
    let sddl = U16CString::from_str(PIPE_SDDL).map_err(io::Error::other)?;
    let sd = SecurityDescriptor::deserialize(&sddl)?;
    let pipe = proto::pipe_name(instance);
    let name = pipe.as_str().to_ns_name::<GenericNamespaced>()?;
    let listener = ListenerOptions::new().name(name).security_descriptor(sd).create_sync()?;
    tracing::info!(pipe = %pipe, "fennec-service listening");
    for conn in listener.incoming() {
        if stop.is_stopped() {
            break;
        }
        match conn {
            Ok(c) => {
                let _ = std::thread::Builder::new().name("fennec-svc-conn".into()).spawn(move || {
                    if let Err(e) = handle(c) {
                        tracing::debug!(error = %e, "connection ended");
                    }
                });
            }
            Err(e) => tracing::warn!(error = %e, "accept failed"),
        }
    }
    Ok(())
}

/// Wakes a blocked `serve` after `stop` was set.
pub fn wake(instance: &str) {
    if let Ok(name) = proto::pipe_name(instance).to_ns_name::<GenericNamespaced>() {
        let _ = Stream::connect(name);
    }
}

struct Volume {
    handle: Handle,
    buf: AlignedBuf,
}

fn error(kind: ErrorKind, message: impl Into<String>) -> Response {
    Response::Error { kind, message: message.into() }
}

fn source_error(e: &SourceError) -> Response {
    let kind = match e {
        SourceError::AccessDenied(_) => ErrorKind::AccessDenied,
        SourceError::JournalReset(_) => ErrorKind::Reset,
        SourceError::Offline(_) => ErrorKind::Unavailable,
        _ => ErrorKind::Io,
    };
    error(kind, e.to_string())
}

/// Only local fixed NTFS/ReFS drives are served.
fn allowed(letter: char) -> bool {
    fnc_ntfs::list_volumes().iter().any(|d| d.letter == letter)
}

fn handle(mut conn: Stream) -> io::Result<()> {
    let t = Instant::now();
    let mut volumes: HashMap<char, Volume> = HashMap::new();
    let mut requests = 0u64;
    while let Some(frame) = proto::read_frame(&mut conn)? {
        requests += 1;
        let req = match Request::decode(&frame) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "rejected malformed request");
                proto::write_frame(&mut conn, &error(ErrorKind::Rejected, e.to_string()).encode())?;
                return Ok(());
            }
        };
        let letter = match req {
            Request::Hello { .. } => None,
            Request::QueryJournal { letter, .. } | Request::EnumMft { letter } | Request::ReadUsn { letter, .. } => {
                Some(letter)
            }
        };
        if let Some(l) = letter
            && !volumes.contains_key(&l)
        {
            if !allowed(l) {
                proto::write_frame(
                    &mut conn,
                    &error(ErrorKind::Rejected, format!("{l}: is not a local NTFS/ReFS drive")).encode(),
                )?;
                continue;
            }
            match win::open_volume(l) {
                Ok(handle) => {
                    volumes.insert(l, Volume { handle, buf: AlignedBuf::new(64 * 1024) });
                }
                Err(e) => {
                    proto::write_frame(&mut conn, &source_error(&SourceError::io(format!("open {l}:"), e)).encode())?;
                    continue;
                }
            }
        }
        let vol = letter.and_then(|l| volumes.get_mut(&l));
        match (req, vol) {
            (Request::Hello { version }, _) => {
                tracing::info!(client_version = version, "client connected");
                proto::write_frame(&mut conn, &Response::Hello { version: proto::VERSION }.encode())?;
            }
            (Request::QueryJournal { letter, create }, Some(v)) => {
                let resp = match query_journal_on(&v.handle, letter, create) {
                    Ok(j) => Response::Journal(j.map(|mut j| {
                        j.root_frn = win::root_frn(letter).unwrap_or(0);
                        j
                    })),
                    Err(e) => source_error(&e),
                };
                proto::write_frame(&mut conn, &resp.encode())?;
            }
            (Request::EnumMft { letter }, Some(v)) => {
                let mut records = 0u64;
                let mut write_err: Option<io::Error> = None;
                let r = win::enum_mft_raw(&v.handle, i64::MAX, |chunk| {
                    records += fnc_ntfs::record::records(chunk).count() as u64;
                    match proto::write_frame(&mut conn, &Response::Chunk(chunk.to_vec()).encode()) {
                        Ok(()) => true,
                        Err(e) => {
                            write_err = Some(e);
                            false
                        }
                    }
                });
                if let Some(e) = write_err {
                    return Err(e);
                }
                let resp = match r {
                    Ok(()) => Response::End { records },
                    Err(e) => source_error(&SourceError::io(format!("enumerate MFT {letter}:"), e)),
                };
                tracing::info!(volume = %letter, records, "MFT enumerated for client");
                proto::write_frame(&mut conn, &resp.encode())?;
            }
            (Request::ReadUsn { journal_id, start, .. }, Some(v)) => {
                let resp = match win::read_journal(&v.handle, journal_id, start, true, &mut v.buf) {
                    Ok((next, len)) => Response::Usn { next, data: v.buf.bytes(len + 8)[8..].to_vec() },
                    Err(ReadError::Reset) => error(ErrorKind::Reset, "journal reset"),
                    Err(ReadError::Unavailable) => error(ErrorKind::Unavailable, "journal unavailable"),
                    Err(ReadError::Io(e)) => error(ErrorKind::Io, e.to_string()),
                };
                proto::write_frame(&mut conn, &resp.encode())?;
            }
            (_, None) => proto::write_frame(&mut conn, &error(ErrorKind::Rejected, "no volume").encode())?,
        }
    }
    tracing::info!(requests, elapsed = ?t.elapsed(), "client disconnected");
    Ok(())
}

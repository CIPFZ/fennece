//! Native IPC server: one thread per connection, bounded concurrency.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Stream};

use crate::backend::{IndexBackend, Peer, PeerKind};
use crate::proto::{self, MAX_REQUEST_FRAME, MAX_RESPONSE_FRAME, Request, Response, Status};

/// Protected DACL of the pipe: full access for LocalSystem and Administrators, read/write
/// for interactive users; network and service logons cannot connect (same as
/// `fennec-service`, ADR-011).
pub const PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)";

/// Server limits.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Most simultaneous connections; further clients get a `Busy` response.
    pub max_connections: usize,
    /// A connection idle for this long is closed (where the platform supports read
    /// timeouts; Windows named pipes do not).
    pub idle_timeout: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self { max_connections: 32, idle_timeout: Duration::from_secs(600) }
    }
}

/// Stop request for [`serve`].
#[derive(Clone, Default, Debug)]
pub struct StopSignal(Arc<AtomicBool>);

impl StopSignal {
    /// New, not stopped.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests the server to stop (call [`wake`] afterwards so a blocked accept returns).
    pub fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// `true` once stopped.
    pub fn is_stopped(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

fn listener(instance: &str) -> io::Result<interprocess::local_socket::Listener> {
    let pipe = proto::pipe_name(instance);
    let name = pipe.as_str().to_ns_name::<GenericNamespaced>()?;
    let opts = ListenerOptions::new().name(name);
    #[cfg(windows)]
    let opts = {
        use interprocess::os::windows::local_socket::ListenerOptionsExt;
        use interprocess::os::windows::security_descriptor::SecurityDescriptor;
        let sddl = widestring::U16CString::from_str(PIPE_SDDL).map_err(io::Error::other)?;
        opts.security_descriptor(SecurityDescriptor::deserialize(&sddl)?)
    };
    opts.create_sync()
}

/// Serves `Fennec.<instance>.ipc` until `stop` is set (checked after every accepted
/// connection; call [`wake`] to unblock). Spawns one thread per connection
/// (`fennec-ipc-conn`), at most `config.max_connections` at a time. Returns an error only
/// if the socket cannot be created (e.g. another instance owns the name).
pub fn serve(instance: &str, backend: Arc<IndexBackend>, stop: &StopSignal, config: &ServerConfig) -> io::Result<()> {
    let listener = listener(instance)?;
    tracing::info!(pipe = %proto::pipe_name(instance), "IPC server listening");
    run_listener(listener, backend, stop, config)
}

/// Wakes a blocked [`serve`] after [`StopSignal::stop`].
pub fn wake(instance: &str) {
    if let Ok(name) = proto::pipe_name(instance).to_ns_name::<GenericNamespaced>() {
        let _ = Stream::connect(name);
    }
}

fn peer_of(conn: &Stream) -> Peer {
    // `pid_t` (i32) on Unix, `u32` on Windows.
    #[allow(clippy::useless_conversion)]
    let pid = conn.peer_creds().ok().and_then(|c| c.pid()).and_then(|p| u32::try_from(p).ok());
    Peer { kind: PeerKind::Pipe, pid }
}

fn handle_connection(mut conn: Stream, backend: &IndexBackend, idle: Duration) -> io::Result<()> {
    // Not supported on Windows named pipes; harmless there.
    let _ = conn.set_recv_timeout(Some(idle));
    let peer = peer_of(&conn);
    while let Some(frame) = proto::read_frame(&mut conn, MAX_REQUEST_FRAME)? {
        let req = match Request::decode(&frame) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "rejected malformed IPC request");
                let resp = Response::error(0, Status::BadRequest, e.to_string());
                if let Ok(bytes) = resp.encode() {
                    proto::write_frame(&mut conn, &bytes, MAX_RESPONSE_FRAME)?;
                }
                // Untrusted peer sent garbage: close the connection.
                return Ok(());
            }
        };
        let resp = match backend.handle(&req.op, &peer) {
            Ok(payload) => Response::ok(req.id, payload),
            Err(e) => Response::error(req.id, e.status, e.message),
        };
        let bytes = match resp.encode() {
            Ok(b) if b.len() <= MAX_RESPONSE_FRAME => b,
            Ok(_) => Response::error(req.id, Status::TooLarge, "response too large; request fewer results")
                .encode()
                .map_err(io::Error::other)?,
            Err(e) => Response::error(req.id, Status::Internal, e.to_string()).encode().map_err(io::Error::other)?,
        };
        proto::write_frame(&mut conn, &bytes, MAX_RESPONSE_FRAME)?;
    }
    Ok(())
}

/// A server running on its own thread (`fennec-ipc`); dropping it stops the server.
pub struct ServerHandle {
    instance: String,
    stop: StopSignal,
    thread: Option<JoinHandle<io::Result<()>>>,
}

impl ServerHandle {
    /// Stops the server and waits for the accept loop to end (open connections finish
    /// their current request on their own threads).
    pub fn stop(mut self) -> io::Result<()> {
        self.shutdown()
    }

    fn shutdown(&mut self) -> io::Result<()> {
        self.stop.stop();
        wake(&self.instance);
        match self.thread.take() {
            Some(t) => t.join().unwrap_or_else(|_| Err(io::Error::other("IPC server thread panicked"))),
            None => Ok(()),
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// Starts [`serve`] on a new thread. The socket is created before this returns, so
/// clients can connect immediately; an error means the name is taken or the socket could
/// not be created.
pub fn spawn(instance: &str, backend: Arc<IndexBackend>, config: ServerConfig) -> io::Result<ServerHandle> {
    let listener = listener(instance)?;
    let stop = StopSignal::new();
    let (stop2, inst) = (stop.clone(), instance.to_string());
    let thread = std::thread::Builder::new().name("fennec-ipc".into()).spawn(move || {
        tracing::info!(pipe = %proto::pipe_name(&inst), "IPC server listening");
        run_listener(listener, backend, &stop2, &config)
    })?;
    Ok(ServerHandle { instance: instance.to_string(), stop, thread: Some(thread) })
}

fn run_listener(
    listener: interprocess::local_socket::Listener,
    backend: Arc<IndexBackend>,
    stop: &StopSignal,
    config: &ServerConfig,
) -> io::Result<()> {
    let active = Arc::new(AtomicUsize::new(0));
    for conn in listener.incoming() {
        if stop.is_stopped() {
            break;
        }
        let mut conn = match conn {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "IPC accept failed");
                continue;
            }
        };
        if active.load(Ordering::SeqCst) >= config.max_connections {
            let resp = Response::error(0, Status::Busy, "too many IPC clients");
            if let Ok(bytes) = resp.encode() {
                let _ = proto::write_frame(&mut conn, &bytes, MAX_RESPONSE_FRAME);
            }
            continue;
        }
        active.fetch_add(1, Ordering::SeqCst);
        let (backend, active2, idle) = (backend.clone(), active.clone(), config.idle_timeout);
        let spawned = std::thread::Builder::new().name("fennec-ipc-conn".into()).spawn(move || {
            if let Err(e) = handle_connection(conn, &backend, idle) {
                tracing::debug!(error = %e, "IPC connection ended");
            }
            active2.fetch_sub(1, Ordering::SeqCst);
        });
        if let Err(e) = spawned {
            active.fetch_sub(1, Ordering::SeqCst);
            tracing::warn!(error = %e, "cannot spawn IPC connection thread");
        }
    }
    tracing::info!("IPC server stopped");
    Ok(())
}

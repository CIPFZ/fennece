//! Blocking IPC client.

use std::io;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, Stream};

use crate::proto::{
    self, BookmarkEntry, FilterEntry, IndexStatus, MAX_REQUEST_FRAME, MAX_RESPONSE_FRAME, Op, Payload, Query, Request,
    Response, Results, SearchSpec, Status, VersionInfo, VolumeEntry,
};

/// Client errors.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// No server is listening on the instance's socket.
    #[error("no Fennec instance is running ({0})")]
    Unavailable(io::Error),
    /// The server did not answer in time.
    #[error("timed out waiting for Fennec")]
    Timeout,
    /// Transport failure (the connection is unusable afterwards).
    #[error("IPC I/O error: {0}")]
    Io(io::Error),
    /// Unexpected or malformed response.
    #[error("IPC protocol error: {0}")]
    Protocol(String),
    /// The server reported an error.
    #[error("{message} ({status:?})")]
    Server {
        /// Status.
        status: Status,
        /// Message.
        message: String,
    },
}

type Exchange = (Vec<u8>, Sender<io::Result<Vec<u8>>>);

/// A connection to a running instance. Requests are sent on a worker thread so every
/// call can time out, also on Windows where named pipes have no read timeouts.
pub struct Client {
    tx: Option<Sender<Exchange>>,
    worker: Option<JoinHandle<()>>,
    next_id: u32,
    timeout: Option<Duration>,
    broken: bool,
}

fn worker(mut stream: Stream, rx: Receiver<Exchange>) {
    while let Ok((req, reply)) = rx.recv() {
        let r = proto::write_frame(&mut stream, &req, MAX_REQUEST_FRAME).and_then(|()| {
            proto::read_frame(&mut stream, MAX_RESPONSE_FRAME)?
                .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "server closed the connection"))
        });
        let failed = r.is_err();
        if reply.send(r).is_err() || failed {
            return;
        }
    }
}

impl Client {
    /// Connects to `Fennec.<instance>.ipc`, retrying for up to `timeout` (the pipe may be
    /// busy or the app still starting). Later calls use `timeout` too (see
    /// [`Client::set_timeout`]).
    pub fn connect(instance: &str, timeout: Duration) -> Result<Self, ClientError> {
        let pipe = proto::pipe_name(instance);
        let deadline = Instant::now() + timeout;
        let stream = loop {
            let name = pipe.as_str().to_ns_name::<GenericNamespaced>().map_err(ClientError::Unavailable)?;
            match Stream::connect(name) {
                Ok(s) => break s,
                Err(e) => {
                    if Instant::now() >= deadline {
                        return Err(ClientError::Unavailable(e));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        };
        let (tx, rx) = crossbeam_channel::unbounded::<Exchange>();
        let worker = std::thread::Builder::new()
            .name("fennec-ipc-client".into())
            .spawn(move || worker(stream, rx))
            .map_err(ClientError::Io)?;
        Ok(Self { tx: Some(tx), worker: Some(worker), next_id: 1, timeout: Some(timeout), broken: false })
    }

    /// Per-call time limit (`None` = wait forever).
    pub fn set_timeout(&mut self, timeout: Option<Duration>) {
        self.timeout = timeout;
    }

    /// Sends one operation and returns the payload.
    pub fn call(&mut self, op: Op) -> Result<Payload, ClientError> {
        if self.broken {
            return Err(ClientError::Io(io::Error::new(io::ErrorKind::NotConnected, "connection lost")));
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let req = Request { id, op }.encode().map_err(|e| ClientError::Protocol(e.to_string()))?;
        let (rtx, rrx) = crossbeam_channel::bounded(1);
        let Some(tx) = &self.tx else { return Err(ClientError::Protocol("client closed".into())) };
        if tx.send((req, rtx)).is_err() {
            self.broken = true;
            return Err(ClientError::Io(io::Error::new(io::ErrorKind::BrokenPipe, "connection lost")));
        }
        let got = match self.timeout {
            Some(t) => rrx.recv_timeout(t).map_err(|e| match e {
                crossbeam_channel::RecvTimeoutError::Timeout => ClientError::Timeout,
                crossbeam_channel::RecvTimeoutError::Disconnected => {
                    ClientError::Io(io::Error::new(io::ErrorKind::BrokenPipe, "connection lost"))
                }
            }),
            None => {
                rrx.recv().map_err(|_| ClientError::Io(io::Error::new(io::ErrorKind::BrokenPipe, "connection lost")))
            }
        };
        let bytes = match got {
            Ok(Ok(b)) => b,
            Ok(Err(e)) => {
                self.broken = true;
                return Err(ClientError::Io(e));
            }
            Err(e) => {
                // A late answer would be mistaken for the next one: give up on this connection.
                self.broken = true;
                return Err(e);
            }
        };
        let resp = Response::decode(&bytes).map_err(|e| ClientError::Protocol(e.to_string()))?;
        if resp.status != Status::Ok {
            return Err(ClientError::Server { status: resp.status, message: resp.message });
        }
        if resp.id != id {
            self.broken = true;
            return Err(ClientError::Protocol(format!("response id {} for request {id}", resp.id)));
        }
        Ok(resp.payload)
    }

    fn unexpected<T>(p: Payload) -> Result<T, ClientError> {
        Err(ClientError::Protocol(format!("unexpected payload {p:?}")))
    }

    /// Runs a query (one page).
    pub fn query(&mut self, q: &Query) -> Result<Results, ClientError> {
        match self.call(Op::Query(q.clone()))? {
            Payload::Results(r) => Ok(r),
            p => Self::unexpected(p),
        }
    }

    /// Another page of a query.
    pub fn query_continue(&mut self, query_id: u64, offset: u64, limit: u64) -> Result<Results, ClientError> {
        match self.call(Op::QueryContinue { query_id, offset, limit })? {
            Payload::Results(r) => Ok(r),
            p => Self::unexpected(p),
        }
    }

    /// Runs a query and follows pages until `q.limit` rows (or the end) are collected.
    pub fn query_all(&mut self, q: &Query) -> Result<Results, ClientError> {
        let mut first = self.query(q)?;
        let want = q.limit.min(first.total.saturating_sub(q.offset));
        while (first.len() as u64) < want {
            let got = first.len() as u64;
            let next = self.query_continue(first.query_id, q.offset + got, want - got)?;
            if next.is_empty() {
                break;
            }
            append(&mut first.columns, next.columns);
        }
        let _ = self.call(Op::ReleaseQuery { query_id: first.query_id });
        Ok(first)
    }

    /// Number of results of a search.
    pub fn result_count(&mut self, spec: &SearchSpec) -> Result<u64, ClientError> {
        match self.call(Op::GetResultCount(spec.clone()))? {
            Payload::Count(n) => Ok(n),
            p => Self::unexpected(p),
        }
    }

    /// Total size of the matching files.
    pub fn total_size(&mut self, spec: &SearchSpec) -> Result<u64, ClientError> {
        match self.call(Op::GetTotalSize(spec.clone()))? {
            Payload::Size(n) => Ok(n),
            p => Self::unexpected(p),
        }
    }

    /// Versions.
    pub fn version(&mut self) -> Result<VersionInfo, ClientError> {
        match self.call(Op::GetVersion)? {
            Payload::Version(v) => Ok(v),
            p => Self::unexpected(p),
        }
    }

    /// Whether the database is loaded.
    pub fn is_db_loaded(&mut self) -> Result<bool, ClientError> {
        match self.call(Op::IsDbLoaded)? {
            Payload::Bool(b) => Ok(b),
            p => Self::unexpected(p),
        }
    }

    /// Index status.
    pub fn index_status(&mut self) -> Result<IndexStatus, ClientError> {
        match self.call(Op::GetIndexStatus)? {
            Payload::IndexStatus(s) => Ok(s),
            p => Self::unexpected(p),
        }
    }

    fn unit(&mut self, op: Op) -> Result<(), ClientError> {
        self.call(op).map(|_| ())
    }

    /// Rebuilds the index.
    pub fn rebuild(&mut self) -> Result<(), ClientError> {
        self.unit(Op::Rebuild)
    }

    /// Rescans folder indexes.
    pub fn update(&mut self) -> Result<(), ClientError> {
        self.unit(Op::Update)
    }

    /// Saves the database.
    pub fn save_db(&mut self) -> Result<(), ClientError> {
        self.unit(Op::SaveDb)
    }

    /// Saves the run history.
    pub fn save_run_history(&mut self) -> Result<(), ClientError> {
        self.unit(Op::SaveRunHistory)
    }

    /// Exits the application.
    pub fn exit(&mut self) -> Result<(), ClientError> {
        self.unit(Op::Exit)
    }

    /// Run count of `path`.
    pub fn run_count(&mut self, path: &str) -> Result<u32, ClientError> {
        match self.call(Op::GetRunCount { path: path.to_string() })? {
            Payload::RunCount(n) => Ok(n),
            p => Self::unexpected(p),
        }
    }

    /// Increments the run count of `path`.
    pub fn inc_run_count(&mut self, path: &str) -> Result<u32, ClientError> {
        match self.call(Op::IncRunCount { path: path.to_string() })? {
            Payload::RunCount(n) => Ok(n),
            p => Self::unexpected(p),
        }
    }

    /// Sets the run count of `path`.
    pub fn set_run_count(&mut self, path: &str, count: u32) -> Result<(), ClientError> {
        self.unit(Op::SetRunCount { path: path.to_string(), count })
    }

    /// Volumes.
    pub fn volumes(&mut self) -> Result<Vec<VolumeEntry>, ClientError> {
        match self.call(Op::ListVolumes)? {
            Payload::Volumes(v) => Ok(v),
            p => Self::unexpected(p),
        }
    }

    /// Filters.
    pub fn filters(&mut self) -> Result<Vec<FilterEntry>, ClientError> {
        match self.call(Op::ListFilters)? {
            Payload::Filters(v) => Ok(v),
            p => Self::unexpected(p),
        }
    }

    /// Bookmarks.
    pub fn bookmarks(&mut self) -> Result<Vec<BookmarkEntry>, ClientError> {
        match self.call(Op::ListBookmarks)? {
            Payload::Bookmarks(v) => Ok(v),
            p => Self::unexpected(p),
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // Closing the channel ends the worker once its current exchange is done. Do not
        // join: after a timeout the worker may still be blocked on the pipe.
        self.tx.take();
        if let Some(w) = self.worker.take()
            && w.is_finished()
        {
            let _ = w.join();
        }
    }
}

fn append(a: &mut proto::ResultColumns, b: proto::ResultColumns) {
    fn ext<T>(x: &mut Option<Vec<T>>, y: Option<Vec<T>>) {
        if let (Some(x), Some(y)) = (x, y) {
            x.extend(y);
        }
    }
    a.flags.extend(b.flags);
    ext(&mut a.name, b.name);
    ext(&mut a.path, b.path);
    ext(&mut a.full_path, b.full_path);
    ext(&mut a.extension, b.extension);
    ext(&mut a.type_name, b.type_name);
    ext(&mut a.size, b.size);
    ext(&mut a.date_modified, b.date_modified);
    ext(&mut a.date_created, b.date_created);
    ext(&mut a.date_accessed, b.date_accessed);
    ext(&mut a.attributes, b.attributes);
    ext(&mut a.run_count, b.run_count);
    ext(&mut a.date_run, b.date_run);
    ext(&mut a.date_recently_changed, b.date_recently_changed);
    ext(&mut a.file_list_name, b.file_list_name);
    ext(&mut a.highlighted_name, b.highlighted_name);
    ext(&mut a.highlighted_path, b.highlighted_path);
    ext(&mut a.highlighted_full_path, b.highlighted_full_path);
}

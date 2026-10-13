//! Fennec HTTP server (SPEC ch.4 §4): a built-in search page, Everything-compatible JSON,
//! file downloads resolved through the index, and status endpoints.
//!
//! ```text
//! pub fn start(config: HttpConfig, backend: Arc<fnc_ipc::IndexBackend>) -> Result<HttpHandle, HttpError>
//! HttpHandle::local_addr() / HttpHandle::stop()   // dropping the handle also stops the server
//! ```
//!
//! Threads: `fennec-http` (owns a Tokio runtime) with two `fennec-http-worker` threads;
//! searches run on Tokio's blocking pool (at most `max_concurrency` at a time).
//!
//! Endpoints (GET/HEAD only):
//!
//! | path | response |
//! |---|---|
//! | `/` | search page; `?search=…&json=1` Everything JSON (see [`render::json`]) |
//! | `/api/v1/status` | `{"version","db_loaded","busy","entries","folders","volumes","generation"}` |
//! | `/api/v1/volumes` | `[{"root_path","display_name","kind","fs_name","online","included"}]` |
//! | `/<percent-encoded full path>` | file download when `allow_downloads`; the path is looked up in the index (never on disk), must be a file of a local volume |
//! | other | files of `static_root` (sanitised, inside the root) or 404 |
//!
//! Protections: HTTP Basic authentication (required; constant-time comparison), fixed
//! `Server: Fennec` header, `nosniff`/`DENY`/no-referrer headers and a strict CSP on the
//! page, request time limit (default 5 s, also cancels the query), concurrency limit
//! (503 when exceeded), per-request result cap, search length/term/wildcard limits, URI
//! length limit, no directory listings (folders link to a search instead).

#![forbid(unsafe_code)]

pub mod params;
pub mod render;

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use fnc_index::VolumeKind;
use fnc_ipc::backend::{IndexBackend, SearchKey, sort_spec};
use fnc_ipc::proto::{Column, Status};
use fnc_ipc::rows::resolve_path;
use tokio::io::AsyncReadExt;
use tokio::sync::{Semaphore, watch};

use crate::params::{SearchParams, base64_decode, check_complexity, ct_eq, json_escape};

/// Columns shown by the built-in page (name is always shown).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpColumns {
    /// Path.
    pub path: bool,
    /// Size.
    pub size: bool,
    /// Date modified.
    pub date_modified: bool,
    /// Date created.
    pub date_created: bool,
    /// Attributes.
    pub attributes: bool,
}

impl Default for HttpColumns {
    fn default() -> Self {
        Self { path: true, size: true, date_modified: true, date_created: false, attributes: false }
    }
}

/// UI strings of the built-in page (the app fills them from its translations).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpLabels {
    /// Search button / box label.
    pub search: String,
    /// Result count line; `{count}` is replaced.
    pub results: String,
    /// Previous page link.
    pub previous: String,
    /// Next page link.
    pub next: String,
    /// Column headers.
    pub name: String,
    /// Path header.
    pub path: String,
    /// Size header.
    pub size: String,
    /// Date modified header.
    pub date_modified: String,
    /// Date created header.
    pub date_created: String,
    /// Attributes header.
    pub attributes: String,
}

impl Default for HttpLabels {
    fn default() -> Self {
        Self {
            search: "Search".into(),
            results: "{count} results".into(),
            previous: "Previous".into(),
            next: "Next".into(),
            name: "Name".into(),
            path: "Path".into(),
            size: "Size".into(),
            date_modified: "Date Modified".into(),
            date_created: "Date Created".into(),
            attributes: "Attributes".into(),
        }
    }
}

impl HttpLabels {
    /// Header text of `c`.
    pub fn column(&self, c: Column) -> &str {
        match c {
            Column::Path => &self.path,
            Column::Size => &self.size,
            Column::DateModified => &self.date_modified,
            Column::DateCreated => &self.date_created,
            Column::Attributes => &self.attributes,
            _ => &self.name,
        }
    }
}

/// Server settings (Options → HTTP Server).
#[derive(Clone, Debug, PartialEq)]
pub struct HttpConfig {
    /// Bind address (default 127.0.0.1).
    pub bind: IpAddr,
    /// Port (default 8080; 0 = any free port).
    pub port: u16,
    /// User name for Basic authentication.
    pub username: String,
    /// Password, or its hash from `fnc_core::secret::hash` (settings store the hash);
    /// required (the server refuses to start without one).
    pub password: String,
    /// Allow file downloads (Everything default: on).
    pub allow_downloads: bool,
    /// Page columns.
    pub columns: HttpColumns,
    /// Results per page of the built-in page (Everything: 32).
    pub page_size: u64,
    /// Serve this directory instead of the built-in page for `/` (index.html) and other
    /// non-search paths.
    pub static_root: Option<PathBuf>,
    /// Most results per request (JSON `count` defaults to all, capped here).
    pub max_results: u64,
    /// Longest accepted search (bytes).
    pub max_search_len: usize,
    /// Most terms per search.
    pub max_terms: usize,
    /// Most requests processed at once.
    pub max_concurrency: usize,
    /// Time limit per request (default 5 s).
    pub request_timeout: Duration,
    /// Log every request (`fennec_http::access` target, info level).
    pub access_log: bool,
    /// Page strings.
    pub labels: HttpLabels,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 8080,
            username: String::new(),
            password: String::new(),
            allow_downloads: true,
            columns: HttpColumns::default(),
            page_size: 32,
            static_root: None,
            max_results: 10_000,
            max_search_len: 1024,
            max_terms: 32,
            max_concurrency: 16,
            request_timeout: Duration::from_secs(5),
            access_log: false,
            labels: HttpLabels::default(),
        }
    }
}

/// Start failures.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// No password configured.
    #[error("the HTTP server needs a password")]
    NoPassword,
    /// The address could not be bound.
    #[error("cannot listen on {addr}: {source}")]
    Bind {
        /// Address.
        addr: SocketAddr,
        /// Cause.
        source: io::Error,
    },
    /// Runtime/thread creation failed.
    #[error("cannot start the HTTP server: {0}")]
    Runtime(io::Error),
}

struct AppState {
    config: HttpConfig,
    backend: Arc<IndexBackend>,
    permits: Arc<Semaphore>,
    /// Credentials verified so far (`user:password` → ok), so PBKDF2 runs once per
    /// client, not per request; bounded.
    verified: parking_lot::Mutex<std::collections::HashMap<Vec<u8>, bool>>,
}

/// A running server; dropping it stops the server.
pub struct HttpHandle {
    addr: SocketAddr,
    stop: Option<watch::Sender<bool>>,
    thread: Option<JoinHandle<()>>,
}

impl HttpHandle {
    /// The bound address (useful with port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stops accepting, lets running requests finish for up to 2 s, then shuts down.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(true);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for HttpHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Starts the server. Binds synchronously, so address errors are reported here.
pub fn start(config: HttpConfig, backend: Arc<IndexBackend>) -> Result<HttpHandle, HttpError> {
    if config.password.is_empty() {
        return Err(HttpError::NoPassword);
    }
    let want = SocketAddr::new(config.bind, config.port);
    let std_listener = std::net::TcpListener::bind(want).map_err(|source| HttpError::Bind { addr: want, source })?;
    std_listener.set_nonblocking(true).map_err(HttpError::Runtime)?;
    let addr = std_listener.local_addr().map_err(HttpError::Runtime)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(config.max_concurrency.max(1) + 2)
        .thread_name("fennec-http-worker")
        .enable_all()
        .build()
        .map_err(HttpError::Runtime)?;
    let state = Arc::new(AppState {
        permits: Arc::new(Semaphore::new(config.max_concurrency.max(1))),
        config,
        backend,
        verified: parking_lot::Mutex::new(std::collections::HashMap::new()),
    });
    let (tx, mut rx) = watch::channel(false);
    let mut rx2 = rx.clone();
    let thread = std::thread::Builder::new()
        .name("fennec-http".into())
        .spawn(move || {
            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::from_std(std_listener) {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::error!(error = %e, "HTTP listener");
                        return;
                    }
                };
                tracing::info!(%addr, "HTTP server listening");
                let app = router(state);
                let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
                    let _ = rx.changed().await;
                });
                let deadline = async move {
                    let _ = rx2.changed().await;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                };
                tokio::select! {
                    r = serve => if let Err(e) = r { tracing::warn!(error = %e, "HTTP server stopped") },
                    () = deadline => {}
                }
            });
            runtime.shutdown_timeout(Duration::from_millis(500));
            tracing::info!("HTTP server stopped");
        })
        .map_err(HttpError::Runtime)?;
    Ok(HttpHandle { addr, stop: Some(tx), thread: Some(thread) })
}

fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/v1/status", get(status))
        .route("/api/v1/volumes", get(volumes))
        .fallback(fallback)
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

fn text(status: StatusCode, msg: &str) -> Response {
    (status, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], format!("{msg}\n")).into_response()
}

const MAX_URI: usize = 8 * 1024;

fn authorized(headers: &HeaderMap, st: &AppState) -> bool {
    let Some(v) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) else { return false };
    let Some((scheme, cred)) = v.trim().split_once(' ') else { return false };
    if !scheme.eq_ignore_ascii_case("basic") {
        return false;
    }
    let Some(got) = base64_decode(cred) else { return false };
    if let Some(&ok) = st.verified.lock().get(&got) {
        return ok;
    }
    let ok = match got.iter().position(|&b| b == b':') {
        Some(i) => {
            let (user, pass) = (&got[..i], &got[i + 1..]);
            ct_eq(user, st.config.username.as_bytes())
                && std::str::from_utf8(pass).is_ok_and(|p| fnc_core::secret::verify(p, &st.config.password))
        }
        None => false,
    };
    let mut cache = st.verified.lock();
    // Bounded: a flood of wrong passwords cannot grow memory (nor skip the hashing).
    if cache.len() < 64 {
        cache.insert(got, ok);
    }
    ok
}

/// Auth, limits, timeout and fixed headers around every request.
async fn guard(State(st): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let t0 = Instant::now();
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let mut resp = if req.uri().to_string().len() > MAX_URI {
        text(StatusCode::URI_TOO_LONG, "URI too long")
    } else if !authorized(req.headers(), &st) {
        let mut r = text(StatusCode::UNAUTHORIZED, "authentication required");
        r.headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"Fennec\", charset=\"UTF-8\""));
        r
    } else if method != Method::GET && method != Method::HEAD {
        let mut r = text(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
        r.headers_mut().insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        r
    } else {
        match st.permits.clone().try_acquire_owned() {
            Err(_) => text(StatusCode::SERVICE_UNAVAILABLE, "server busy"),
            Ok(_permit) => match tokio::time::timeout(st.config.request_timeout, next.run(req)).await {
                Ok(r) => r,
                Err(_) => text(StatusCode::SERVICE_UNAVAILABLE, "request timed out"),
            },
        }
    };
    let h = resp.headers_mut();
    h.insert(header::SERVER, HeaderValue::from_static("Fennec"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    if !h.contains_key(header::CACHE_CONTROL) {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    if st.config.access_log {
        tracing::info!(target: "fennec_http::access", %method, %path, status = resp.status().as_u16(),
            ms = t0.elapsed().as_millis() as u64, "request");
    }
    resp
}

async fn status(State(st): State<Arc<AppState>>) -> Response {
    let b = st.backend.clone();
    let s = match tokio::task::spawn_blocking(move || b.index_status()).await {
        Ok(s) => s,
        Err(_) => return text(StatusCode::INTERNAL_SERVER_ERROR, "status failed"),
    };
    let body = format!(
        "{{\"version\":\"{}\",\"db_loaded\":{},\"busy\":{},\"entries\":{},\"folders\":{},\"volumes\":{},\"generation\":{}}}\n",
        env!("CARGO_PKG_VERSION"),
        s.db_loaded,
        s.busy,
        s.entries,
        s.folders,
        s.volumes,
        s.generation
    );
    json_response(body)
}

async fn volumes(State(st): State<Arc<AppState>>) -> Response {
    let b = st.backend.clone();
    let vols = match tokio::task::spawn_blocking(move || b.volumes()).await {
        Ok(v) => v,
        Err(_) => return text(StatusCode::INTERNAL_SERVER_ERROR, "volumes failed"),
    };
    let items: Vec<String> = vols
        .iter()
        .map(|v| {
            format!(
                "{{\"root_path\":\"{}\",\"display_name\":\"{}\",\"kind\":\"{}\",\"fs_name\":\"{}\",\"online\":{},\"included\":{}}}",
                json_escape(&v.root_path),
                json_escape(&v.display_name),
                json_escape(&v.kind),
                json_escape(&v.fs_name),
                v.online,
                v.included
            )
        })
        .collect();
    json_response(format!("[{}]\n", items.join(",")))
}

fn json_response(body: String) -> Response {
    ([(header::CONTENT_TYPE, "application/json; charset=utf-8")], body).into_response()
}

fn html_response(status: StatusCode, body: String) -> Response {
    let mut r = (status, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response();
    r.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
        ),
    );
    r
}

async fn fallback(State(st): State<Arc<AppState>>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    if path == "/" {
        if query.is_empty()
            && let Some(root) = &st.config.static_root
            && let Some(r) = serve_static(root, "/index.html").await
        {
            return r;
        }
        return search(st, &query).await;
    }
    if path.starts_with("/api/") {
        return text(StatusCode::NOT_FOUND, "not found");
    }
    if let Some(root) = &st.config.static_root
        && let Some(r) = serve_static(root, &path).await
    {
        return r;
    }
    download(st, &path).await
}

async fn search(st: Arc<AppState>, query: &str) -> Response {
    let p = match SearchParams::parse(query) {
        Ok(p) => p,
        Err(e) => return text(StatusCode::BAD_REQUEST, &e),
    };
    let cfg = &st.config;
    let page_cols = render::html_columns(&cfg.columns);
    let count = p.count.unwrap_or(if p.json { u64::MAX } else { cfg.page_size }).min(cfg.max_results);
    if let Err(e) = check_complexity(&p.search, cfg.max_search_len, cfg.max_terms) {
        return if p.json {
            text(StatusCode::BAD_REQUEST, &e)
        } else {
            html_response(StatusCode::BAD_REQUEST, render::html(&p, None, &page_cols, &cfg.labels, count, Some(&e)))
        };
    }
    if !p.json && p.search.is_empty() {
        // Empty page (Everything shows all files; Fennec waits for a search).
        return html_response(StatusCode::OK, render::html(&p, None, &page_cols, &cfg.labels, count, None));
    }
    let mut cols = vec![Column::Name, Column::Path, Column::FullPath];
    cols.extend([Column::Size, Column::DateModified, Column::DateCreated, Column::Attributes]);
    let backend = st.backend.clone();
    let key = SearchKey {
        search: p.search.clone(),
        mods: p.mods,
        sort: sort_spec(&[(p.sort, p.ascending)], backend.host().folders_first()),
    };
    let timeout = cfg.request_timeout;
    let offset = p.offset;
    let res = tokio::task::spawn_blocking(move || {
        let rs = backend.search(&key, timeout)?;
        Ok::<_, fnc_ipc::IpcError>(backend.page(&rs, 0, offset, count, &cols, false))
    })
    .await;
    let results = match res {
        Ok(Ok(r)) => r,
        Ok(Err(e)) if e.status == Status::Timeout => return text(StatusCode::SERVICE_UNAVAILABLE, "search timed out"),
        Ok(Err(e)) => return text(StatusCode::INTERNAL_SERVER_ERROR, &e.message),
        Err(_) => return text(StatusCode::INTERNAL_SERVER_ERROR, "search failed"),
    };
    if p.json {
        json_response(render::json(&p, &results))
    } else {
        html_response(StatusCode::OK, render::html(&p, Some(&results), &page_cols, &cfg.labels, count, None))
    }
}

fn decode_path(path: &str) -> Option<String> {
    let s = percent_encoding::percent_decode_str(path).decode_utf8().ok()?;
    if s.contains('\0') { None } else { Some(s.into_owned()) }
}

/// Finds the indexed file for a download URL (`/C:/dir/file`, `//home/u/file`).
fn lookup(backend: &IndexBackend, path: &str) -> Option<String> {
    let decoded = decode_path(path)?;
    let rel = decoded.strip_prefix('/').unwrap_or(&decoded);
    let snap = backend.index().snapshot();
    let id = resolve_path(&snap, rel).or_else(|| resolve_path(&snap, &decoded))?;
    if snap.is_dir(id) {
        return None;
    }
    let vol = snap.volume_info(snap.volume_of(id))?;
    if vol.info.kind == VolumeKind::FileList || !vol.online {
        return None;
    }
    let mut full = String::new();
    snap.full_path(id, &mut full);
    Some(full)
}

fn content_disposition(name: &str) -> String {
    let ascii: String =
        name.chars().map(|c| if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' { c } else { '_' }).collect();
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{}", params::encode_component(name))
}

fn file_stream(file: tokio::fs::File) -> Body {
    let stream = futures_util::stream::unfold(file, |mut f| async move {
        let mut buf = vec![0u8; 64 * 1024];
        match f.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<Bytes, io::Error>(Bytes::from(buf)), f))
            }
            Err(e) => Some((Err(e), f)),
        }
    });
    Body::from_stream(stream)
}

async fn download(st: Arc<AppState>, path: &str) -> Response {
    if !st.config.allow_downloads {
        return text(StatusCode::FORBIDDEN, "downloads are disabled");
    }
    let backend = st.backend.clone();
    let p = path.to_string();
    let full = match tokio::task::spawn_blocking(move || lookup(&backend, &p)).await {
        Ok(Some(f)) => f,
        _ => return text(StatusCode::NOT_FOUND, "not found"),
    };
    let file = match tokio::fs::File::open(&full).await {
        Ok(f) => f,
        Err(_) => return text(StatusCode::NOT_FOUND, "not found"),
    };
    let meta = match file.metadata().await {
        Ok(m) if m.is_file() => m,
        _ => return text(StatusCode::NOT_FOUND, "not found"),
    };
    let name = full.rsplit(['\\', '/']).next().unwrap_or("download");
    let mut r = Response::new(file_stream(file));
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    if let Ok(v) = HeaderValue::from_str(&meta.len().to_string()) {
        h.insert(header::CONTENT_LENGTH, v);
    }
    if let Ok(v) = HeaderValue::from_str(&content_disposition(name)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-cache"));
    r
}

fn static_type(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("txt") => "text/plain; charset=utf-8",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}

/// Serves `url_path` from `root` if it names a regular file inside it.
async fn serve_static(root: &Path, url_path: &str) -> Option<Response> {
    let decoded = decode_path(url_path)?;
    let mut rel = PathBuf::new();
    for part in decoded.split('/').filter(|p| !p.is_empty()) {
        if part.contains(['\\', ':']) {
            return None;
        }
        match Path::new(part).components().next() {
            Some(Component::Normal(c)) if Path::new(part).components().count() == 1 => rel.push(c),
            _ => return None,
        }
    }
    let root = tokio::fs::canonicalize(root).await.ok()?;
    let target = tokio::fs::canonicalize(root.join(&rel)).await.ok()?;
    if !target.starts_with(&root) {
        return None;
    }
    let meta = tokio::fs::metadata(&target).await.ok()?;
    if !meta.is_file() || meta.len() > 64 << 20 {
        return None;
    }
    let bytes = tokio::fs::read(&target).await.ok()?;
    Some(([(header::CONTENT_TYPE, static_type(&target))], bytes).into_response())
}

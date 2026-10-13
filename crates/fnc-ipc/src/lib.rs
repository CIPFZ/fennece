//! Fennec IPC: the native local-socket protocol, its server and client, the Everything 1.4
//! `WM_COPYDATA` compatibility layer and result export (SPEC ch.4 §3, §7).
//!
//! # Integration contract (for the application)
//!
//! ```text
//! // 1. Backend: queries run on index snapshots, independent of the UI's SearchEngine.
//! pub trait Host: Send + Sync {                       // every method has a default
//!     fn search_options(&self) -> fnc_exec::SearchOptions;          // precedence, macros, type names…
//!     fn folders_first(&self) -> fnc_core::settings::FoldersFirst;
//!     fn is_db_loaded(&self) -> bool;  fn is_busy(&self) -> bool;
//!     fn is_admin(&self) -> bool;      fn is_appdata(&self) -> bool;
//!     fn status_text(&self) -> String;
//!     fn authorize(&self, cmd: AdminCommand, peer: &Peer) -> bool;    // default: allow
//!     fn command(&self, cmd: AdminCommand) -> Result<(), HostError>;  // Rebuild/Update/SaveDb/
//!                                                     // SaveRunHistory/DeleteRunHistory/Exit
//!     fn run_count_changed(&self);                     // after IPC changed a run count
//!     fn filters(&self) -> Vec<FilterEntry>;  fn bookmarks(&self) -> Vec<BookmarkEntry>;
//!     fn command_line(&self, show_command: u32, command_line: &str) -> bool;
//!     fn everything_version(&self) -> [u32; 4];        // default 1.4.1.<fennec build>
//! }
//! pub fn IndexBackend::new(shared: fnc_index::SharedIndex, host: Arc<dyn Host>) -> IndexBackend
//! pub fn IndexBackend::with_config(shared, host, config: BackendConfig) -> IndexBackend
//!
//! // 2. Native protocol server (`\\.\pipe\Fennec.<instance>.ipc`):
//! pub fn server::spawn(instance: &str, backend: Arc<IndexBackend>, config: ServerConfig)
//!     -> io::Result<ServerHandle>                       // thread "fennec-ipc"; drop = stop
//! pub fn server::serve(instance: &str, backend: Arc<IndexBackend>, stop: &StopSignal,
//!     config: &ServerConfig) -> io::Result<()>          // blocking variant; stop.stop() + server::wake(instance)
//!
//! // 3. Everything 1.4 compatibility (hidden window `compat::window_class(instance)`):
//! pub fn compat::handle_wm_ipc(wparam: usize, lparam: isize, backend: &IndexBackend) -> isize
//! pub fn compat::decode_copydata(dw_data: usize, bytes: &[u8]) -> Result<CopyDataRequest, CompatError>
//! pub fn compat::execute_copydata(req: &CopyDataRequest, backend: &IndexBackend) -> CopyDataOutcome
//! pub fn compat::handle_copydata(dw_data: usize, bytes: &[u8], backend: &IndexBackend)
//!     -> Option<CopyDataOutcome>                        // decode + execute in one call
//! // CopyDataOutcome { result: isize /* return from WndProc */,
//! //                   reply: Option<CopyDataReply { hwnd: isize, copydata_id: usize, bytes: Vec<u8> }> }
//!
//! // 4. Export (File → Export, fes, HTTP):
//! pub fn export::export_entries<W: Write>(out: W, opts: &ExportOptions, store: &IndexStore,
//!     ids: &[EntryId], caches: &Caches) -> io::Result<u64>
//! pub fn export::export_results<W: Write>(out: W, opts: &ExportOptions, results: &Results) -> io::Result<u64>
//! pub struct export::Exporter<W>   // streaming: new(out, opts)?, write_row(&Row)?, finish()?
//! ```
//!
//! ## Threads
//!
//! * [`server::spawn`] starts `fennec-ipc` (accept loop) and one `fennec-ipc-conn` thread per
//!   client (at most [`server::ServerConfig::max_connections`], default 32).
//! * Every query with a time limit starts a short-lived `fennec-ipc-watchdog` thread that
//!   cancels the query at its deadline. Queries use the global rayon pool like the UI.
//! * [`client::Client`] runs one `fennec-ipc-client` thread per connection.
//! * The compatibility functions spawn nothing; they run on the caller's thread.
//!
//! ## Win32 plumbing expected from the app (sketch)
//!
//! Working reference: `examples/compat_window.rs` (verified with official es.exe 1.1.0.38 and
//! Everything64.dll, ADR-031).
//!
//! ```text
//! RegisterClassW(compat::window_class(instance));
//! CreateWindowExW(0, class, class, WS_OVERLAPPED, 0, 0, 0, 0, None, ..) // top-level, never shown:
//!   // FindWindow() does not see HWND_MESSAGE (message-only) windows.
//! WndProc:
//!   EVERYTHING_WM_IPC (WM_USER) => return compat::handle_wm_ipc(wParam, lParam, &backend)
//!   WM_COPYDATA => {
//!       let cds = &*(lParam as *const COPYDATASTRUCT);
//!       let bytes = slice(cds.lpData, cds.cbData).to_vec();      // copy before returning
//!       match compat::decode_copydata(cds.dwData, &bytes) {
//!           Err(_) => return FALSE,
//!           Ok(req @ (Query | Query2)) => { worker: let out = execute_copydata(&req, &backend);
//!                 if let Some(r) = out.reply { SendMessageTimeoutW(r.hwnd, WM_COPYDATA, our_hwnd,
//!                     &COPYDATASTRUCT { dwData: r.copydata_id, cbData: r.bytes.len(), lpData: r.bytes }) }
//!                 return TRUE }
//!           Ok(other) => return execute_copydata(&other, &backend).result,  // run counts: take the
//!                                                     // index write lock briefly (a worker is safer)
//!       }
//!   }
//! ChangeWindowMessageFilterEx(hwnd, WM_COPYDATA / WM_USER, MSGFLT_ALLOW) so non-elevated
//! clients can talk to an elevated Fennec. Do not register the class while real Everything
//! (`FindWindowW("EVERYTHING_TASKBAR_NOTIFICATION")` from another process) is running.
//! ```
//!
//! Searches from every front end may contain the Everything 1.5 `count:<max>` limit (es 1.1.0.38
//! sends `-n` that way); [`backend::extract_count_limit`] applies it after sorting.
//!
//! Modules: [`proto`] (wire types, frames), [`backend`] (execution, [`Host`]), [`server`],
//! [`client`], [`compat`] (Everything 1.4), [`export`] and [`mod@format`] (es formats), [`rows`].

#![forbid(unsafe_code)]

pub mod backend;
pub mod client;
pub mod compat;
pub mod export;
pub mod format;
pub mod proto;
pub mod rows;
pub mod server;

pub use backend::{AdminCommand, BackendConfig, DefaultHost, Host, HostError, IndexBackend, IpcError, Peer, PeerKind};
pub use client::{Client, ClientError};
pub use compat::{CopyDataOutcome, CopyDataReply, CopyDataRequest, handle_copydata, handle_wm_ipc};
pub use export::{ExportOptions, Exporter, Format, export_entries, export_results};
pub use proto::{Column, Mods, Op, Payload, Query, Results, SearchSpec, Status};
pub use server::{ServerConfig, ServerHandle, StopSignal, serve, spawn};

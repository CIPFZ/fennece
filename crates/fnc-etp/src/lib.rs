//! Fennec ETP/FTP server and client (SPEC ch.4 §5, ADR-036).
//!
//! ETP is FTP plus Everything's `SITE EVERYTHING` extension: a client sets search options
//! (`SITE EVERYTHING SEARCH abc`, `… CASE 1`, `… COUNT 26`, `… PATH_COLUMN 1`, …) and runs
//! `SITE EVERYTHING QUERY`, which answers with one multi-line reply:
//!
//! ```text
//! 200-Query results
//!  RESULT_COUNT 2
//!  PATH C:\Data
//!  SIZE 1234
//!  DATE_MODIFIED 133000000000000000
//!  FILE alpha.txt
//!  …
//! 200 End.
//! ```
//!
//! Everything 1.4/1.5 clients send the same sub-commands without `SITE`
//! (`EVERYTHING QUERY`); both forms are accepted. Plain FTP clients browse the index
//! (`LIST`/`MLSD` of `/C:/…`) and download files (`RETR`).
//!
//! ```text
//! pub fn server::start(config: EtpConfig, backend: Arc<fnc_ipc::IndexBackend>) -> Result<EtpHandle, EtpError>
//! pub fn client::EtpClient::connect(target: &Target, timeout: Duration) -> Result<EtpClient, ClientError>
//! ```
//!
//! Threads: `fennec-etp` accepts; one `fennec-etp-session` per connection. Nothing runs
//! while the server is stopped.

#![forbid(unsafe_code)]

pub mod client;
pub mod namespace;
pub mod proto;
pub mod server;

pub use client::{ClientError, EtpClient, Target};
pub use proto::{QueryPage, ResultFields, ResultItem, SearchState};
pub use server::{EtpConfig, EtpError, EtpHandle, start};

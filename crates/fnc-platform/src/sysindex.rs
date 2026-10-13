//! [`fnc_exec::SystemIndexSource`] for `si:` over [`crate::winsearch`] (feature `sysindex`).

use std::time::Duration;

use fnc_exec::{SystemIndexError, SystemIndexHits, SystemIndexSource};

/// `si:` through the Windows Search system index of the user running Fennec
/// ([`crate::winsearch`]); unavailable on other platforms and without the service.
#[derive(Clone, Debug)]
pub struct WinSearch {
    /// At most this many items per query (the rest is dropped with a warning).
    pub max_results: usize,
    /// A query that takes longer is abandoned with a warning.
    pub timeout: Duration,
}

impl Default for WinSearch {
    fn default() -> Self {
        Self { max_results: 100_000, timeout: Duration::from_secs(20) }
    }
}

impl SystemIndexSource for WinSearch {
    fn search(&self, aqs: &str, cancel: &dyn Fn() -> bool) -> Result<SystemIndexHits, SystemIndexError> {
        use crate::winsearch::{Error, search};
        match search(aqs, self.max_results, self.timeout, cancel) {
            Ok(h) => Ok(SystemIndexHits { limit: h.truncated.then_some(self.max_results), paths: h.paths }),
            Err(Error::Unavailable(why)) => {
                tracing::info!(%why, "Windows Search system index unavailable");
                Err(SystemIndexError::Unavailable)
            }
            Err(Error::Cancelled) => Err(SystemIndexError::Cancelled),
            Err(Error::Failed(why)) => {
                tracing::warn!(%why, query = aqs, "Windows Search query failed");
                Err(SystemIndexError::Failed(why))
            }
            Err(e @ Error::TimedOut(_)) => {
                tracing::warn!(error = %e, query = aqs, "Windows Search query timed out");
                Err(SystemIndexError::Failed(e.to_string()))
            }
        }
    }
}

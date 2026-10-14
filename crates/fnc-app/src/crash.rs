//! Crash reports of the window process (REL-02): installs the generic hook of
//! [`fnc_core::crash`] with the app's index-state summary, and the status-bar item
//! "Fennec did not exit normally last time — view log" of the next start.
//!
//! Decision (caught panics): every panic gets a report, also the ones a local
//! `catch_unwind` contains (search engine, property parsers, IPC handlers); when the
//! process ends normally its reports are renamed `*.caught.log`
//! ([`fnc_core::crash::mark_survived`]). Only reports of processes that died — any
//! binary writing into this logs folder: `fennec`, `fes`, the IFilter host — make the
//! prompt appear. Clicking it opens the folder (the newest report selected) and
//! acknowledges; right-click dismisses without opening. Either way it does not come back
//! for the same reports.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use fnc_core::crash;
use fnc_index::SharedIndex;

use crate::i18n::{t, t_args};
use crate::indexer::IndexStatus;

/// Indexer phase for the report (updated from the UI thread, read with `try_lock`).
static PHASE: Mutex<String> = Mutex::new(String::new());
/// `-debug-panic`: panic on the UI thread once the window runs.
static DEBUG_PANIC: AtomicBool = AtomicBool::new(false);
/// Longest wait for a ready index before the `-debug-panic` panic.
const DEBUG_PANIC_WAIT: Duration = Duration::from_secs(20);

/// Installs the hook for `fennec` writing into `logs` (also the folder handed to the
/// IFilter hosts, see `crate::ifilter::provider`).
pub fn install(logs: &Path, redact_queries: bool) {
    crash::install("fennec", logs.to_path_buf(), fnc_platform::os::version());
    crash::set_redact_queries(redact_queries);
    fnc_platform::os::install_exception_filter();
}

/// Registers the index summary of the reports: entries, volumes, generation and the
/// indexer phase. Never blocks (the index lock is only tried).
pub fn register_state(shared: SharedIndex) {
    crash::set_state_provider(std::sync::Arc::new(move || {
        let phase = match PHASE.try_lock() {
            Ok(p) if !p.is_empty() => p.clone(),
            Ok(_) => "Starting".to_string(),
            Err(_) => "(unavailable)".to_string(),
        };
        let index = match shared.try_snapshot_for(Duration::ZERO) {
            Some(s) => {
                let vols: Vec<String> = s
                    .volumes()
                    .iter()
                    .filter(|v| !v.is_retired())
                    .map(|v| {
                        format!(
                            "{} [{}{}]",
                            v.info.display_name,
                            v.info.fs_name,
                            if v.online { "" } else { ", offline" }
                        )
                    })
                    .collect();
                format!(
                    "entries: {} live / {} rows\nvolumes: {} ({})\ngeneration: {}  instance: {}",
                    s.live_count(),
                    s.len(),
                    vols.len(),
                    vols.join(", "),
                    s.generation(),
                    s.instance()
                )
            }
            None => "index: (locked by a writer)".to_string(),
        };
        format!("{index}\nindexer: {phase}")
    }));
}

/// Remembers the indexer phase for the report.
pub fn set_phase(s: &IndexStatus) {
    if let Ok(mut p) = PHASE.try_lock() {
        *p = format!("{s:?}");
    }
}

/// `-debug-panic` (hidden): arms the deliberate UI-thread panic.
pub fn arm_debug_panic() {
    DEBUG_PANIC.store(true, Ordering::Relaxed);
}

/// Called every frame: with `-debug-panic`, panics once the index is ready (or after
/// [`DEBUG_PANIC_WAIT`]), so the report has searches and an index summary.
pub fn debug_panic_tick(ctx: &egui::Context, ready: bool) {
    static ARMED_AT: Mutex<Option<Instant>> = Mutex::new(None);
    if !DEBUG_PANIC.load(Ordering::Relaxed) {
        return;
    }
    let Ok(mut at) = ARMED_AT.lock() else { return };
    let since = *at.get_or_insert_with(Instant::now);
    drop(at);
    if ready || since.elapsed() >= DEBUG_PANIC_WAIT {
        DEBUG_PANIC.store(false, Ordering::Relaxed);
        panic!("-debug-panic: deliberate panic on the UI thread (crash report test)");
    }
    ctx.request_repaint_after(Duration::from_millis(200));
}

/// The "did not exit normally" status-bar item.
#[derive(Debug, Default)]
pub struct CrashPrompt {
    dir: Option<PathBuf>,
    files: Vec<PathBuf>,
}

impl CrashPrompt {
    /// Reports of earlier runs in the folder of the installed hook (none in tests).
    pub fn detect() -> Self {
        crash::dir().map(|d| Self::for_dir(&d)).unwrap_or_default()
    }

    /// Unacknowledged reports in `dir`.
    pub fn for_dir(dir: &Path) -> Self {
        Self { files: crash::pending(dir), dir: Some(dir.to_path_buf()) }
    }

    /// `true` while the item shows.
    pub fn visible(&self) -> bool {
        !self.files.is_empty()
    }

    /// Draws the item; returns a message for the status bar on failure.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<String> {
        if !self.visible() {
            return None;
        }
        let (Some(dir), Some(newest)) = (self.dir.clone(), self.files.last().cloned()) else { return None };
        ui.separator();
        let tip = t_args(
            "status-crash-tooltip",
            &[("count", (self.files.len() as i64).into()), ("dir", dir.display().to_string().into())],
        );
        let r = ui
            .add(egui::Link::new(egui::RichText::new(t("status-crash-prompt")).color(ui.visuals().warn_fg_color)))
            .on_hover_text(tip);
        let open = r.clicked();
        if !open && !r.secondary_clicked() {
            return None;
        }
        let mut err = None;
        if open && let Err(e) = fnc_platform::shell::reveal(&newest) {
            tracing::warn!(error = %e, "cannot open the logs folder");
            err = Some(e.to_string());
        }
        if let Err(e) = crash::acknowledge(&dir, &self.files) {
            tracing::warn!(error = %e, "cannot acknowledge the crash reports");
            err = Some(e.to_string());
        }
        self.files.clear();
        err.map(|e| t_args("msg-crash-open-failed", &[("error", e.into())]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_lists_unacknowledged_reports() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!CrashPrompt::for_dir(dir.path()).visible());
        std::fs::write(dir.path().join("crash-20261014-101500-fes.log"), "x").unwrap();
        std::fs::write(dir.path().join("crash-20261014-101400.caught.log"), "x").unwrap();
        let p = CrashPrompt::for_dir(dir.path());
        assert!(p.visible());
        assert_eq!(p.files.len(), 1);
        crash::acknowledge(dir.path(), &p.files).unwrap();
        assert!(!CrashPrompt::for_dir(dir.path()).visible());
        assert!(!CrashPrompt::default().visible());
    }

    #[test]
    fn phase_is_remembered() {
        set_phase(&IndexStatus::Ready { entries: 3, seconds: 0.5 });
        assert!(PHASE.lock().unwrap().starts_with("Ready"));
    }
}

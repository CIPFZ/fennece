//! Sorting by the columns that read files — link target, file signature, recycle bin
//! original location (REL-06, SPEC ch.2 §9 "读文件的列"): "confirm, then read in the
//! background, then sort".
//!
//! A header click (or Ctrl+click) on such a column first counts, on a background thread,
//! the results that have no cached value yet. None: the sort is applied at once (it runs on
//! the search thread like every sort, ranking the values of the persistent
//! `FileValueCache`). Otherwise a dialog asks "N files need to be read (about X s)"; on
//! confirmation [`crate::cells::LazyCells::read_files`] reads them on background threads
//! with progress and a Cancel button in the status bar (Esc and a new search cancel too),
//! and the sort is applied when every file was read. Cancelling leaves the order as it was.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use crossbeam_channel::Receiver;
use fnc_core::EntryId;
use fnc_exec::SortSpec;
use fnc_props::filecache::FileField;

use super::FennecApp;
use super::dialogs::Dialog;
use crate::format;
use crate::i18n::{t, t_args};

/// A file column sort waiting for its values.
pub(super) struct FileSort {
    /// The sort to apply once the values are cached.
    spec: SortSpec,
    fields: Vec<FileField>,
    cancel: Arc<AtomicBool>,
    phase: Phase,
}

enum Phase {
    /// Counting the results without cached values.
    Counting(Receiver<Counted>),
    /// The dialog asks whether to read `ids`.
    Asking { ids: Arc<Vec<EntryId>>, instance: u64 },
    /// Reading on background threads.
    Reading { total: usize, done: Arc<AtomicUsize>, rx: Receiver<bool> },
}

struct Counted {
    ids: Vec<EntryId>,
    instance: u64,
}

impl FileSort {
    /// `(done, total)` while files are read.
    pub(super) fn progress(&self) -> Option<(usize, usize)> {
        match &self.phase {
            Phase::Reading { total, done, .. } => Some((done.load(Ordering::Relaxed).min(*total), *total)),
            _ => None,
        }
    }
}

impl FennecApp {
    /// Applies the sort `spec` (header clicks): at once, or — when it has file-reading
    /// keys whose values are not all cached — after asking and reading them.
    pub(super) fn request_sort(&mut self, spec: SortSpec) {
        let fields: Vec<FileField> = spec.file_fields().collect();
        self.cancel_file_sort();
        if fields.is_empty() || self.v.results.is_empty() {
            self.v.sort = spec;
            self.send_search();
            return;
        }
        // Remote results are never read (an ETP server cannot sort by these columns).
        if self.is_remote() {
            return;
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let (shared, results, stores, stop) =
            (self.view_index().clone(), self.v.results.clone(), self.stores.clone(), cancel.clone());
        let f = fields.clone();
        let ctx = self.ctx.clone();
        let spawned = std::thread::Builder::new().name("fennec-file-sort".into()).spawn(move || {
            let s = shared.snapshot();
            let instance = s.instance();
            // Results of another id space (the index was renumbered): sort with what is cached.
            let ids = if results.instance == instance {
                stores.cells.missing(&s, &results.ids, &f, &stop)
            } else {
                Vec::new()
            };
            drop(s);
            let _ = tx.send(Counted { ids, instance });
            ctx.request_repaint();
        });
        match spawned {
            Ok(_) => self.v.file_sort = Some(FileSort { spec, fields, cancel, phase: Phase::Counting(rx) }),
            Err(e) => tracing::warn!(error = %e, "cannot start the file sort"),
        }
    }

    /// Advances the pending file column sort (every frame).
    pub(super) fn poll_file_sort(&mut self) {
        let Some(job) = &mut self.v.file_sort else { return };
        let ready = match &job.phase {
            Phase::Counting(rx) => match rx.try_recv() {
                Ok(c) if c.ids.is_empty() => true,
                Ok(c) => {
                    let secs = self.stores.cells.estimate(c.ids.len()).as_secs_f64().ceil().max(1.0) as u64;
                    self.dialog = Some(Dialog::ReadFiles { count: c.ids.len(), secs });
                    job.phase = Phase::Asking { ids: Arc::new(c.ids), instance: c.instance };
                    false
                }
                Err(crossbeam_channel::TryRecvError::Empty) => false,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.v.file_sort = None;
                    return;
                }
            },
            Phase::Asking { .. } => {
                // The dialog was closed (Cancel, Esc, its close button).
                if !matches!(self.dialog, Some(Dialog::ReadFiles { .. })) {
                    self.v.file_sort = None;
                }
                false
            }
            Phase::Reading { rx, .. } => {
                self.ctx.request_repaint_after(Duration::from_millis(100));
                match rx.try_recv() {
                    // Read everything, or the index was renumbered meanwhile (sort with
                    // what is cached).
                    Ok(_) => !job.cancel.load(Ordering::Relaxed),
                    Err(crossbeam_channel::TryRecvError::Empty) => false,
                    Err(crossbeam_channel::TryRecvError::Disconnected) => {
                        self.v.file_sort = None;
                        return;
                    }
                }
            }
        };
        if ready && let Some(job) = self.v.file_sort.take() {
            self.v.sort = job.spec;
            self.send_search();
            self.sync_view_settings();
        }
    }

    /// The dialog's Continue: reads the files in the background.
    pub(super) fn confirm_file_read(&mut self) {
        let (shared, stores, ctx) = (self.view_index().clone(), self.stores.clone(), self.ctx.clone());
        let Some(job) = &mut self.v.file_sort else { return };
        let Phase::Asking { ids, instance } = &job.phase else { return };
        let (ids, instance) = (ids.clone(), *instance);
        let total = ids.len();
        let done = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = crossbeam_channel::bounded(1);
        let (cancel, fields) = (job.cancel.clone(), job.fields.clone());
        let d = done.clone();
        let spawned = std::thread::Builder::new().name("fennec-file-read".into()).spawn(move || {
            let t = std::time::Instant::now();
            let ok = stores.cells.read_files(&shared, instance, &ids, &fields, &cancel, &d);
            tracing::info!(files = ids.len(), elapsed = ?t.elapsed(), complete = ok, "files read for sorting");
            let _ = tx.send(ok);
            ctx.request_repaint();
        });
        match spawned {
            Ok(_) => job.phase = Phase::Reading { total, done, rx },
            Err(e) => {
                tracing::warn!(error = %e, "cannot start reading files");
                self.v.file_sort = None;
            }
        }
    }

    /// Cancels a pending file column sort (the order stays as it is); `true` if there was
    /// one.
    pub(super) fn cancel_file_sort(&mut self) -> bool {
        let Some(job) = self.v.file_sort.take() else { return false };
        job.cancel.store(true, Ordering::Relaxed);
        if matches!(self.dialog, Some(Dialog::ReadFiles { .. })) {
            self.dialog = None;
        }
        true
    }

    /// Esc while files are read: cancels the reading (`true` if it did).
    pub(super) fn escape_file_read(&mut self) -> bool {
        self.v.file_sort.as_ref().is_some_and(|j| matches!(j.phase, Phase::Reading { .. })) && self.cancel_file_sort()
    }

    /// Status bar: reading progress and its Cancel button.
    pub(super) fn file_sort_status(&mut self, ui: &mut egui::Ui) {
        let Some((done, total)) = self.v.file_sort.as_ref().and_then(FileSort::progress) else { return };
        ui.separator();
        ui.label(t_args(
            "status-reading-files",
            &[("done", format::group_digits(done as u64).into()), ("total", format::group_digits(total as u64).into())],
        ));
        if ui.small_button(t("dialog-cancel")).clicked() {
            self.cancel_file_sort();
        }
    }
}

//! Semantic search in the window (SPEC ch.5 §3.3): the "semantic" toggle next to the
//! search box (`?sem:` for every search), status-bar text, the setup window (scope,
//! model download) and re-running searches as the semantic index fills.
//!
//! Without the `semantic` feature every hook is a no-op.

#[cfg(feature = "semantic")]
use std::time::{Duration, Instant};

use super::FennecApp;
use crate::i18n::t;
#[cfg(feature = "semantic")]
use crate::i18n::t_args;

/// Per-app semantic state.
#[derive(Default)]
pub(super) struct SemanticUi {
    #[cfg(feature = "semantic")]
    search: Option<crate::semantic::SemanticSearch>,
    /// Vector generation the visible results were computed with.
    #[cfg(feature = "semantic")]
    seen: u64,
    #[cfg(feature = "semantic")]
    last_refresh: Option<Instant>,
    /// Setup window open.
    pub(super) setup_open: bool,
    /// Folder list being edited in the setup window.
    setup_folders: String,
}

/// `true` if the search sends anything to the semantic channel.
#[cfg_attr(not(feature = "semantic"), allow(dead_code))]
pub(super) fn is_semantic_query(q: &str, smart: bool) -> bool {
    let lower = q.to_ascii_lowercase();
    smart
        || lower.contains("?sem:")
        || lower.contains("?semantic:")
        || ["sem:", "semantic:", "semname:", "semcontent:", "semcode:", "semimage:"].iter().any(|f| {
            lower.split(|c: char| c.is_whitespace() || c == '<' || c == '|' || c == '!').any(|w| w.starts_with(f))
        })
}

impl FennecApp {
    /// Starts semantic search if enabled (app start).
    pub(super) fn semantic_init(&mut self) {
        #[cfg(feature = "semantic")]
        if self.settings.semantic_enabled {
            self.semantic_start();
        }
    }

    #[cfg(feature = "semantic")]
    fn semantic_data_dir(&self) -> std::path::PathBuf {
        self.settings_path.parent().map(std::path::Path::to_path_buf).unwrap_or_default()
    }

    #[cfg(feature = "semantic")]
    fn semantic_start(&mut self) {
        let dir = self.semantic_data_dir();
        let text = crate::semantic::SemanticSearch::plain_text();
        match crate::semantic::SemanticSearch::start(&self.settings, &dir, self.shared.clone(), text) {
            Ok(s) => self.sem.search = Some(s),
            Err(e) => {
                tracing::warn!(error = %e, "cannot start semantic search");
                self.say(t_args("sem-status-error", &[("error", e.to_string().into())]));
            }
        }
    }

    /// The semantic source for the search options (`None` = off, zero cost).
    pub(super) fn semantic_source(&self) -> Option<fnc_exec::Shared<dyn fnc_exec::SemanticSource>> {
        #[cfg(feature = "semantic")]
        {
            self.sem.search.as_ref().map(crate::semantic::SemanticSearch::source)
        }
        #[cfg(not(feature = "semantic"))]
        None
    }

    /// Follows changed settings (enable/disable/scope/model).
    pub(super) fn semantic_apply(&mut self, old: &fnc_core::settings::Settings) {
        #[cfg(feature = "semantic")]
        {
            let on = self.settings.semantic_enabled;
            if !on {
                if let Some(s) = self.sem.search.take() {
                    s.stop();
                }
                return;
            }
            let dir = self.semantic_data_dir();
            let keep = self.sem.search.as_mut().is_some_and(|s| s.reconfigure(&self.settings, &dir));
            if !keep {
                if let Some(s) = self.sem.search.take() {
                    s.stop();
                }
                self.semantic_start();
            }
            if !old.semantic_enabled && self.sem.search.as_ref().is_some_and(|s| !s.model_present()) {
                self.sem.setup_open = true;
            }
        }
        #[cfg(not(feature = "semantic"))]
        let _ = old;
    }

    /// Per-frame: re-runs semantic searches while vectors arrive, finishes downloads.
    pub(super) fn semantic_tick(&mut self, ctx: &egui::Context) {
        #[cfg(feature = "semantic")]
        {
            let Some(s) = &mut self.sem.search else { return };
            if let Some(r) = s.take_finished_download() {
                let msg = match r {
                    Ok(()) => t("sem-download-done"),
                    Err(e) => t_args("sem-download-failed", &[("error", e.into())]),
                };
                self.say(msg);
                self.v.engine.refresh();
                return;
            }
            if s.download().is_some() {
                ctx.request_repaint_after(Duration::from_millis(300));
            }
            let g = s.generation();
            if g != self.sem.seen && is_semantic_query(&self.v.query, self.settings.semantic_smart) {
                // At most every 2 s while indexing.
                if self.sem.last_refresh.is_none_or(|t| t.elapsed() >= Duration::from_secs(2)) {
                    self.sem.seen = g;
                    self.sem.last_refresh = Some(Instant::now());
                    self.v.engine.refresh();
                } else {
                    ctx.request_repaint_after(Duration::from_millis(500));
                }
            }
            if matches!(s.status(), fnc_semantic::Status::Indexing { .. } | fnc_semantic::Status::Loading) {
                ctx.request_repaint_after(Duration::from_secs(1));
            }
        }
        #[cfg(not(feature = "semantic"))]
        let _ = ctx;
    }

    /// The "semantic" toggle next to the search box (= `?sem:` for every search).
    pub(super) fn semantic_toggle(&mut self, ui: &mut egui::Ui) {
        if !self.settings.semantic_enabled {
            return;
        }
        let mut on = self.settings.semantic_smart;
        let r = ui.toggle_value(&mut on, t("sem-toggle")).on_hover_text(t("sem-toggle-tip"));
        if r.changed() {
            self.settings.semantic_smart = on;
            if let Err(e) = self.settings.save(&self.settings_path) {
                tracing::warn!(error = %e, "cannot save settings");
            }
            self.rebuild_search_options();
            self.send_search();
        }
    }

    /// Status-bar text, if semantic search is on.
    pub(super) fn semantic_status(&self) -> Option<String> {
        #[cfg(feature = "semantic")]
        {
            self.sem.search.as_ref().map(crate::semantic::SemanticSearch::status_text)
        }
        #[cfg(not(feature = "semantic"))]
        None
    }

    /// Opens the setup window (Tools → Semantic search…).
    pub(super) fn semantic_open_setup(&mut self) {
        self.sem.setup_open = true;
        self.sem.setup_folders = if self.settings.semantic_folders.is_empty() {
            #[cfg(feature = "semantic")]
            {
                crate::semantic::default_folders().join(";")
            }
            #[cfg(not(feature = "semantic"))]
            String::new()
        } else {
            self.settings.semantic_folders.join(";")
        };
    }

    /// The setup window: what semantic search costs, its scope, the model (SPEC ch.5 §3.3).
    pub(super) fn semantic_setup_window(&mut self, ctx: &egui::Context) {
        if !self.sem.setup_open {
            return;
        }
        let mut open = true;
        #[cfg_attr(not(feature = "semantic"), allow(unused_mut))]
        let mut apply: Option<fnc_core::settings::Settings> = None;
        egui::Window::new(t("sem-setup-title")).open(&mut open).resizable(false).default_width(520.0).show(ctx, |ui| {
            ui.label(t("sem-setup-intro"));
            ui.add_space(6.0);
            #[cfg(not(feature = "semantic"))]
            {
                ui.colored_label(ui.visuals().warn_fg_color, t("sem-setup-unavailable"));
            }
            #[cfg(feature = "semantic")]
            {
                ui.horizontal(|ui| {
                    ui.label(t("sem-setup-folders"));
                    ui.add(egui::TextEdit::singleline(&mut self.sem.setup_folders).desired_width(360.0));
                });
                ui.weak(t("sem-setup-folders-hint"));
                ui.add_space(6.0);
                match &mut self.sem.search {
                    Some(s) => {
                        let spec = s.spec().clone();
                        ui.label(t_args("sem-setup-model", &[("name", spec.name.clone().into())]));
                        ui.weak(s.model_path().display().to_string());
                        if s.model_present() {
                            ui.label(t("sem-setup-model-ok"));
                        } else if let Some(d) = s.download() {
                            let done = d.done.load(std::sync::atomic::Ordering::Relaxed);
                            let frac = if d.total > 0 { done as f32 / d.total as f32 } else { 0.0 };
                            ui.add(egui::ProgressBar::new(frac).show_percentage());
                            if ui.button(t("dialog-cancel")).clicked() {
                                d.cancel();
                            }
                        } else {
                            let mb = spec.size.unwrap_or(0) >> 20;
                            if ui.button(t_args("sem-setup-download", &[("mb", mb.into())])).clicked()
                                && let Err(e) = s.start_download()
                            {
                                self.message = Some((e, Instant::now()));
                            }
                        }
                        let (docs, chunks, bytes) = s.stats();
                        ui.add_space(4.0);
                        ui.weak(t_args(
                            "sem-setup-stats",
                            &[
                                ("docs", docs.into()),
                                ("chunks", chunks.into()),
                                ("size", crate::format::size(bytes as u64).into()),
                            ],
                        ));
                        ui.weak(s.status_text());
                        ui.weak(if s.model_loaded() { t("sem-setup-loaded") } else { t("sem-setup-unloaded") });
                    }
                    None => {
                        ui.label(t("sem-setup-enable-first"));
                    }
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let label =
                        if self.settings.semantic_enabled { t("sem-setup-apply") } else { t("sem-setup-enable") };
                    if ui.button(label).clicked() {
                        let mut s = self.settings.clone();
                        s.semantic_enabled = true;
                        s.semantic_folders = self
                            .sem
                            .setup_folders
                            .split(';')
                            .map(str::trim)
                            .filter(|f| !f.is_empty())
                            .map(str::to_string)
                            .collect();
                        apply = Some(s);
                    }
                    if self.settings.semantic_enabled
                        && ui.button(t("sem-setup-rebuild")).clicked()
                        && let Some(s) = &self.sem.search
                    {
                        s.rebuild();
                    }
                });
            }
        });
        if let Some(s) = apply {
            self.apply_settings(ctx, s);
        }
        if !open {
            self.sem.setup_open = false;
        }
    }

    /// Saves the vectors and stops the semantic indexer (exit).
    pub(super) fn semantic_stop(&mut self) {
        #[cfg(feature = "semantic")]
        if let Some(s) = self.sem.search.take() {
            s.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_queries_are_recognised() {
        assert!(is_semantic_query("sem:budget", false));
        assert!(is_semantic_query("ext:pdf <semcontent:\"api key\" | x>", false));
        assert!(is_semantic_query("?sem: plan", false));
        assert!(is_semantic_query("anything", true));
        assert!(!is_semantic_query("semester notes", false));
        assert!(!is_semantic_query("ext:pdf", false));
    }
}

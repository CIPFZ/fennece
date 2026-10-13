//! Undo/redo of Fennec's file operations and the advanced rename window (SPEC ch.3 §9–10).

use std::path::PathBuf;
use std::time::Duration;

use super::{FennecApp, WorkerMsg};
use crate::i18n::{t, t_args};
use crate::rename::{CaseMode, Item, Preview, Problem, Rule};
use crate::undo::{Action, Op, UndoStack};

/// Open advanced rename window.
pub(super) struct AdvancedRename {
    items: Vec<Item>,
    pub(super) rule: Rule,
    previews: Vec<Preview>,
    /// Rule the previews were computed for.
    computed: Option<Rule>,
}

/// Undo stack, busy flag and the rename window.
#[derive(Default)]
pub(super) struct EditUi {
    pub(super) undo: UndoStack,
    /// An undo/redo is running on a worker thread.
    busy: bool,
    pub(super) rename: Option<AdvancedRename>,
}

/// What a finished worker reports back for the undo stack.
pub(super) enum Recorded {
    /// A new action ran.
    Did(Action),
    /// An action was undone (can be redone).
    Undid(Action),
    /// An action was redone.
    Redid(Action),
}

fn problem_text(p: Problem) -> String {
    t(match p {
        Problem::Empty => "rename-problem-empty",
        Problem::Invalid => "rename-problem-invalid",
        Problem::Duplicate => "rename-problem-duplicate",
        Problem::Exists => "rename-problem-exists",
        Problem::BadPattern => "rename-problem-pattern",
    })
}

impl FennecApp {
    /// Runs `op` on a worker thread; its action goes onto the undo stack.
    pub(super) fn spawn_recorded(
        &mut self,
        done: String,
        op: impl FnOnce() -> std::io::Result<Recorded> + Send + 'static,
    ) {
        let tx = self.worker_tx.clone();
        let r = std::thread::Builder::new().name("fennec-fileop".into()).spawn(move || {
            let msg = match op() {
                Ok(rec) => WorkerMsg::Recorded(done, rec),
                Err(e) => WorkerMsg::Failed(t_args("msg-op-failed", &[("error", e.to_string().into())])),
            };
            let _ = tx.send(msg);
        });
        if let Err(e) = r {
            self.say(t_args("msg-op-failed", &[("error", e.to_string().into())]));
        }
    }

    /// A worker finished something recordable.
    pub(super) fn on_recorded(&mut self, r: Recorded) {
        self.edit.busy = false;
        match r {
            Recorded::Did(a) => self.edit.undo.push(a),
            Recorded::Undid(a) => self.edit.undo.undone(a),
            Recorded::Redid(a) => self.edit.undo.redone(a),
        }
    }

    /// Deletes to the recycle bin, recording where the items went.
    pub(super) fn recycle_paths(&mut self, paths: Vec<PathBuf>) {
        let owner = self.owner();
        let n = paths.len();
        self.spawn_recorded(t_args("msg-deleted", &[("count", n.into())]), move || {
            let recycled = fnc_platform::fileops::delete_to_recycle_bin(owner, &paths)?;
            Ok(Recorded::Did(Action { label: t("undo-delete"), ops: recycled.into_iter().map(Op::Recycle).collect() }))
        });
    }

    /// Renames one item (F2), recorded for undo.
    pub(super) fn rename_recorded(&mut self, path: PathBuf, new: String) {
        let owner = self.owner();
        self.spawn_recorded(t_args("msg-renamed", &[("name", new.clone().into())]), move || {
            fnc_platform::fileops::rename(owner, &path, &new)?;
            let to = path.with_file_name(&new);
            Ok(Recorded::Did(Action { label: t("undo-rename"), ops: vec![Op::Rename { from: path, to }] }))
        });
    }

    /// Creates a folder, recorded for undo.
    pub(super) fn new_folder_recorded(&mut self, parent: PathBuf, name: String) {
        let owner = self.owner();
        self.spawn_recorded(t_args("msg-created", &[("name", name.clone().into())]), move || {
            fnc_platform::fileops::new_folder(owner, &parent, &name)?;
            Ok(Recorded::Did(Action { label: t("undo-new-folder"), ops: vec![Op::CreateFolder(parent.join(&name))] }))
        });
    }

    /// Ctrl+Z.
    pub(super) fn undo_last(&mut self) {
        if self.edit.busy {
            return;
        }
        let Some(a) = self.edit.undo.take_undo() else {
            self.say(t("msg-nothing-to-undo"));
            return;
        };
        self.edit.busy = true;
        let owner = self.owner();
        let label = a.label.clone();
        let tx = self.worker_tx.clone();
        let _ = std::thread::Builder::new().name("fennec-undo".into()).spawn(move || {
            let msg = match crate::undo::undo(owner, &a) {
                Ok(redo) => {
                    WorkerMsg::Recorded(t_args("msg-undone", &[("action", label.into())]), Recorded::Undid(redo))
                }
                // Failed: keep it undoable (the user may fix the cause).
                Err(e) => {
                    WorkerMsg::Recorded(t_args("msg-op-failed", &[("error", e.to_string().into())]), Recorded::Redid(a))
                }
            };
            let _ = tx.send(msg);
        });
    }

    /// Ctrl+Y.
    pub(super) fn redo_last(&mut self) {
        if self.edit.busy {
            return;
        }
        let Some(a) = self.edit.undo.take_redo() else {
            self.say(t("msg-nothing-to-redo"));
            return;
        };
        self.edit.busy = true;
        let owner = self.owner();
        let label = a.label.clone();
        let tx = self.worker_tx.clone();
        let _ = std::thread::Builder::new().name("fennec-redo".into()).spawn(move || {
            let msg = match crate::undo::redo(owner, &a) {
                Ok(done) => {
                    WorkerMsg::Recorded(t_args("msg-redone", &[("action", label.into())]), Recorded::Redid(done))
                }
                Err(e) => {
                    WorkerMsg::Recorded(t_args("msg-op-failed", &[("error", e.to_string().into())]), Recorded::Undid(a))
                }
            };
            let _ = tx.send(msg);
        });
    }

    /// Edit-menu label of Undo / Redo.
    pub(super) fn undo_label(&self, redo: bool) -> String {
        let next = if redo { self.edit.undo.next_redo() } else { self.edit.undo.next_undo() };
        let key = if redo { "cmd-redo-action" } else { "cmd-undo-action" };
        match next {
            Some(a) => t_args(key, &[("action", a.label.clone().into())]),
            None => t(if redo { "cmd-redo" } else { "cmd-undo" }),
        }
    }

    /// `true` if there is something to undo / redo.
    pub(super) fn can_undo(&self, redo: bool) -> bool {
        !self.edit.busy
            && if redo { self.edit.undo.next_redo().is_some() } else { self.edit.undo.next_undo().is_some() }
    }

    /// Opens advanced rename for the selection.
    pub(super) fn open_advanced_rename(&mut self) {
        let Some(snap) = self.shared.try_snapshot_for(Duration::from_millis(50)) else { return };
        let items: Vec<Item> = self
            .target_paths()
            .into_iter()
            .filter(|(id, _)| snap.is_live(*id) && snap.parent(*id).is_some())
            .map(|(id, path)| Item {
                path,
                is_dir: snap.is_dir(id),
                size: snap.size(id),
                modified: snap.date_modified(id),
                created: snap.date_created(id),
            })
            .collect();
        drop(snap);
        if items.is_empty() {
            return;
        }
        self.edit.rename = Some(AdvancedRename { items, rule: Rule::default(), previews: Vec::new(), computed: None });
    }

    /// The advanced rename window.
    pub(super) fn advanced_rename_window(&mut self, ctx: &egui::Context) {
        let Some(mut st) = self.edit.rename.take() else { return };
        if st.computed.as_ref() != Some(&st.rule) {
            let snap = self.shared.try_snapshot_for(Duration::from_millis(20));
            // Existing names in the folder: from the index (no disk access on the UI thread).
            let exists = |dir: &std::path::Path, name: &str| {
                snap.as_ref().is_some_and(|s| s.find_path(&dir.join(name).to_string_lossy()).is_some())
            };
            st.previews = crate::rename::preview(&st.items, &st.rule, &exists);
            st.computed = Some(st.rule.clone());
        }
        let mut open = true;
        let mut apply = false;
        egui::Window::new(t_args("rename-title", &[("count", st.items.len().into())]))
            .open(&mut open)
            .default_size([720.0, 520.0])
            .show(ctx, |ui| {
                egui::Grid::new("adv-rename").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                    let r = &mut st.rule;
                    ui.label(t("rename-template"));
                    ui.add(egui::TextEdit::singleline(&mut r.template).desired_width(420.0))
                        .on_hover_text(t("rename-template-help"));
                    ui.end_row();
                    ui.label(t("rename-find"));
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut r.find).desired_width(200.0));
                        ui.label(t("rename-replace"));
                        ui.add(egui::TextEdit::singleline(&mut r.replace).desired_width(160.0));
                    });
                    ui.end_row();
                    ui.label("");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut r.regex, t("rename-regex"));
                        ui.checkbox(&mut r.match_case, t("rename-match-case"));
                        ui.checkbox(&mut r.keep_ext, t("rename-keep-ext"));
                        ui.checkbox(&mut r.trim, t("rename-trim"));
                    });
                    ui.end_row();
                    ui.label(t("rename-case"));
                    egui::ComboBox::from_id_salt("rename-case").selected_text(case_label(r.case)).show_ui(ui, |ui| {
                        for c in [CaseMode::Keep, CaseMode::Lower, CaseMode::Upper, CaseMode::Title, CaseMode::Sentence]
                        {
                            ui.selectable_value(&mut r.case, c, case_label(c));
                        }
                    });
                    ui.end_row();
                    ui.label(t("rename-counter"));
                    ui.horizontal(|ui| {
                        ui.label(t("rename-start"));
                        ui.add(egui::DragValue::new(&mut r.start));
                        ui.label(t("rename-step"));
                        ui.add(egui::DragValue::new(&mut r.step));
                        ui.label(t("rename-digits"));
                        ui.add(egui::DragValue::new(&mut r.digits).range(1..=12));
                    });
                    ui.end_row();
                });
                ui.separator();
                let problems = st.previews.iter().filter(|p| p.problem.is_some()).count();
                let changes = st.previews.iter().filter(|p| p.changes()).count();
                egui::ScrollArea::vertical().max_height(300.0).auto_shrink([false, true]).show(ui, |ui| {
                    egui::Grid::new("adv-rename-preview").num_columns(3).striped(true).show(ui, |ui| {
                        ui.strong(t("rename-old"));
                        ui.strong(t("rename-new"));
                        ui.strong("");
                        ui.end_row();
                        for p in &st.previews {
                            ui.label(&p.old);
                            if p.old == p.new {
                                ui.weak(&p.new);
                            } else {
                                ui.label(&p.new);
                            }
                            match p.problem {
                                Some(pr) => ui.colored_label(ui.visuals().error_fg_color, problem_text(pr)),
                                None => ui.label(""),
                            };
                            ui.end_row();
                        }
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    let ok = problems == 0 && changes > 0;
                    if ui
                        .add_enabled(ok, egui::Button::new(t_args("rename-apply", &[("count", changes.into())])))
                        .clicked()
                    {
                        apply = true;
                    }
                    if problems > 0 {
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            t_args("rename-problems", &[("count", problems.into())]),
                        );
                    }
                });
            });
        if apply {
            let steps = crate::rename::plan_steps(&st.items, &st.previews);
            let owner = self.owner();
            let n = st.previews.iter().filter(|p| p.changes()).count();
            self.spawn_recorded(t_args("msg-renamed-many", &[("count", n.into())]), move || {
                let mut done = Vec::new();
                for (from, to) in steps {
                    let name = to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    if let Err(e) = fnc_platform::fileops::rename(owner, &from, &name) {
                        // Roll back what already happened.
                        for op in done.iter().rev() {
                            if let Op::Rename { from, to } = op {
                                let back = from
                                    .file_name()
                                    .map(|n: &std::ffi::OsStr| n.to_string_lossy().into_owned())
                                    .unwrap_or_default();
                                let _ = fnc_platform::fileops::rename(owner, to, &back);
                            }
                        }
                        return Err(e);
                    }
                    done.push(Op::Rename { from, to });
                }
                Ok(Recorded::Did(Action { label: t("undo-advanced-rename"), ops: done }))
            });
            return;
        }
        if open {
            self.edit.rename = Some(st);
        }
    }
}

fn case_label(c: CaseMode) -> String {
    t(match c {
        CaseMode::Keep => "rename-case-keep",
        CaseMode::Lower => "rename-case-lower",
        CaseMode::Upper => "rename-case-upper",
        CaseMode::Title => "rename-case-title",
        CaseMode::Sentence => "rename-case-sentence",
    })
}

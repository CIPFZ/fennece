//! Tools → Connect to ETP Server… (SPEC ch.4 §5, ADR-036): the connect dialog and the
//! remote mode of a view. While a view is connected its searches go to the server and its
//! results live in the session's own read-only index ([`crate::remote`]); commands that
//! would change files are refused, and opening a result downloads it first (link type
//! `ftp`) or opens the share path (other link types).

use std::sync::Arc;
use std::time::Duration;

use fnc_core::settings::EtpLinkType;
use fnc_etp::Target;
use fnc_exec::ResultSet;
use fnc_index::SharedIndex;
use fnc_ipc::proto::Mods;

use super::FennecApp;
use crate::commands::Command;
use crate::format;
use crate::i18n::{t, t_args};
use crate::remote::{RemoteEvent, RemoteRequest, RemoteSession, RemoteTarget};

/// The remote state of a view.
pub(super) struct RemoteView {
    pub(super) session: RemoteSession,
    /// Logged in.
    pub(super) connected: bool,
    /// Results of the last search on the server.
    pub(super) total: u64,
}

/// The connect dialog.
#[derive(Clone, Debug)]
pub(super) struct ConnectForm {
    pub(super) host: String,
    pub(super) port: String,
    pub(super) username: String,
    pub(super) password: String,
    pub(super) link: EtpLinkType,
    pub(super) error: Option<String>,
}

impl ConnectForm {
    /// The target typed in (`host` may also be `user:password@host:port`).
    pub(super) fn target(&self) -> Result<RemoteTarget, String> {
        let port: u16 = self.port.trim().parse().map_err(|_| t("msg-etp-bad-port"))?;
        let mut target = Target::parse(&self.host, port).ok_or_else(|| t("msg-etp-bad-host"))?;
        if target.username.is_empty() {
            target.username = self.username.trim().to_string();
        }
        if target.password.is_empty() {
            target.password = self.password.clone();
        }
        Ok(RemoteTarget { target, link: self.link })
    }
}

/// Commands that change files, the index or run history: refused for remote results.
fn changes_files(cmd: Command) -> bool {
    use Command as K;
    matches!(
        cmd,
        K::Delete
            | K::DeletePermanently
            | K::Rename
            | K::AdvancedRename
            | K::Paste
            | K::CutFiles
            | K::NewFolder
            | K::SetRunCount
            | K::Omit
    )
}

/// Shell commands that need a local file (not available for downloaded-on-demand results).
fn needs_local_file(cmd: Command) -> bool {
    use Command as K;
    matches!(cmd, K::OpenWith | K::RunAsAdmin | K::Properties | K::ContextMenu | K::CopyFiles)
}

fn link_label(l: EtpLinkType) -> String {
    t(&format!("etp-link-{}", l.as_str()))
}

impl FennecApp {
    /// The index the current view's result ids belong to (a remote view has its own).
    pub(super) fn view_index(&self) -> &SharedIndex {
        self.v.remote.as_ref().map_or(&self.shared, |r| r.session.index())
    }

    /// The current view shows results of an ETP server.
    pub(super) fn is_remote(&self) -> bool {
        self.v.remote.is_some()
    }

    /// Opens the connect dialog with the last server.
    pub(super) fn open_connect_dialog(&mut self) {
        let s = &self.settings;
        self.dialog = Some(super::dialogs::Dialog::ConnectEtp(Box::new(ConnectForm {
            host: s.etp_client_host.clone(),
            port: s.etp_client_port.to_string(),
            username: s.etp_client_username.clone(),
            password: String::new(),
            link: s.etp_client_link_type,
            error: None,
        })));
    }

    /// The connect dialog body: `Some(true)` = connect, `Some(false)` = cancel.
    pub(super) fn connect_form_ui(&mut self, ui: &mut egui::Ui, f: &mut ConnectForm) -> Option<bool> {
        egui::Grid::new("etp-connect").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            ui.label(t("etp-host"));
            ui.add(
                egui::TextEdit::singleline(&mut f.host)
                    .desired_width(260.0)
                    .hint_text("host  /  user:password@host:port"),
            );
            ui.end_row();
            ui.label(t("etp-port"));
            ui.add(egui::TextEdit::singleline(&mut f.port).desired_width(80.0));
            ui.end_row();
            ui.label(t("etp-username"));
            ui.add(egui::TextEdit::singleline(&mut f.username).desired_width(200.0));
            ui.end_row();
            ui.label(t("etp-password"));
            ui.add(egui::TextEdit::singleline(&mut f.password).desired_width(200.0).password(true));
            ui.end_row();
            ui.label(t("etp-link-type"));
            egui::ComboBox::from_id_salt("etp-link").selected_text(link_label(f.link)).show_ui(ui, |ui| {
                for l in [EtpLinkType::Ftp, EtpLinkType::Server, EtpLinkType::Share, EtpLinkType::AdminShare] {
                    ui.selectable_value(&mut f.link, l, link_label(l));
                }
            });
            ui.end_row();
        });
        ui.weak(t("etp-plain-text-warning"));
        if let Some(e) = &f.error {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        let mut r = None;
        ui.horizontal(|ui| {
            if ui.button(t("etp-connect")).clicked() || enter {
                r = Some(true);
            }
            if ui.button(t("dialog-cancel")).clicked() {
                r = Some(false);
            }
        });
        if r == Some(true) {
            match f.target() {
                Ok(target) => {
                    self.connect_etp(target);
                    return Some(true);
                }
                Err(e) => {
                    f.error = Some(e);
                    return None;
                }
            }
        }
        r
    }

    /// Switches the current view to `target` (the password is not saved).
    pub(super) fn connect_etp(&mut self, target: RemoteTarget) {
        let wake: Arc<dyn Fn() + Send + Sync> = {
            let c = self.ctx.clone();
            Arc::new(move || c.request_repaint())
        };
        let mut s = self.settings.clone();
        s.etp_client_host = target.target.host.clone();
        s.etp_client_port = target.target.port;
        s.etp_client_username = target.target.username.clone();
        s.etp_client_link_type = target.link;
        let ctx = self.ctx.clone();
        self.apply_settings(&ctx, s);
        let label = target.label();
        let session = RemoteSession::connect(target, wake);
        self.v.remote = Some(RemoteView { session, connected: false, total: 0 });
        self.reset_view_results();
        self.say(t_args("msg-etp-connecting", &[("host", label.into())]));
        self.send_search();
    }

    /// Back to the local index.
    pub(super) fn disconnect_etp(&mut self) {
        if self.v.remote.take().is_some() {
            self.reset_view_results();
            self.send_search();
        }
    }

    fn reset_view_results(&mut self) {
        self.v.results = Arc::new(ResultSet::default());
        self.v.rows.clear();
        self.v.sel.clear();
        self.v.selected = None;
        self.v.selected_id = None;
        self.v.highlighter = None;
        self.v.diagnostics.clear();
    }

    /// Sends the view's search to the server; `false` if the view is local.
    pub(super) fn send_remote_search(&mut self, id: u64) -> bool {
        let Some(r) = &self.v.remote else { return false };
        let s = &self.settings;
        let mods = Mods {
            case: s.match_case,
            whole_word: s.match_whole_word,
            path: s.match_path,
            diacritics: s.match_diacritics,
            regex: s.match_regex,
            ..Mods::default()
        };
        let filters: Vec<&crate::filters::Filter> =
            self.v.active_filters.iter().filter_map(|&i| self.filters.list.get(i)).collect();
        let filter = match filters.as_slice() {
            [] => None,
            [f] => Some((
                f.search.clone(),
                Mods {
                    case: f.case,
                    whole_word: f.whole_word,
                    path: f.path,
                    diacritics: f.diacritics,
                    regex: f.regex,
                    ..Mods::default()
                },
            )),
            many => {
                Some((many.iter().map(|f| format!("<{}>", f.search)).collect::<Vec<_>>().join("|"), Mods::default()))
            }
        };
        r.session.search(RemoteRequest {
            id,
            text: self.v.query.clone(),
            mods,
            filter,
            sort: self.v.sort,
            max_results: s.etp_client_max_results.max(1),
        });
        true
    }

    /// Handles the session's events (every frame).
    pub(super) fn drain_remote(&mut self) {
        let Some(r) = &self.v.remote else { return };
        let events: Vec<RemoteEvent> = r.session.events().try_iter().collect();
        for e in events {
            match e {
                RemoteEvent::Connected { welcome } => {
                    tracing::info!(%welcome, "ETP server greeting");
                    if let Some(r) = &mut self.v.remote {
                        r.connected = true;
                        let host = r.session.target().label();
                        self.say(t_args("msg-etp-connected", &[("host", host.into())]));
                    }
                }
                RemoteEvent::Results { outcome, total } => {
                    if let Some(r) = &mut self.v.remote {
                        r.total = total;
                    }
                    self.on_outcome(outcome);
                }
                RemoteEvent::QueryFailed(e) => self.say(t_args("msg-etp-query-failed", &[("error", e.into())])),
                RemoteEvent::Disconnected(e) => {
                    self.disconnect_etp();
                    self.say(t_args("msg-etp-disconnected", &[("error", e.into())]));
                    return;
                }
                RemoteEvent::Downloaded { path, reveal } => {
                    let r = if reveal { fnc_platform::shell::reveal(&path) } else { fnc_platform::shell::open(&path) };
                    if let Err(e) = r {
                        let msg = t_args(
                            "msg-open-failed",
                            &[("path", path.display().to_string().into()), ("error", e.to_string().into())],
                        );
                        self.say(msg);
                    }
                }
                RemoteEvent::DownloadFailed(e) => {
                    self.say(t_args("msg-etp-download-failed", &[("error", e.into())]));
                }
            }
        }
    }

    /// Refuses commands remote results cannot do; `true` if `cmd` was handled here.
    pub(super) fn remote_refuses(&mut self, cmd: Command) -> bool {
        let Some(r) = &self.v.remote else { return false };
        let ftp = r.session.target().link == EtpLinkType::Ftp;
        if changes_files(cmd) || (ftp && needs_local_file(cmd)) {
            self.say(t("msg-etp-not-available"));
            return true;
        }
        false
    }

    /// Opens remote results (link type `ftp`): files are downloaded and then opened (or
    /// shown in their folder), folders are searched on the server. `false` for local
    /// views and other link types (their paths open directly).
    pub(super) fn remote_open(&mut self, reveal: bool) -> bool {
        let Some(r) = &self.v.remote else { return false };
        if r.session.target().link != EtpLinkType::Ftp {
            return false;
        }
        let targets = self.target_paths();
        let snap = self.view_index().try_snapshot_for(Duration::from_millis(20));
        let mut folder = None;
        let mut started = 0usize;
        for (id, path) in targets.into_iter().take(if reveal { 1 } else { 32 }) {
            let is_dir = snap.as_ref().is_some_and(|s| s.is_dir(id));
            if is_dir {
                folder.get_or_insert(path);
            } else if let Some(r) = &self.v.remote {
                r.session.download(path.display().to_string(), reveal);
                started += 1;
            }
        }
        drop(snap);
        if started > 0 {
            self.say(t_args("msg-etp-downloading", &[("count", started.into())]));
        } else if let Some(f) = folder {
            let mut s = f.display().to_string();
            if !s.ends_with(['\\', '/']) {
                s.push('\\');
            }
            self.set_query(format!("\"{s}\" "));
            self.v.focus_search = true;
        }
        self.record_history();
        true
    }

    /// The server and its result count in the status bar.
    pub(super) fn remote_status_ui(&self, ui: &mut egui::Ui) {
        let Some(r) = &self.v.remote else { return };
        ui.separator();
        let host = r.session.target().label();
        let text = if r.connected {
            t_args("status-etp-remote", &[("host", host.into()), ("total", format::group_digits(r.total).into())])
        } else {
            t_args("msg-etp-connecting", &[("host", host.into())])
        };
        ui.colored_label(super::ACCENT, text);
    }
}

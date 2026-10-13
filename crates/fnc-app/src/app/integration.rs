//! Glue between the window and the servers (SPEC ch.4 §3–4): starts them with the app,
//! mirrors the UI state into the IPC [`AppHost`], handles its events, applies the
//! `server` settings and shows the HTTP address in the status bar.

use std::sync::Arc;

use crossbeam_channel::Receiver;
use fnc_ipc::IndexBackend;

use super::FennecApp;
use crate::i18n::{t, t_args};
use crate::indexer::IndexStatus;
use crate::ipc_host::{AppHost, HostEvent, bookmark_entries, filter_entries};
use crate::servers::{CompatStatus, Desired, HttpStatus, Servers, ServersContext, ServersStatus};

/// Servers of a running app.
pub(super) struct AppServers {
    host: Arc<AppHost>,
    servers: Servers,
    events: Receiver<HostEvent>,
    /// Status shown last (messages on changes).
    shown: ServersStatus,
    /// Index status mirrored last.
    index_status: Option<IndexStatus>,
}

impl FennecApp {
    /// Starts the servers (`ctx` is `None` in tests that do not want any).
    pub(super) fn start_servers(&mut self, ctx: Option<ServersContext>) {
        let Some(ctx) = ctx else { return };
        let (tx, events) = crossbeam_channel::unbounded();
        let wake: Arc<dyn Fn() + Send + Sync> = {
            let c = self.ctx.clone();
            Arc::new(move || c.request_repaint())
        };
        let host =
            Arc::new(AppHost::new(self.shared.clone(), Some(self.indexer.control()), tx, wake.clone(), ctx.portable));
        let backend = Arc::new(IndexBackend::new(self.shared.clone(), host.clone()));
        let servers = Servers::start(&ctx, backend, Desired::from_settings(&self.settings), wake);
        self.servers = Some(AppServers { host, servers, events, shown: ServersStatus::default(), index_status: None });
        self.sync_host();
    }

    /// Mirrors search options, filters and bookmarks into the IPC host.
    pub(super) fn sync_host(&mut self) {
        let Some(s) = &self.servers else { return };
        let mut options = (*self.v.search_opts).clone();
        options.omit = None;
        let (filters, bookmarks) = (filter_entries(&self.filters.list), bookmark_entries(&self.bookmarks.list));
        let folders_first = self.settings.folders_first;
        s.host.update(move |h| {
            h.options = options;
            h.folders_first = folders_first;
            h.filters = filters;
            h.bookmarks = bookmarks;
        });
    }

    /// Applies changed `server` settings.
    pub(super) fn apply_server_settings(&mut self) {
        if let Some(s) = &self.servers {
            s.servers.apply(Desired::from_settings(&self.settings));
        }
    }

    /// Handles host events and status changes (every frame, cheap).
    pub(super) fn drain_host_events(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.servers else { return };
        if s.index_status.as_ref() != Some(&self.status) {
            s.index_status = Some(self.status.clone());
            let (loaded, busy) = match &self.status {
                IndexStatus::Starting | IndexStatus::Building { .. } => (false, true),
                IndexStatus::Metadata { .. } => (true, true),
                IndexStatus::Ready { .. } => (true, false),
            };
            let text = super::status_text(&self.status);
            s.host.update(|h| {
                h.db_loaded = loaded;
                h.busy = busy;
                h.status = text;
            });
        }
        let status = s.servers.status();
        let mut notes = Vec::new();
        if status.compat != s.shown.compat {
            match &status.compat {
                CompatStatus::Conflict => notes.push(t("msg-everything-running")),
                CompatStatus::Failed(e) if cfg!(windows) => {
                    notes.push(t_args("msg-op-failed", &[("error", e.clone().into())]));
                }
                _ => {}
            }
        }
        if status.ipc != s.shown.ipc
            && let Some(Err(e)) = &status.ipc
        {
            notes.push(t_args("msg-ipc-failed", &[("error", e.clone().into())]));
        }
        s.shown = status;
        let events: Vec<HostEvent> = s.events.try_iter().collect();
        for n in notes {
            self.say(n);
        }
        for e in events {
            match e {
                HostEvent::Exit => self.quit(ctx),
                HostEvent::Rebuild => self.rebuild_index(),
                HostEvent::Refresh => {
                    self.v.engine.refresh();
                    for w in &self.windows {
                        w.view.engine.refresh();
                    }
                }
                HostEvent::CommandLine(args) => {
                    let cli = crate::cli::parse(args);
                    if !self.apply_cli(ctx, cli, true) {
                        return;
                    }
                }
            }
        }
    }

    /// HTTP address (or error) in the status bar.
    pub(super) fn server_status_ui(&self, ui: &mut egui::Ui) {
        let Some(s) = &self.servers else { return };
        match &s.shown.http {
            HttpStatus::Off => {}
            HttpStatus::Running(addr) => {
                let url = format!("http://{addr}/");
                ui.separator();
                ui.label(t_args("status-http", &[("url", url.into())]));
            }
            HttpStatus::Failed(e) => {
                ui.separator();
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    t_args("status-http-failed", &[("error", e.clone().into())]),
                );
            }
        }
    }

    /// Stops every server (exit).
    pub(super) fn stop_servers(&mut self) {
        if let Some(mut s) = self.servers.take() {
            s.servers.shutdown();
        }
    }

    /// Status of the servers (tests).
    #[cfg(test)]
    pub(super) fn servers_status(&self) -> Option<ServersStatus> {
        self.servers.as_ref().map(|s| s.servers.status())
    }
}

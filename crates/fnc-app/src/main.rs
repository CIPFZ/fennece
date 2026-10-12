//! `fennec` — the Fennec desktop application.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

mod app;
mod cli;
mod fonts;
mod format;
mod i18n;
mod icon;
mod indexer;
#[cfg(windows)]
mod tray;

use std::process::ExitCode;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use fnc_core::logging::{LogConfig, init as init_logging};
use fnc_core::paths::DataPaths;
use fnc_core::settings::Settings;
use fnc_exec::SearchEngine;
use fnc_index::SharedIndex;
use fnc_platform::instance::{Instance, acquire};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e:#}");
            eprintln!("fennec: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<()> {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let cli = cli::parse(raw_args.clone());
    if cli.help {
        eprintln!("{}", cli::USAGE);
        return Ok(());
    }
    if cli.version {
        eprintln!("Fennec {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let paths = DataPaths::resolve(&cli.instance);
    let settings_path = paths.settings_file();
    let (mut settings, settings_error) = match Settings::load(&settings_path) {
        Ok(s) => (s, None),
        Err(e) => (Settings::default(), Some(e)),
    };
    let level = if cli.verbose { "debug" } else { settings.log_level.as_str() };
    let logs = paths.logs_dir();
    let _log_guard = init_logging(&LogConfig {
        dir: Some(&logs),
        prefix: "fennec",
        level,
        max_files: settings.log_max_files as usize,
        console: cli.console || cli.verbose,
    })?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), data = %paths.root.display(), portable = paths.portable, "starting");
    if let Some(e) = settings_error {
        tracing::warn!(error = %e, "settings unreadable; using defaults");
    }
    for u in &cli.unknown {
        tracing::warn!(switch = %u, "unknown command line switch ignored");
    }
    i18n::init(&settings.language);

    // Single instance: forward our command line to a running primary and exit.
    let ctx_cell: Arc<OnceLock<egui::Context>> = Arc::new(OnceLock::new());
    let wake = {
        let c = ctx_cell.clone();
        move || {
            if let Some(ctx) = c.get() {
                ctx.request_repaint();
            }
        }
    };
    let commands_rx = if settings.single_instance && !cli.first_instance {
        match acquire(&cli.instance, &raw_args, Box::new(wake.clone())) {
            Ok(Instance::Secondary) => {
                tracing::info!("forwarded command line to the running instance");
                return Ok(());
            }
            Ok(Instance::Primary(p)) => Some(p),
            Err(e) => {
                tracing::warn!(error = %e, "single-instance check failed; continuing");
                None
            }
        }
    } else {
        None
    };
    if cli.exit {
        return Ok(());
    }

    // Index + search engine.
    if settings.search_threads > 0
        && let Err(e) = fnc_exec::configure_threads(settings.search_threads as usize)
    {
        tracing::warn!(error = %e, "cannot size the search thread pool");
    }
    let shared = SharedIndex::default();
    let (engine_tx, engine_rx) = crossbeam_channel::unbounded();
    let engine = SearchEngine::spawn(shared.clone(), engine_tx, Box::new(wake.clone()));

    let mut file_lists: Vec<std::path::PathBuf> = settings.file_lists.iter().map(Into::into).collect();
    file_lists.extend(cli.file_lists.iter().map(Into::into));
    let mut folders: Vec<std::path::PathBuf> = settings.folders.iter().map(Into::into).collect();
    folders.extend(cli.folders.iter().map(Into::into));
    if !cfg!(windows) && folders.is_empty() && file_lists.is_empty() {
        // Non-Windows default: index the home folder (SPEC ch.1 §12, MAY).
        if let Some(home) = std::env::var_os("HOME") {
            folders.push(home.into());
        }
    }
    let cfg = indexer::IndexerConfig {
        file_lists,
        folders,
        ntfs: cfg!(windows) && settings.ntfs_auto_include,
        ntfs_exclude: settings.ntfs_exclude_volumes.clone(),
        usn_create: settings.usn_create_journal,
        usn_max_size: u64::from(settings.usn_max_size_mb) << 20,
        usn_delta: u64::from(settings.usn_allocation_delta_mb) << 20,
        usn_poll: Duration::from_millis(u64::from(settings.usn_poll_interval.max(100))),
        date_accessed: settings.index_date_accessed,
    };
    let (index_tx, index_rx) = crossbeam_channel::unbounded();
    let handle = engine.handle();
    let indexer = indexer::Indexer::spawn(
        cfg,
        shared.clone(),
        Arc::new(move || handle.refresh()),
        index_tx,
        Arc::new(wake.clone()),
    );
    settings.close_to_tray &= settings.show_tray_icon;

    let viewport = egui::ViewportBuilder::default()
        .with_title(i18n::t("app-title"))
        .with_inner_size([1000.0, 600.0])
        .with_min_inner_size([400.0, 200.0])
        .with_app_id("fennec");
    let viewport = match icon::app_icon() {
        Some(i) => viewport.with_icon(Arc::new(i)),
        None => viewport,
    };
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    let deps = app::AppDeps {
        shared,
        engine,
        engine_rx,
        index_rx,
        commands_rx: commands_rx.map(|p| p.commands().clone()),
        indexer,
        settings,
        settings_path,
        initial_search: cli.search.clone(),
        start_hidden: cli.start_hidden,
    };
    eframe::run_native(
        "Fennec",
        options,
        Box::new(move |cc| {
            let _ = ctx_cell.set(cc.egui_ctx.clone());
            Ok(Box::new(app::FennecApp::new(cc, deps)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("window: {e}"))
}

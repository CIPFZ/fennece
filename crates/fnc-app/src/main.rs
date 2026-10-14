//! `fennec` — the Fennec desktop application.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

mod app;
mod cells;
mod cli;
mod columns;
mod commands;
mod exclude;
mod export;
mod filters;
mod fonts;
mod format;
mod history;
mod i18n;
mod icon;
mod ifilter;
mod import;
mod indexer;
mod ipc_host;
mod journal;
mod remote;
mod rename;
#[cfg(feature = "semantic")]
mod semantic;
mod servers;
mod stores;
mod thumbs;
#[cfg(windows)]
mod tray;
mod undo;
mod upgrade;

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
    // Hidden mode: IFilter host process of the content extraction (ADR-035).
    if ifilter::is_host_invocation(&raw_args) {
        ifilter::run_host();
    }
    let cli = cli::parse(raw_args.clone());
    if cli.help {
        eprintln!("{}", cli::USAGE);
        return Ok(());
    }
    if cli.version {
        eprintln!("Fennec {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let paths = if cli.portable { DataPaths::portable(&cli.instance) } else { DataPaths::resolve(&cli.instance) };
    let settings_path = cli.config.as_ref().map_or_else(|| paths.settings_file(), std::path::PathBuf::from);
    // Never loses the file: newer schemas, bad values and syntax errors keep a copy (ADR-038).
    let (settings, settings_load) = Settings::load_checked(&settings_path);
    let level = if cli.verbose { "debug" } else { settings.log_level.as_str() };
    // `-log <file>`: that folder and file prefix instead of the data folder.
    let (logs, prefix) = match &cli.log {
        Some(f) => {
            let f = std::path::Path::new(f);
            let dir = f
                .parent()
                .filter(|d| !d.as_os_str().is_empty())
                .map_or_else(|| ".".into(), std::path::Path::to_path_buf);
            let stem = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "fennec".into());
            (dir, stem)
        }
        None => (paths.logs_dir(), "fennec".to_string()),
    };
    let _log_guard = init_logging(&LogConfig {
        dir: Some(&logs),
        prefix: &prefix,
        level,
        max_files: settings.log_max_files as usize,
        console: cli.console || cli.verbose,
    })?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), data = %paths.root.display(), portable = paths.portable, "starting");
    if let Some(e) = &settings_load.error {
        tracing::warn!(error = %e, "settings unreadable; using defaults");
    }
    for u in &cli.unknown {
        tracing::warn!(switch = %u, "unknown command line switch ignored");
    }
    i18n::init(&settings.language);
    let settings_notice = upgrade::settings_notice(&settings_load);
    i18n::apply_collation(settings.name_collation);
    i18n::apply_date_locale(settings.date_input_order, settings.week_start);

    if let Some(action) = cli.service {
        return service_command(action, &cli.instance);
    }
    if let Some(tool) = &cli.tool {
        return run_tool(tool);
    }
    if cli.admin && !fnc_platform::privilege::is_elevated() {
        // Restart elevated with the same arguments (minus -admin); this process exits.
        let args: Vec<String> = raw_args
            .iter()
            .filter(|a| !a.trim_start_matches(['-', '/']).eq_ignore_ascii_case("admin"))
            .cloned()
            .collect();
        let joined = args
            .iter()
            .map(|a| if a.contains(' ') { format!("\"{a}\"") } else { a.clone() })
            .collect::<Vec<_>>()
            .join(" ");
        fnc_platform::shell::run_elevated(&std::env::current_exe()?, &format!("-first-instance {joined}"))?;
        return Ok(());
    }
    if integration_commands(&cli)? {
        return Ok(());
    }

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
    let primary = if settings.single_instance && !cli.first_instance {
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
    let new_engine: Arc<dyn Fn() -> (SearchEngine, crossbeam_channel::Receiver<fnc_exec::EngineEvent>) + Send + Sync> = {
        let (shared, wake) = (shared.clone(), wake.clone());
        Arc::new(move || {
            let (tx, rx) = crossbeam_channel::unbounded();
            (SearchEngine::spawn(shared.clone(), tx, Box::new(wake.clone())), rx)
        })
    };

    let db_path = if cli.no_db { None } else { Some(cli.db.as_ref().map_or_else(|| paths.index_file(), Into::into)) };
    // Property, content and hash stores: created once, shared by every search (M3).
    let stores = {
        let handle = engine.handle();
        let dir = db_path.as_ref().and_then(|p| p.parent()).map(std::path::Path::to_path_buf);
        stores::Stores::start(shared.clone(), &settings, dir, Arc::new(move || handle.refresh()))
    };
    let index_cfg = {
        let (cli, paths, db_path, stores) = (cli.clone(), paths.clone(), db_path.clone(), stores.clone());
        move |settings: &Settings| indexer_config(settings, &cli, &paths, db_path.clone(), stores.clone())
    };
    let cfg = index_cfg(&settings);
    let (index_tx, index_rx) = crossbeam_channel::unbounded();
    if let Some(n) = settings_notice {
        let _ = index_tx.send(indexer::IndexEvent::Notice(n));
    }
    let handle = engine.handle();
    let indexer = indexer::Indexer::spawn(
        cfg,
        shared.clone(),
        Arc::new(move || handle.refresh()),
        index_tx.clone(),
        Arc::new(wake.clone()),
    );
    let reindex: app::Reindex = {
        let (shared, handle, wake) = (shared.clone(), engine.handle(), wake.clone());
        Arc::new(move |indexer: &mut indexer::Indexer, settings: &Settings| {
            let mut cfg = index_cfg(settings);
            // `-reindex` aside, a rebuild never reads the database it replaces.
            cfg.reindex = true;
            let handle = handle.clone();
            indexer.restart(
                cfg,
                shared.clone(),
                Arc::new(move || handle.refresh()),
                index_tx.clone(),
                Arc::new(wake.clone()),
            );
        })
    };

    let mut viewport = egui::ViewportBuilder::default()
        .with_title(i18n::t("app-title"))
        .with_inner_size([1000.0, 600.0])
        .with_min_inner_size([400.0, 200.0])
        .with_app_id("fennec")
        .with_maximized(settings.window_maximized)
        // `-startup` / `-minimized`: create the window hidden (no flash).
        .with_visible(!cli.start_hidden);
    // Remembered geometry (SPEC ch.3 §1).
    if let Some([x, y, w, h]) = app::parse_window_rect(&settings.window_rect) {
        viewport = viewport.with_position([x, y]).with_inner_size([w, h]);
    }
    let viewport = match icon::app_icon() {
        Some(i) => viewport.with_icon(Arc::new(i)),
        None => viewport,
    };
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    let deps = app::AppDeps {
        shared,
        engine,
        engine_rx,
        new_engine,
        index_rx,
        instance: primary,
        indexer,
        reindex: Some(reindex),
        stores,
        settings,
        settings_path,
        initial_search: cli.search.clone(),
        initial_cli: Some(cli.clone()),
        start_hidden: cli.start_hidden,
        servers: Some(servers::ServersContext { instance: cli.instance.clone(), portable: paths.portable }),
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

/// What to index, from the settings and the command line (start-up and Rebuild Index).
fn indexer_config(
    settings: &Settings,
    cli: &cli::Cli,
    paths: &fnc_core::paths::DataPaths,
    db_path: Option<std::path::PathBuf>,
    stores: Arc<stores::Stores>,
) -> indexer::IndexerConfig {
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
    indexer::IndexerConfig {
        file_lists,
        folders,
        ntfs: cfg!(windows) && settings.ntfs_auto_include,
        ntfs_exclude: settings.ntfs_exclude_volumes.clone(),
        usn_create: settings.usn_create_journal,
        usn_max_size: u64::from(settings.usn_max_size_mb) << 20,
        usn_delta: u64::from(settings.usn_allocation_delta_mb) << 20,
        usn_poll: Duration::from_millis(u64::from(settings.usn_poll_interval.max(100))),
        date_accessed: settings.index_date_accessed,
        db_path,
        reindex: cli.reindex,
        instance: cli.instance.clone(),
        exclusions: exclude::Exclusions::from_settings(settings),
        folder_rescan: Duration::from_secs(u64::from(settings.folder_rescan_interval) * 60),
        folder_watch: settings.folder_watch,
        rescan_epoch: Arc::default(),
        journal: settings.index_journal_enabled.then(|| {
            (paths.root.join("journal.tsv"), settings.index_journal_days, settings.index_journal_max as usize)
        }),
        save_interval: Duration::from_secs(u64::from(settings.db_save_interval) * 60),
        hooks: Some(stores),
    }
}

/// `-install-service` / `-uninstall-service`: runs `fennec-service.exe` (next to this
/// executable), elevating through UAC when needed.
/// `-install-run-on-system-startup`, context menu switches; `true` if any ran (exit).
fn integration_commands(cli: &cli::Cli) -> anyhow::Result<bool> {
    let exe = std::env::current_exe()?;
    let mut ran = false;
    if let Some(on) = cli.run_on_startup {
        fnc_platform::integration::set_run_on_startup(&exe, on)?;
        ran = true;
    }
    for &(kind, on) in &cli.context_menu {
        let (folder, label) = match kind {
            cli::ContextMenu::Folder => (true, i18n::t("shell-search-fennec")),
            cli::ContextMenu::File => (false, i18n::t("shell-explore-in-fennec")),
        };
        fnc_platform::integration::set_context_menu(&exe, folder, &label, on)?;
        ran = true;
    }
    Ok(ran)
}

/// Commands that work without a window (SPEC ch.4 §1 tool commands).
fn run_tool(tool: &cli::Tool) -> anyhow::Result<()> {
    match tool {
        cli::Tool::CreateFileList { out, folder } => {
            let mut src = fnc_platform::walk::FolderSource::new(folder);
            let mut store = fnc_index::IndexStore::default();
            let vol = store.add_volume(fnc_platform::VolumeSource::info(&src), Some(1))?;
            struct Sink<'a>(fnc_index::BulkLoader<'a>);
            impl fnc_platform::EntrySink for Sink<'_> {
                fn entry(&mut self, e: &fnc_platform::RawEntry<'_>) -> Result<(), fnc_platform::SourceError> {
                    self.0
                        .push(e.frn, e.parent_frn, e.name, &e.meta)
                        .map(|_| ())
                        .map_err(|e| fnc_platform::SourceError::io("index", std::io::Error::other(e)))
                }
            }
            let mut sink = Sink(store.bulk_load(vol));
            fnc_platform::VolumeSource::enumerate(&mut src, &mut sink, &fnc_platform::StopFlag::new())?;
            sink.0.finish()?;
            let f = std::io::BufWriter::new(std::fs::File::create(out)?);
            let mut w = fnc_index::efu::EfuWriter::new(f)?;
            let mut buf = String::new();
            let mut n = 0usize;
            for id in (0..store.len() as u32).map(fnc_core::EntryId) {
                if store.is_live(id) && !store.flags(id).contains(fnc_core::EntryFlags::ROOT) {
                    w.write_entry(&store, id, &mut buf)?;
                    n += 1;
                }
            }
            use std::io::Write;
            w.finish()?.flush()?;
            eprintln!("wrote {n} entries to {out}");
            Ok(())
        }
        #[cfg(windows)]
        cli::Tool::CreateUsnJournal { drive, max, delta } => {
            let letter = drive.chars().next().ok_or_else(|| anyhow::anyhow!("drive letter expected"))?;
            let h = fnc_ntfs::win::open_volume(letter)?;
            fnc_ntfs::win::create_journal(&h, *max, *delta)?;
            eprintln!("USN journal of {letter}: created (max {max}, delta {delta})");
            Ok(())
        }
        #[cfg(windows)]
        cli::Tool::DeleteUsnJournal { drive } => {
            let letter = drive.chars().next().ok_or_else(|| anyhow::anyhow!("drive letter expected"))?;
            let h = fnc_ntfs::win::open_volume(letter)?;
            let j = fnc_ntfs::win::query_journal(&h)?.ok_or_else(|| anyhow::anyhow!("{letter}: has no USN journal"))?;
            fnc_ntfs::win::delete_journal(&h, j.id)?;
            eprintln!("USN journal of {letter}: deleted");
            Ok(())
        }
        #[cfg(not(windows))]
        cli::Tool::CreateUsnJournal { .. } | cli::Tool::DeleteUsnJournal { .. } => {
            anyhow::bail!("USN journals exist on Windows NTFS volumes only")
        }
    }
}

pub(crate) fn service_command(action: cli::ServiceAction, instance: &str) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let name = if cfg!(windows) { "fennec-service.exe" } else { "fennec-service" };
    let svc = exe.with_file_name(name);
    anyhow::ensure!(svc.exists(), "{} not found", svc.display());
    let mut args = match action {
        cli::ServiceAction::Install => "-install".to_string(),
        cli::ServiceAction::Uninstall => "-uninstall".to_string(),
        cli::ServiceAction::Start => "-start".to_string(),
        cli::ServiceAction::Stop => "-stop".to_string(),
    };
    if !instance.is_empty() {
        args.push_str(&format!(" -instance {instance}"));
    }
    if fnc_platform::privilege::is_elevated() {
        let status = std::process::Command::new(&svc).args(args.split(' ')).status()?;
        anyhow::ensure!(status.success(), "fennec-service {args} failed ({status})");
    } else {
        fnc_platform::shell::run_elevated(&svc, &args)?;
    }
    tracing::info!(%args, "service command done");
    Ok(())
}

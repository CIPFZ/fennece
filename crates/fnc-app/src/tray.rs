//! Notification-area icon and global hotkey (Windows; SPEC ch.3 §11).
//!
//! Both crates dispatch from the UI thread's message loop; handlers forward events to a
//! channel and wake egui, whose `App::logic` runs even while the window is hidden.

use crossbeam_channel::{Receiver, Sender};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::i18n::t;

/// Events from the tray icon and the hotkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellEvent {
    /// Left click on the tray icon: toggle the window.
    Toggle,
    /// "Show" menu item or hotkey: show and focus the search box.
    Show,
    /// "Options" menu item.
    Options,
    /// "Pause indexing updates" menu item.
    TogglePause,
    /// "Exit" menu item.
    Exit,
}

/// Keeps the tray icon and hotkey registration alive.
pub struct Integration {
    _tray: Option<TrayIcon>,
    _hotkeys: Option<GlobalHotKeyManager>,
    /// Incoming events.
    pub events: Receiver<ShellEvent>,
}

fn send(tx: &Sender<ShellEvent>, ctx: &egui::Context, e: ShellEvent) {
    let _ = tx.send(e);
    ctx.request_repaint();
}

/// Creates the tray icon (if `tray`) and registers `hotkey` (if non-empty).
pub fn install(ctx: &egui::Context, tray: bool, hotkey: &str) -> Integration {
    let (tx, rx) = crossbeam_channel::unbounded();
    let tray_icon = if tray { build_tray(ctx, &tx) } else { None };
    let hotkeys = if hotkey.trim().is_empty() { None } else { register_hotkey(ctx, &tx, hotkey) };
    Integration { _tray: tray_icon, _hotkeys: hotkeys, events: rx }
}

fn build_tray(ctx: &egui::Context, tx: &Sender<ShellEvent>) -> Option<TrayIcon> {
    let (rgba, w, h) = crate::icon::tray_rgba()?;
    let icon = tray_icon::Icon::from_rgba(rgba, w, h).ok()?;
    let show = MenuItem::new(t("tray-show"), true, None);
    let options = MenuItem::new(t("tray-options"), true, None);
    let pause = MenuItem::new(t("tray-pause"), true, None);
    let exit = MenuItem::new(t("tray-exit"), true, None);
    let menu = Menu::new();
    let sep = PredefinedMenuItem::separator();
    if let Err(e) = menu.append_items(&[&show, &options, &pause, &sep, &exit]) {
        tracing::warn!(error = %e, "tray menu");
    }
    let ids = [
        (show.id().clone(), ShellEvent::Show),
        (options.id().clone(), ShellEvent::Options),
        (pause.id().clone(), ShellEvent::TogglePause),
        (exit.id().clone(), ShellEvent::Exit),
    ];
    let (tx_menu, ctx_menu) = (tx.clone(), ctx.clone());
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        if let Some((_, ev)) = ids.iter().find(|(id, _)| *id == e.id) {
            send(&tx_menu, &ctx_menu, *ev);
        }
    }));
    let (tx_icon, ctx_icon) = (tx.clone(), ctx.clone());
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| match e {
        TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } => {
            send(&tx_icon, &ctx_icon, ShellEvent::Toggle);
        }
        TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } => send(&tx_icon, &ctx_icon, ShellEvent::Show),
        _ => {}
    }));
    // Left click toggles the window; the menu is on the right button only.
    match TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip(t("app-title"))
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
    {
        Ok(t) => Some(t),
        Err(e) => {
            tracing::warn!(error = %e, "cannot create tray icon");
            None
        }
    }
}

/// Accepts `Ctrl+Backquote` style names (as stored in settings).
fn parse_hotkey(s: &str) -> Option<HotKey> {
    let normalized = s.replace(' ', "");
    normalized.parse::<HotKey>().ok().or_else(|| normalized.replace("Ctrl", "Control").parse().ok())
}

fn register_hotkey(ctx: &egui::Context, tx: &Sender<ShellEvent>, hotkey: &str) -> Option<GlobalHotKeyManager> {
    let Some(hk) = parse_hotkey(hotkey) else {
        tracing::warn!(hotkey, "invalid hotkey");
        return None;
    };
    let manager = match GlobalHotKeyManager::new() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "global hotkeys unavailable");
            return None;
        }
    };
    if let Err(e) = manager.register(hk) {
        tracing::warn!(hotkey, error = %e, "cannot register hotkey (in use?)");
        return None;
    }
    let (tx, ctx) = (tx.clone(), ctx.clone());
    GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
        if e.state == HotKeyState::Pressed {
            send(&tx, &ctx, ShellEvent::Show);
        }
    }));
    Some(manager)
}

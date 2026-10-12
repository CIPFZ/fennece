# ADR-001: egui / eframe version and rendering stack

- Status: accepted (M0)
- Related: SPEC ch.0 §5 (GUI), ch.6 §8 Q2

## Background
SPEC requires `eframe` + `egui` + `egui_extras` at the latest stable version, locked in `Cargo.lock`,
with a virtualised result table able to scroll 1 M rows at ≥ 55 fps (UI-01), AccessKit support, IME
input and multiple viewports later (M6).

## Decision
- `eframe`/`egui`/`egui_extras` **0.36.2** (latest at implementation time), pinned via `Cargo.lock`.
- eframe features: `glow` (OpenGL) renderer, `default_fonts`, `accesskit`, `x11` + `wayland` (Linux back-end only).
  The wgpu renderer is not compiled (smaller binary and build, no shader toolchain).
- 0.36 splits `App::ui` (per frame, only when visible) from `App::logic` (also runs while the window is
  hidden on `request_repaint`). Fennec drains engine/indexer/tray channels in `logic`, so tray clicks and the
  global hotkey can show a hidden window without platform hacks.
- Result list: `egui_extras::TableBuilder` with `body.rows(height, n, …)` (only visible rows are laid out)
  — the SPEC Q2 preference. Revisit `egui_table` only if column drag-reorder/frozen columns are needed (M2).
- CJK glyphs: a system CJK font is loaded at runtime as a fallback (egui ships none); nothing is bundled.
- Headless UI tests use `egui_kittest` (eframe feature, no renderer).

## Alternatives
- wgpu renderer: better on some drivers, but larger and slower to build; can be enabled later via feature.
- `egui_table`: richer column handling, less mature; not needed for M1.
- Other toolkits (iced, slint, native Win32): rejected by SPEC.

## Consequences
- Measured on the 1 M-entry fixture: UI pass + tessellation while paging is ~1.2 ms CPU per frame
  (`ui01_scrolling_one_million_results`, release); GPU submission is not included in that number.
- Upgrading egui is a deliberate change (API churn between minor versions); bump together with
  `egui_extras` and `egui_kittest`.

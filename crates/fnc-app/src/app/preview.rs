//! Preview panel (SPEC ch.3 §5, Ctrl+P): pictures, text (first 1 MiB), a shell
//! thumbnail for other types, file details, indexed properties, folder statistics, and
//! totals for a multi-selection. For `content:` searches the text is the extracted text
//! (indexed, else extracted on demand), every match is highlighted and the view scrolls
//! to the first hit. Loading happens on worker threads.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use fnc_core::{EntryId, FileTime};

use super::FennecApp;
use crate::i18n::{t, t_args};
use crate::thumbs::ThumbKey;

/// Bytes of text shown.
const TEXT_LIMIT: usize = 1 << 20;

const TEXT_EXTS: &[&str] = &[
    "txt",
    "md",
    "log",
    "ini",
    "cfg",
    "conf",
    "json",
    "xml",
    "csv",
    "tsv",
    "html",
    "htm",
    "css",
    "js",
    "ts",
    "tsx",
    "jsx",
    "py",
    "rs",
    "go",
    "java",
    "kt",
    "c",
    "h",
    "cpp",
    "hpp",
    "cc",
    "cs",
    "rb",
    "php",
    "sh",
    "ps1",
    "bat",
    "cmd",
    "toml",
    "yaml",
    "yml",
    "sql",
    "lua",
    "swift",
    "gradle",
    "properties",
    "reg",
    "srt",
    "vb",
    "vbs",
    "tex",
    "rst",
];

/// Highlighted matches shown at most.
const MAX_HIGHLIGHTS: usize = 5000;

/// A loaded text preview.
pub(super) struct Loaded {
    text: String,
    truncated: bool,
    /// Byte ranges of content matches in `text`.
    highlights: Vec<Range<usize>>,
    /// Byte offset of the first content hit (scroll target).
    first_hit: Option<usize>,
}

/// What the panel shows for the current selection.
enum Body {
    Empty,
    Loading,
    Text {
        loaded: Loaded,
        /// Laid-out text (built once per theme).
        galley: Option<(bool, Arc<egui::Galley>)>,
        /// Scrolled to the first hit already.
        scrolled: bool,
    },
    Picture,
    Other,
    Failed(String),
}

/// Selection summary computed from the index.
#[derive(Default, Clone)]
struct Info {
    name: String,
    path: PathBuf,
    is_dir: bool,
    size: Option<u64>,
    modified: FileTime,
    created: FileTime,
    accessed: FileTime,
    attributes: u32,
    child_files: usize,
    child_folders: usize,
    stamp: u64,
}

/// A loaded text preview (text, truncated) or an error message.
type TextResult = Result<(String, bool), String>;

/// A loaded preview or an error message.
type LoadResult = Result<Loaded, String>;

/// Multi-selection summary: count, total size, most common types.
type MultiInfo = (usize, u64, Vec<(String, usize)>);

/// Panel state.
pub(super) struct PreviewState {
    /// Cheap fingerprint of the selection (all, count, cursor, result set).
    key: (bool, usize, Option<EntryId>, usize),
    for_ids: Vec<EntryId>,
    generation: u64,
    info: Option<Info>,
    multi: Option<MultiInfo>,
    body: Body,
    /// Indexed properties of the selected file.
    props: Vec<(String, String)>,
    /// Plan of the shown search (content highlights), when it has content terms.
    plan: Option<Arc<fnc_query::PlannedQuery>>,
    tx: Sender<(EntryId, LoadResult)>,
    rx: Receiver<(EntryId, LoadResult)>,
}

impl Default for PreviewState {
    fn default() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        Self {
            key: (false, 0, None, 0),
            for_ids: Vec::new(),
            generation: 0,
            info: None,
            multi: None,
            body: Body::Empty,
            props: Vec::new(),
            plan: None,
            tx,
            rx,
        }
    }
}

impl PreviewState {
    /// Text preview state: scrolled to the first hit, text of the first highlight.
    #[cfg(test)]
    pub(super) fn text_state(&self) -> Option<(bool, String)> {
        match &self.body {
            Body::Text { loaded, scrolled, .. } => {
                let r = loaded.highlights.first()?;
                Some((*scrolled, loaded.text.get(r.clone())?.to_string()))
            }
            _ => None,
        }
    }

    /// Remembers the plan of the shown search if it searches content.
    pub(super) fn set_plan(&mut self, plan: &Arc<fnc_query::PlannedQuery>) {
        let mut preds = Vec::new();
        fnc_query::plan::collect_content(&plan.root, &mut preds);
        let p = (!preds.is_empty()).then(|| plan.clone());
        if p.as_ref().map(Arc::as_ptr) != self.plan.as_ref().map(Arc::as_ptr) {
            self.plan = p;
            // Reload the text with the new highlights.
            self.key.3 = 0;
        }
    }
}

/// Largest char boundary `<= i`.
fn floor_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Text for a `content:` search: the indexed text, else the extracted text, else the
/// decoded file; with the matches of `plan` and the first hit. Long texts are cut to a
/// window of [`TEXT_LIMIT`] bytes around the first hit.
pub(super) fn load_content(
    indexed: Option<String>,
    path: &Path,
    on_demand: &dyn fnc_props::ondemand::OnDemand,
    plan: &fnc_query::PlannedQuery,
    hit: Option<usize>,
) -> LoadResult {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let text = match indexed.or_else(|| on_demand.content(path, &ext)) {
        Some(t) => t,
        None => read_text(path)?.0,
    };
    let highlights = fnc_exec::content_highlights(plan, &text, MAX_HIGHLIGHTS);
    let first = hit.filter(|&h| h < text.len()).or_else(|| highlights.first().map(|r| r.start));
    Ok(windowed(text, highlights, first))
}

/// Text for a semantic hit: the extracted text with the best chunk marked (SPEC ch.5 §3.3).
pub(super) fn load_semantic(
    path: &Path,
    on_demand: &dyn fnc_props::ondemand::OnDemand,
    chunk: Range<usize>,
) -> LoadResult {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let text = match on_demand.content(path, &ext) {
        Some(t) => t,
        None => read_text(path)?.0,
    };
    let end = floor_boundary(&text, chunk.end.min(text.len()));
    let start = floor_boundary(&text, chunk.start.min(end));
    let highlights: Vec<Range<usize>> = if start < end { std::iter::once(start..end).collect() } else { Vec::new() };
    Ok(windowed(text, highlights, Some(start)))
}

/// Cuts long texts to a window of [`TEXT_LIMIT`] bytes around `first`.
fn windowed(text: String, mut highlights: Vec<Range<usize>>, first: Option<usize>) -> Loaded {
    if text.len() <= TEXT_LIMIT {
        return Loaded { text, truncated: false, highlights, first_hit: first };
    }
    let start = floor_boundary(&text, first.unwrap_or(0).saturating_sub(TEXT_LIMIT / 4));
    let end = floor_boundary(&text, start + TEXT_LIMIT);
    highlights.retain(|r| r.start >= start && r.end <= end);
    for r in &mut highlights {
        *r = r.start - start..r.end - start;
    }
    Loaded {
        text: text[start..end].to_string(),
        truncated: true,
        highlights,
        first_hit: first.map(|f| f.saturating_sub(start)),
    }
}

/// Lays out `text` in monospace with `highlights` marked.
fn text_job(ui: &egui::Ui, l: &Loaded) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let normal = TextFormat { font_id: font.clone(), color: ui.visuals().text_color(), ..Default::default() };
    let marked =
        TextFormat { font_id: font, color: egui::Color32::BLACK, background: super::ACCENT, ..Default::default() };
    let mut job = LayoutJob::default();
    let mut pos = 0;
    for r in &l.highlights {
        if r.start < pos || r.end > l.text.len() || !l.text.is_char_boundary(r.start) || !l.text.is_char_boundary(r.end)
        {
            continue;
        }
        job.append(&l.text[pos..r.start], 0.0, normal.clone());
        job.append(&l.text[r.clone()], 0.0, marked.clone());
        pos = r.end;
    }
    job.append(&l.text[pos..], 0.0, normal);
    job.wrap.max_width = f32::INFINITY;
    job
}

fn is_text(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| TEXT_EXTS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// Reads the first [`TEXT_LIMIT`] bytes as text (UTF-8, UTF-16 with BOM, or lossy);
/// `Err` for binary files.
pub(super) fn read_text(path: &Path) -> TextResult {
    use std::io::Read;
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut buf = Vec::with_capacity(TEXT_LIMIT.min(len as usize));
    f.take(TEXT_LIMIT as u64).read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let truncated = len > TEXT_LIMIT as u64;
    let utf16 = |be: bool, b: &[u8]| -> String {
        let units: Vec<u16> = b
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| if be { u16::from_be_bytes(*c) } else { u16::from_le_bytes(*c) })
            .collect();
        String::from_utf16_lossy(&units)
    };
    let text = match buf.as_slice() {
        [0xFF, 0xFE, rest @ ..] => utf16(false, rest),
        [0xFE, 0xFF, rest @ ..] => utf16(true, rest),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        b => {
            // NULs in the first block mean binary.
            if b.iter().take(4096).any(|&c| c == 0) {
                return Err(t("preview-binary"));
            }
            String::from_utf8_lossy(b).into_owned()
        }
    };
    Ok((text, truncated))
}

fn attr_text(a: u32) -> String {
    crate::columns::attribute_letters(a)
}

impl FennecApp {
    /// Recomputes what to show when the selection or the index changed.
    fn refresh_preview(&mut self) {
        let key = (
            self.v.sel.all,
            self.v.sel.ids.len(),
            self.v.selected_id,
            std::sync::Arc::as_ptr(&self.v.results) as usize,
        );
        let generation = self
            .view_index()
            .try_snapshot_for(Duration::from_millis(1))
            .map_or(self.v.preview.generation, |s| s.generation());
        if key == self.v.preview.key && generation == self.v.preview.generation {
            return;
        }
        self.v.preview.key = key;
        let ids = self.target_ids();
        let Some(snap) = self.view_index().try_snapshot_for(Duration::from_millis(5)) else { return };
        // Remote results: metadata only, their paths are not read.
        let remote = self.is_remote();
        let p = &mut self.v.preview;
        p.for_ids = ids.clone();
        p.generation = generation;
        p.info = None;
        p.multi = None;
        p.body = Body::Empty;
        p.props.clear();
        match ids.as_slice() {
            [] => {}
            [id] if snap.is_live(*id) => {
                let id = *id;
                let mut path = String::new();
                snap.full_path(id, &mut path);
                let is_dir = snap.is_dir(id);
                let (mut files, mut folders) = (0, 0);
                if is_dir {
                    for c in snap.children(id).filter(|&c| snap.is_live(c)) {
                        if snap.is_dir(c) { folders += 1 } else { files += 1 }
                    }
                }
                let size = if is_dir { None } else { snap.size(id) };
                let info = Info {
                    name: snap.name_string(id),
                    path: PathBuf::from(&path),
                    is_dir,
                    size,
                    modified: snap.date_modified(id),
                    created: snap.date_created(id),
                    accessed: snap.date_accessed(id),
                    attributes: snap.attributes(id),
                    child_files: files,
                    child_folders: folders,
                    stamp: (snap.date_modified(id).0 as u64) ^ size.unwrap_or(0).rotate_left(17),
                };
                // Indexed properties (non-blocking; empty if the store is busy).
                if !is_dir
                    && let Some(ps) = self.stores.props.try_snapshot_for(Duration::from_millis(1))
                    && ps.instance() == snap.instance()
                {
                    p.props = ps
                        .values(id)
                        .into_iter()
                        .map(|(prop, v)| {
                            (
                                crate::columns::ColumnId::Property(prop).title(),
                                crate::columns::format_property(prop, &v),
                            )
                        })
                        .collect();
                }
                let hit = self.v.results.content_hit(id).map(|h| h.offset as usize);
                let chunk = self.v.results.semantic_range(id);
                p.body = if is_dir || remote {
                    Body::Empty
                } else if let (Some(chunk), None) = (chunk, &p.plan) {
                    // Semantic hit in a content/code chunk: show that passage.
                    let (tx, path, od) = (p.tx.clone(), info.path.clone(), self.stores.on_demand());
                    let _ = std::thread::Builder::new().name("fennec-preview".into()).spawn(move || {
                        let _ = tx.send((id, load_semantic(&path, &*od, chunk)));
                    });
                    Body::Loading
                } else if let Some(plan) = p.plan.clone() {
                    // `content:` search: extracted text with highlights.
                    let (tx, path) = (p.tx.clone(), info.path.clone());
                    let (content, od, instance) =
                        (self.stores.content.clone(), self.stores.on_demand(), snap.instance());
                    let _ = std::thread::Builder::new().name("fennec-preview".into()).spawn(move || {
                        let indexed = {
                            let c = content.snapshot();
                            (c.instance() == instance).then(|| c.text(id)).flatten()
                        };
                        let _ = tx.send((id, load_content(indexed, &path, &*od, &plan, hit)));
                    });
                    Body::Loading
                } else if crate::thumbs::is_picture(&info.path) {
                    Body::Picture
                } else if is_text(&info.path) || size.is_some_and(|s| s < 64 << 10) && info.path.extension().is_none() {
                    let (tx, path) = (p.tx.clone(), info.path.clone());
                    let _ = std::thread::Builder::new().name("fennec-preview".into()).spawn(move || {
                        let r = read_text(&path).map(|(text, truncated)| Loaded {
                            text,
                            truncated,
                            highlights: Vec::new(),
                            first_hit: None,
                        });
                        let _ = tx.send((id, r));
                    });
                    Body::Loading
                } else {
                    Body::Other
                };
                p.info = Some(info);
            }
            many => {
                let mut total = 0u64;
                let mut types: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
                for &id in many.iter().filter(|&&i| snap.is_live(i)).take(200_000) {
                    if snap.is_dir(id) {
                        *types.entry(t("type-folder")).or_default() += 1;
                    } else {
                        total += snap.size(id).unwrap_or(0);
                        let ext = String::from_utf8_lossy(snap.ext_dict().name(snap.ext_id(id))).to_uppercase();
                        *types.entry(if ext.is_empty() { t("type-file") } else { ext }).or_default() += 1;
                    }
                }
                let mut top: Vec<(String, usize)> = types.into_iter().collect();
                top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                top.truncate(8);
                p.multi = Some((many.len(), total, top));
            }
        }
    }

    /// Draws the preview side panel.
    pub(super) fn preview_panel(&mut self, ui: &mut egui::Ui) {
        self.refresh_preview();
        while let Ok((id, r)) = self.v.preview.rx.try_recv() {
            if self.v.preview.for_ids == [id] {
                self.v.preview.body = match r {
                    Ok(loaded) => Body::Text { loaded, galley: None, scrolled: false },
                    Err(e) => Body::Failed(e),
                };
            }
        }
        let fmt = self.settings.date_format;
        let now = FileTime::now();
        egui::ScrollArea::vertical().id_salt("preview-scroll").auto_shrink([false, false]).show(ui, |ui| {
            if let Some((n, total, types)) = &self.v.preview.multi {
                ui.heading(t_args("preview-selected", &[("count", (*n).into())]));
                ui.label(t_args("preview-total-size", &[("size", crate::format::size(*total).into())]));
                ui.separator();
                for (ty, c) in types {
                    ui.label(format!("{ty}: {c}"));
                }
                return;
            }
            let Some(info) = self.v.preview.info.clone() else {
                ui.weak(t("preview-nothing"));
                return;
            };
            let avail = ui.available_width();
            let dark = ui.visuals().dark_mode;
            match &mut self.v.preview.body {
                Body::Picture | Body::Other => {
                    let size = (avail * ui.ctx().pixels_per_point()).clamp(64.0, 1024.0) as u32;
                    let key = ThumbKey {
                        path: info.path.clone(),
                        size: size.next_power_of_two().min(1024),
                        icon: false,
                        stamp: info.stamp,
                    };
                    match self.thumbs.get(key, &info.path) {
                        Some(tex) => {
                            // Texture pixels → points; fit the panel, never upscale.
                            let native = tex.size_vec2() / ui.ctx().pixels_per_point();
                            let scale = (avail / native.x).min(420.0 / native.y).min(1.0);
                            ui.add(egui::Image::new(&tex).fit_to_exact_size(native * scale));
                        }
                        None => {
                            ui.weak(t("preview-loading"));
                        }
                    }
                }
                Body::Loading => {
                    ui.weak(t("preview-loading"));
                }
                Body::Text { loaded, galley, scrolled } => {
                    let g = match galley {
                        Some((d, g)) if *d == dark => g.clone(),
                        _ => {
                            let job = text_job(ui, loaded);
                            let g = ui.fonts_mut(|f| f.layout_job(job));
                            *galley = Some((dark, g.clone()));
                            g
                        }
                    };
                    if !loaded.highlights.is_empty() {
                        let n = loaded.highlights.len();
                        ui.weak(t_args("preview-hits", &[("count", n.into())]));
                    }
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        egui::ScrollArea::both().id_salt("preview-text").max_height(ui.available_height() * 0.6).show(
                            ui,
                            |ui| {
                                let resp = ui.add(egui::Label::new(g.clone()).wrap_mode(egui::TextWrapMode::Extend));
                                // Jump to the first content hit once.
                                if !*scrolled && let Some(at) = loaded.first_hit {
                                    *scrolled = true;
                                    let chars = loaded.text[..floor_boundary(&loaded.text, at)].chars().count();
                                    let r = g.pos_from_cursor(egui::text::CCursor::new(chars));
                                    ui.scroll_to_rect(r.translate(resp.rect.min.to_vec2()), Some(egui::Align::Center));
                                }
                            },
                        );
                    });
                    if loaded.truncated {
                        ui.weak(t("preview-truncated"));
                    }
                }
                Body::Failed(e) => {
                    ui.weak(e.as_str());
                }
                Body::Empty => {}
            }
            ui.separator();
            ui.heading(&info.name);
            egui::Grid::new("preview-info").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                let mut row = |k: &str, v: String| {
                    if !v.is_empty() {
                        ui.weak(t(k));
                        ui.add(egui::Label::new(v).wrap());
                        ui.end_row();
                    }
                };
                row("col-path", info.path.parent().map(|p| p.display().to_string()).unwrap_or_default());
                if let Some(s) = info.size {
                    row("col-size", format!("{} ({} B)", crate::format::size(s), crate::format::group_digits(s)));
                }
                if info.is_dir {
                    row(
                        "preview-contents",
                        t_args(
                            "preview-children",
                            &[("files", info.child_files.into()), ("folders", info.child_folders.into())],
                        ),
                    );
                }
                row("col-dm", crate::format::date_with(info.modified, fmt, now));
                row("col-dc", crate::format::date_with(info.created, fmt, now));
                row("col-da", crate::format::date_with(info.accessed, fmt, now));
                row("col-attributes", attr_text(info.attributes));
                for (k, v) in &self.v.preview.props {
                    if !v.is_empty() {
                        ui.weak(k);
                        ui.add(egui::Label::new(v).wrap());
                        ui.end_row();
                    }
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_detection_and_decoding() {
        crate::i18n::init("en");
        let d = tempfile::tempdir().unwrap();
        let utf8 = d.path().join("a.txt");
        std::fs::write(&utf8, "héllo 季度").unwrap();
        assert_eq!(read_text(&utf8).unwrap(), ("héllo 季度".to_string(), false));
        let utf16 = d.path().join("b.txt");
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend("报告".encode_utf16().flat_map(u16::to_le_bytes));
        std::fs::write(&utf16, bytes).unwrap();
        assert_eq!(read_text(&utf16).unwrap().0, "报告");
        let bin = d.path().join("c.bin");
        std::fs::write(&bin, [0u8, 1, 2, 3]).unwrap();
        assert!(read_text(&bin).is_err());
        let big = d.path().join("d.log");
        std::fs::write(&big, vec![b'x'; TEXT_LIMIT + 10]).unwrap();
        let (t, truncated) = read_text(&big).unwrap();
        assert!(truncated && t.len() == TEXT_LIMIT);
        assert!(is_text(Path::new("x.RS")) && !is_text(Path::new("x.exe")));
    }
}

#[cfg(test)]
mod semantic_tests {
    use super::*;

    struct NoExtract;
    impl fnc_props::ondemand::OnDemand for NoExtract {
        fn properties(
            &self,
            _: &Path,
            _: &str,
            _: &[fnc_props::PropertyId],
        ) -> Vec<(fnc_props::PropertyId, fnc_props::Value)> {
            Vec::new()
        }
        fn content(&self, _: &Path, _: &str) -> Option<String> {
            None
        }
        fn text_as(&self, _: &Path, _: fnc_props::content::TextEncoding) -> Option<String> {
            None
        }
        fn bytes(&self, _: &Path) -> Option<Vec<u8>> {
            None
        }
    }

    #[test]
    fn semantic_chunk_is_marked() {
        crate::i18n::init("en");
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("notes.txt");
        std::fs::write(&p, "Intro.\n\nTo rotate an API key, open the console.\n\nOutro.").unwrap();
        let l = load_semantic(&p, &NoExtract, 8..47).unwrap();
        assert_eq!(&l.text[l.highlights[0].clone()], "To rotate an API key, open the console.");
        assert_eq!(l.first_hit, Some(8));
        // Out-of-range chunks (file changed) are clamped, never panic.
        let l = load_semantic(&p, &NoExtract, 40..4000).unwrap();
        assert!(l.highlights[0].end <= l.text.len());
    }
}

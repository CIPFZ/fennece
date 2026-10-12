//! CJK-capable fonts: egui's bundled fonts have no CJK glyphs, so a system font is
//! added as a fallback (loaded on a background thread to keep start-up fast).

use std::path::PathBuf;

/// Candidate CJK fonts in preference order.
fn candidates() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        let windir = std::env::var_os("WINDIR").map_or_else(|| PathBuf::from("C:\\Windows"), PathBuf::from);
        for f in ["msyh.ttc", "msyh.ttf", "simsun.ttc", "meiryo.ttc", "YuGothR.ttc", "malgun.ttf"] {
            v.push(windir.join("Fonts").join(f));
        }
    } else if cfg!(target_os = "macos") {
        for f in ["/System/Library/Fonts/PingFang.ttc", "/System/Library/Fonts/Hiragino Sans GB.ttc"] {
            v.push(PathBuf::from(f));
        }
    } else {
        for f in [
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
            "/usr/share/fonts/wqy-microhei/wqy-microhei.ttc",
            "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        ] {
            v.push(PathBuf::from(f));
        }
    }
    v
}

/// Loads the first available CJK font and installs it as a fallback for all text.
pub fn install_cjk_fallback(ctx: egui::Context) {
    let _ = std::thread::Builder::new().name("fennec-fonts".into()).spawn(move || {
        for path in candidates() {
            let Ok(bytes) = std::fs::read(&path) else { continue };
            tracing::debug!(font = %path.display(), "using CJK fallback font");
            let fallback = |family: egui::FontFamily| egui::epaint::text::InsertFontFamily {
                family,
                priority: egui::epaint::text::FontPriority::Lowest,
            };
            ctx.add_font(egui::epaint::text::FontInsert {
                name: "cjk-fallback".into(),
                data: egui::FontData::from_owned(bytes),
                families: vec![fallback(egui::FontFamily::Proportional), fallback(egui::FontFamily::Monospace)],
            });
            ctx.request_repaint();
            return;
        }
        tracing::info!("no CJK font found; CJK names may not render");
    });
}

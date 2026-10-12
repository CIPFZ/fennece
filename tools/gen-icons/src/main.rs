//! `gen-icons [out-dir]` — renders the Fennec icon set into `assets/icon/`.
//!
//! The geometry below is the single source of truth: it is written out as
//! `fennec.svg` and rasterised (4×4 supersampled signed-distance shapes) into
//! `fennec-{16..256}.png`, `fennec.ico` and the monochrome tray variants, so the
//! vector and bitmap versions cannot drift apart.
//!
//! Design (SPEC ch.0 §1.2): squircle background (radius 22%) with a slate gradient,
//! a rust-orange lens ring (stroke 12% of its diameter) with two large ears and a
//! 45° handle, cream eyes and nose. Tray variants: lines only, 16% stroke, no face.

use std::io::Write;
use std::path::{Path, PathBuf};

const ORANGE: [u8; 4] = [0xE8, 0x76, 0x2B, 0xFF];
const CREAM: [u8; 4] = [0xF6, 0xE7, 0xD2, 0xFF];
const BG_A: [u8; 4] = [0x1B, 0x22, 0x30, 0xFF];
const BG_B: [u8; 4] = [0x24, 0x2C, 0x3B, 0xFF];
const WHITE: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];

#[derive(Clone, Copy)]
enum Paint {
    Solid([u8; 4]),
    /// Diagonal gradient from top-left (`a`) to bottom-right (`b`).
    Diagonal([u8; 4], [u8; 4]),
}

#[derive(Clone)]
enum Shape {
    RoundRect {
        size: f32,
        radius: f32,
    },
    Ring {
        cx: f32,
        cy: f32,
        r: f32,
        width: f32,
    },
    Disc {
        cx: f32,
        cy: f32,
        r: f32,
    },
    Capsule {
        a: (f32, f32),
        b: (f32, f32),
        width: f32,
    },
    /// Triangle outline with round joins (and optionally filled with `fill`).
    Polyline {
        pts: Vec<(f32, f32)>,
        width: f32,
        closed: bool,
    },
    Triangle {
        pts: [(f32, f32); 3],
    },
    /// Arc of a ring from `start` to `end` degrees (clockwise from +x).
    Arc {
        cx: f32,
        cy: f32,
        r: f32,
        width: f32,
        start: f32,
        end: f32,
    },
}

struct Layer {
    shape: Shape,
    paint: Paint,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Variant {
    /// Full-colour application icon.
    App,
    /// Monochrome orange lines (tray).
    TrayOrange,
    /// Monochrome white lines (tray, dark taskbar).
    TrayWhite,
    /// Tray while indexing: small arc at the handle end.
    TrayIndexing,
}

const LENS: (f32, f32, f32) = (124.0, 136.0, 50.0);

/// `boost` thickens strokes for tiny sizes so lines stay at least ~1.5 px wide.
fn layers(v: Variant, boost: f32) -> Vec<Layer> {
    let (cx, cy, r) = LENS;
    let mono = v != Variant::App;
    let line = match v {
        Variant::TrayWhite => WHITE,
        _ => ORANGE,
    };
    // Ring stroke: 12% of the lens diameter (16% for monochrome variants).
    let ring_w = if mono { 0.16 * 2.0 * r } else { 0.12 * 2.0 * r } * boost;
    let ear_w = ring_w * 0.85;
    let mut out = Vec::new();
    if !mono {
        out.push(Layer { shape: Shape::RoundRect { size: 256.0, radius: 56.0 }, paint: Paint::Diagonal(BG_A, BG_B) });
    }
    // Ears rise from the top of the ring.
    let left = vec![(cx - 40.0, cy - 28.0), (cx - 58.0, cy - 102.0), (cx - 6.0, cy - 50.0)];
    let right = vec![(cx + 40.0, cy - 28.0), (cx + 58.0, cy - 102.0), (cx + 6.0, cy - 50.0)];
    out.push(Layer { shape: Shape::Polyline { pts: left, width: ear_w, closed: false }, paint: Paint::Solid(line) });
    out.push(Layer { shape: Shape::Polyline { pts: right, width: ear_w, closed: false }, paint: Paint::Solid(line) });
    out.push(Layer { shape: Shape::Ring { cx, cy, r, width: ring_w }, paint: Paint::Solid(line) });
    // Handle at 45° to the lower right.
    let d = std::f32::consts::FRAC_1_SQRT_2;
    let start = (cx + (r + ring_w * 0.3) * d, cy + (r + ring_w * 0.3) * d);
    let end = (cx + (r + 52.0) * d, cy + (r + 52.0) * d);
    let handle_w = if mono { ring_w * 1.25 } else { 20.0 * boost };
    out.push(Layer { shape: Shape::Capsule { a: start, b: end, width: handle_w }, paint: Paint::Solid(line) });
    if v == Variant::TrayIndexing {
        out.push(Layer {
            shape: Shape::Arc { cx: end.0, cy: end.1, r: 26.0, width: 10.0, start: 200.0, end: 340.0 },
            paint: Paint::Solid(line),
        });
    }
    if !mono {
        out.push(Layer { shape: Shape::Disc { cx: cx - 16.0, cy: cy - 8.0, r: 5.5 }, paint: Paint::Solid(CREAM) });
        out.push(Layer { shape: Shape::Disc { cx: cx + 16.0, cy: cy - 8.0, r: 5.5 }, paint: Paint::Solid(CREAM) });
        out.push(Layer {
            shape: Shape::Triangle { pts: [(cx - 8.0, cy + 10.0), (cx + 8.0, cy + 10.0), (cx, cy + 19.0)] },
            paint: Paint::Solid(CREAM),
        });
    }
    out
}

fn seg_dist(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (px, py) = (p.0 - a.0, p.1 - a.1);
    let (bx, by) = (b.0 - a.0, b.1 - a.1);
    let t = ((px * bx + py * by) / (bx * bx + by * by).max(1e-6)).clamp(0.0, 1.0);
    let (dx, dy) = (px - bx * t, py - by * t);
    (dx * dx + dy * dy).sqrt()
}

fn inside(shape: &Shape, x: f32, y: f32) -> bool {
    match shape {
        Shape::RoundRect { size, radius } => {
            let cx = x.clamp(*radius, size - radius);
            let cy = y.clamp(*radius, size - radius);
            (0.0..=*size).contains(&x) && (0.0..=*size).contains(&y) && (x - cx).hypot(y - cy) <= *radius
        }
        Shape::Ring { cx, cy, r, width } => ((x - cx).hypot(y - cy) - r).abs() <= width / 2.0,
        Shape::Disc { cx, cy, r } => (x - cx).hypot(y - cy) <= *r,
        Shape::Capsule { a, b, width } => seg_dist((x, y), *a, *b) <= width / 2.0,
        Shape::Polyline { pts, width, closed } => {
            let n = pts.len();
            let segs = if *closed { n } else { n - 1 };
            (0..segs).any(|i| seg_dist((x, y), pts[i], pts[(i + 1) % n]) <= width / 2.0)
        }
        Shape::Triangle { pts } => {
            let s = |a: (f32, f32), b: (f32, f32)| (b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0);
            let (d1, d2, d3) = (s(pts[0], pts[1]), s(pts[1], pts[2]), s(pts[2], pts[0]));
            let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
            let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
            !(neg && pos)
        }
        Shape::Arc { cx, cy, r, width, start, end } => {
            let ang = (y - cy).atan2(x - cx).to_degrees().rem_euclid(360.0);
            ((x - cx).hypot(y - cy) - r).abs() <= width / 2.0 && ang >= *start && ang <= *end
        }
    }
}

fn color_at(p: Paint, x: f32, y: f32) -> [f32; 4] {
    let f = |c: [u8; 4]| [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, c[3] as f32 / 255.0];
    match p {
        Paint::Solid(c) => f(c),
        Paint::Diagonal(a, b) => {
            let t = ((x + y) / 512.0).clamp(0.0, 1.0);
            let (a, b) = (f(a), f(b));
            [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, 1.0]
        }
    }
}

/// Renders `variant` at `size`×`size` (RGBA8, straight alpha). `opacity` scales alpha.
fn render(variant: Variant, size: u32, opacity: f32) -> Vec<u8> {
    const SS: u32 = 4;
    let boost = match size {
        0..=24 => 1.6,
        25..=32 => 1.25,
        _ => 1.0,
    };
    let ls = layers(variant, boost);
    let scale = 256.0 / size as f32;
    let mut out = vec![0u8; (size * size * 4) as usize];
    for py in 0..size {
        for px in 0..size {
            let mut acc = [0f32; 4]; // premultiplied
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) * scale;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) * scale;
                    let mut c = [0f32; 4];
                    for l in &ls {
                        if inside(&l.shape, x, y) {
                            let s = color_at(l.paint, x, y);
                            // `over` compositing in premultiplied space.
                            let a = s[3];
                            for k in 0..3 {
                                c[k] = s[k] * a + c[k] * (1.0 - a);
                            }
                            c[3] = a + c[3] * (1.0 - a);
                        }
                    }
                    for k in 0..4 {
                        acc[k] += c[k];
                    }
                }
            }
            let n = (SS * SS) as f32;
            let a = acc[3] / n;
            let i = ((py * size + px) * 4) as usize;
            if a > 0.0 {
                for k in 0..3 {
                    out[i + k] = ((acc[k] / n / a) * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
            out[i + 3] = (a * opacity * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

fn encode_png(rgba: &[u8], size: u32) -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, size, size);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header()?;
        w.write_image_data(rgba)?;
    }
    Ok(buf)
}

/// ICO container with PNG-compressed images (supported since Windows Vista).
fn encode_ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len();
    for (size, png) in images {
        let dim = if *size >= 256 { 0u8 } else { *size as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += png.len();
    }
    for (_, png) in images {
        out.extend_from_slice(png);
    }
    out
}

fn svg(variant: Variant) -> String {
    let hex = |c: [u8; 4]| format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]);
    let mut s = String::from(
        "<svg viewBox=\"0 0 256 256\" xmlns=\"http://www.w3.org/2000/svg\">\n  <!-- Generated by tools/gen-icons; edit the geometry there. -->\n",
    );
    s.push_str(&format!(
        "  <defs><linearGradient id=\"bg\" x1=\"0\" y1=\"0\" x2=\"1\" y2=\"1\">\n    <stop offset=\"0\" stop-color=\"{}\"/><stop offset=\"1\" stop-color=\"{}\"/></linearGradient></defs>\n",
        hex(BG_A),
        hex(BG_B)
    ));
    for l in layers(variant, 1.0) {
        let paint = match l.paint {
            Paint::Solid(c) => hex(c),
            Paint::Diagonal(..) => "url(#bg)".into(),
        };
        let line = match l.shape {
            Shape::RoundRect { size, radius } => {
                format!("<rect width=\"{size}\" height=\"{size}\" rx=\"{radius}\" fill=\"{paint}\"/>")
            }
            Shape::Ring { cx, cy, r, width } => {
                format!(
                    "<circle cx=\"{cx}\" cy=\"{cy}\" r=\"{r}\" fill=\"none\" stroke=\"{paint}\" stroke-width=\"{width}\"/>"
                )
            }
            Shape::Disc { cx, cy, r } => format!("<circle cx=\"{cx}\" cy=\"{cy}\" r=\"{r}\" fill=\"{paint}\"/>"),
            Shape::Capsule { a, b, width } => format!(
                "<path d=\"M{:.1} {:.1} L{:.1} {:.1}\" stroke=\"{paint}\" stroke-width=\"{width}\" stroke-linecap=\"round\"/>",
                a.0, a.1, b.0, b.1
            ),
            Shape::Polyline { pts, width, closed } => {
                let mut d = String::new();
                for (i, p) in pts.iter().enumerate() {
                    d.push_str(&format!("{}{:.1} {:.1} ", if i == 0 { "M" } else { "L" }, p.0, p.1));
                }
                if closed {
                    d.push('Z');
                }
                format!(
                    "<path d=\"{}\" fill=\"none\" stroke=\"{paint}\" stroke-width=\"{width:.1}\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>",
                    d.trim_end()
                )
            }
            Shape::Triangle { pts } => format!(
                "<path d=\"M{:.1} {:.1} L{:.1} {:.1} L{:.1} {:.1} Z\" fill=\"{paint}\"/>",
                pts[0].0, pts[0].1, pts[1].0, pts[1].1, pts[2].0, pts[2].1
            ),
            Shape::Arc { cx, cy, r, width, start, end } => {
                let p = |deg: f32| (cx + r * deg.to_radians().cos(), cy + r * deg.to_radians().sin());
                let (a, b) = (p(start), p(end));
                format!(
                    "<path d=\"M{:.1} {:.1} A{r} {r} 0 0 1 {:.1} {:.1}\" fill=\"none\" stroke=\"{paint}\" stroke-width=\"{width}\" stroke-linecap=\"round\"/>",
                    a.0, a.1, b.0, b.1
                )
            }
        };
        s.push_str("  ");
        s.push_str(&line);
        s.push('\n');
    }
    s.push_str("</svg>\n");
    s
}

fn write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(bytes)?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let out: PathBuf = std::env::args().nth(1).map_or_else(|| PathBuf::from("assets/icon"), PathBuf::from);
    std::fs::create_dir_all(&out)?;
    write(&out.join("fennec.svg"), svg(Variant::App).as_bytes())?;
    write(&out.join("fennec-tray.svg"), svg(Variant::TrayOrange).as_bytes())?;
    let mut ico = Vec::new();
    for size in [16u32, 20, 24, 32, 48, 64, 128, 256] {
        let png = encode_png(&render(Variant::App, size, 1.0), size)?;
        write(&out.join(format!("fennec-{size}.png")), &png)?;
        ico.push((size, png));
    }
    write(&out.join("fennec.ico"), &encode_ico(&ico))?;
    write(&out.join("fennec-512.png"), &encode_png(&render(Variant::App, 512, 1.0), 512)?)?;
    for (name, variant, opacity) in [
        ("tray", Variant::TrayOrange, 1.0),
        ("tray-white", Variant::TrayWhite, 1.0),
        ("tray-indexing", Variant::TrayIndexing, 1.0),
        ("tray-paused", Variant::TrayOrange, 0.6),
    ] {
        for size in [16u32, 32] {
            write(&out.join(format!("fennec-{name}-{size}.png")), &encode_png(&render(variant, size, opacity), size)?)?;
        }
    }
    eprintln!("icons written to {}", out.display());
    Ok(())
}

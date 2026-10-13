//! Video containers: MP4/MOV/3GP (`mp4parse` on the `moov` box) and Matroska/WebM (a
//! minimal EBML reader for the segment info and track headers).

use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use fnc_core::FileTime;

use super::{ExtractLimits, Props, open, push_text};
use crate::schema::PropertyId as P;
use crate::value::Value;

/// Largest `moov` / EBML master element read into memory.
const MAX_HEADER_BOX: u64 = 32 << 20;

fn codec_name(fourcc: &[u8]) -> Option<&'static str> {
    Some(match fourcc {
        b"avc1" | b"avc3" => "H.264",
        b"hvc1" | b"hev1" => "HEVC",
        b"av01" => "AV1",
        b"vp09" => "VP9",
        b"vp08" => "VP8",
        b"mp4v" => "MPEG-4 Visual",
        b"s263" | b"h263" => "H.263",
        b"apcn" | b"apch" | b"apcs" | b"apco" | b"ap4h" => "ProRes",
        b"jpeg" | b"mjpa" => "Motion JPEG",
        b"mp4a" => "AAC",
        b"ac-3" => "AC-3",
        b"ec-3" => "E-AC-3",
        b"Opus" => "Opus",
        b"fLaC" => "FLAC",
        b"alac" => "ALAC",
        b".mp3" => "MP3",
        b"lpcm" | b"sowt" | b"twos" => "PCM",
        _ => return None,
    })
}

fn video_fourcc(f: &[u8]) -> bool {
    matches!(
        f,
        b"avc1"
            | b"avc3"
            | b"hvc1"
            | b"hev1"
            | b"av01"
            | b"vp09"
            | b"vp08"
            | b"mp4v"
            | b"s263"
            | b"h263"
            | b"apcn"
            | b"apch"
            | b"apcs"
            | b"apco"
            | b"ap4h"
            | b"jpeg"
            | b"mjpa"
    )
}

/// Reads the `ftyp` and `moov` top-level boxes (seeking over `mdat`).
fn header_boxes<R: Read + Seek>(r: &mut R) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut have_moov = false;
    for _ in 0..64 {
        let mut h = [0u8; 8];
        if r.read_exact(&mut h).is_err() {
            break;
        }
        let mut size = u64::from(u32::from_be_bytes([h[0], h[1], h[2], h[3]]));
        let typ = [h[4], h[5], h[6], h[7]];
        let mut header_len = 8u64;
        if size == 1 {
            let mut l = [0u8; 8];
            r.read_exact(&mut l).ok()?;
            size = u64::from_be_bytes(l);
            header_len = 16;
        }
        if size != 0 && size < header_len {
            return None;
        }
        if &typ == b"ftyp" || &typ == b"moov" {
            if size == 0 || size > MAX_HEADER_BOX {
                return None;
            }
            let body_len = size - header_len;
            out.extend_from_slice(&((body_len + 8) as u32).to_be_bytes());
            out.extend_from_slice(&typ);
            let start = out.len();
            out.resize(start + body_len as usize, 0);
            r.read_exact(&mut out[start..]).ok()?;
            if &typ == b"moov" {
                have_moov = true;
                break;
            }
        } else {
            if size == 0 {
                break;
            }
            r.seek(SeekFrom::Current(i64::try_from(size - header_len).ok()?)).ok()?;
        }
    }
    have_moov.then_some(out)
}

/// `(creation time, duration in ms)` from the `mvhd` box.
fn mvhd(moov: &[u8]) -> (Option<FileTime>, Option<i64>) {
    let Some(p) = memchr::memmem::find(moov, b"mvhd") else { return (None, None) };
    let b = &moov[p + 4..];
    let be32 = |o: usize| b.get(o..o + 4).map(|s| u64::from(u32::from_be_bytes([s[0], s[1], s[2], s[3]])));
    let be64 = |o: usize| b.get(o..o + 8).map(|s| u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]));
    let (created, timescale, duration) = match b.first() {
        Some(1) => (be64(4), be32(20), be64(24)),
        Some(0) => (be32(4), be32(12), be32(16)),
        _ => return (None, None),
    };
    // Seconds since 1904-01-01 UTC.
    const MAC_EPOCH: i64 = -2_082_844_800;
    let created = created
        .filter(|&c| c > 0)
        .and_then(|c| i64::try_from(c).ok())
        .map(|c| FileTime::from_unix(MAC_EPOCH.saturating_add(c), 0));
    let ms = match (timescale, duration) {
        (Some(ts), Some(d)) if ts > 0 && d > 0 && d != u64::from(u32::MAX) && d != u64::MAX => {
            i64::try_from(u128::from(d) * 1000 / u128::from(ts)).ok()
        }
        _ => None,
    };
    (created, ms)
}

/// Sample-entry FourCCs in `stsd` boxes, in track order.
fn stsd_fourccs(moov: &[u8]) -> Vec<[u8; 4]> {
    memchr::memmem::find_iter(moov, b"stsd")
        .filter_map(|p| moov.get(p + 16..p + 20))
        .map(|s| [s[0], s[1], s[2], s[3]])
        .collect()
}

/// MP4/MOV properties.
pub fn extract_mp4(path: &Path, limits: ExtractLimits, out: &mut Props) {
    let Ok(mut r) = open(path, limits) else { return };
    let file_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let Some(boxes) = header_boxes(&mut r) else { return };
    mp4_from_boxes(&boxes, file_len, out);
}

fn mp4_from_boxes(boxes: &[u8], file_len: u64, out: &mut Props) {
    let (created, movie_ms) = mvhd(boxes);
    if let Some(t) = created {
        out.push((P::DATE_ENCODED, Value::Date(t)));
    }
    let fourccs = stsd_fourccs(boxes);
    let ctx = mp4parse::read_mp4(&mut Cursor::new(boxes)).ok();
    let mut duration_ms = movie_ms;
    let mut streams = 0i64;
    let mut video_done = false;
    if let Some(ctx) = &ctx {
        for t in ctx.tracks.iter() {
            streams += 1;
            let secs = match (t.duration, t.timescale) {
                (Some(d), Some(ts)) if ts.0 > 0 => Some(d.0 as f64 / ts.0 as f64),
                _ => None,
            };
            if duration_ms.is_none()
                && let Some(s) = secs
            {
                duration_ms = Some((s * 1000.0).round() as i64);
            }
            if t.track_type == mp4parse::TrackType::Video && !video_done {
                video_done = true;
                let (mut w, mut h) = (0u32, 0u32);
                if let Some(mp4parse::SampleEntry::Video(v)) = t.stsd.as_ref().and_then(|s| s.descriptions.first()) {
                    w = u32::from(v.width);
                    h = u32::from(v.height);
                }
                if (w == 0 || h == 0)
                    && let Some(tk) = &t.tkhd
                {
                    w = tk.width >> 16;
                    h = tk.height >> 16;
                }
                if w > 0 && h > 0 {
                    out.push((P::FRAME_WIDTH, Value::Int(i64::from(w))));
                    out.push((P::FRAME_HEIGHT, Value::Int(i64::from(h))));
                }
                let frames: u64 =
                    t.stts.as_ref().map_or(0, |stts| stts.samples.iter().map(|x| u64::from(x.sample_count)).sum());
                if let Some(s) = secs.filter(|s| *s > 0.0)
                    && frames > 1
                {
                    out.push((P::FRAME_RATE, Value::Float(((frames as f64 / s) * 1000.0).round() / 1000.0)));
                }
                if let (Some(stsz), Some(s)) = (&t.stsz, secs.filter(|s| *s > 0.0)) {
                    let bytes: u64 = if stsz.sample_size > 0 {
                        u64::from(stsz.sample_size) * frames
                    } else {
                        stsz.sample_sizes.iter().map(|&x| u64::from(x)).sum()
                    };
                    if bytes > 0 {
                        out.push((P::VIDEO_BITRATE, Value::Int((bytes as f64 * 8.0 / s / 1000.0).round() as i64)));
                    }
                }
            }
            if t.track_type == mp4parse::TrackType::Audio
                && let Some(mp4parse::SampleEntry::Audio(a)) = t.stsd.as_ref().and_then(|s| s.descriptions.first())
            {
                if a.channelcount > 0 {
                    out.push((P::CHANNELS, Value::Int(i64::from(a.channelcount))));
                }
                if a.samplerate > 0.0 {
                    out.push((P::SAMPLE_RATE, Value::Int(a.samplerate.round() as i64)));
                }
            }
        }
    } else {
        streams = fourccs.len() as i64;
    }
    if let Some(f) = fourccs.iter().find(|f| video_fourcc(&f[..])) {
        push_text(out, P::FOUR_CC, &String::from_utf8_lossy(f));
        if let Some(n) = codec_name(f) {
            push_text(out, P::VIDEO_CODEC, n);
            push_text(out, P::COMPRESSION, n);
        }
    }
    if let Some(n) = fourccs.iter().filter(|f| !video_fourcc(&f[..])).find_map(|f| codec_name(f)) {
        push_text(out, P::AUDIO_CODEC, n);
    }
    if streams > 0 {
        out.push((P::STREAM_COUNT, Value::Int(streams)));
    }
    if let Some(ms) = duration_ms.filter(|ms| *ms > 0) {
        out.push((P::LENGTH, Value::Duration(ms)));
        if file_len > 0 {
            out.push((P::TOTAL_BITRATE, Value::Int((file_len as f64 * 8.0 / ms as f64).round() as i64)));
        }
    }
}

// ------------------------------------------------------------------ Matroska

/// EBML variable-length integer; `strip` removes the length marker (sizes).
fn vint<R: Read>(r: &mut R, strip: bool) -> Option<(u64, usize)> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b).ok()?;
    let first = b[0];
    let len = first.leading_zeros() as usize + 1;
    if len > 8 {
        return None;
    }
    let mut v = u64::from((u16::from(first) & (0xffu16 >> len)) as u8);
    if !strip {
        v = u64::from(first);
    }
    for _ in 1..len {
        r.read_exact(&mut b).ok()?;
        v = (v << 8) | u64::from(b[0]);
    }
    Some((v, len))
}

/// "Unknown size" marker for a size of `len` bytes.
fn unknown_size(v: u64, len: usize) -> bool {
    v == (1u64 << (7 * len)) - 1
}

fn uint(b: &[u8]) -> u64 {
    b.iter().take(8).fold(0, |a, &x| (a << 8) | u64::from(x))
}

fn float(b: &[u8]) -> Option<f64> {
    match b.len() {
        4 => Some(f64::from(f32::from_be_bytes([b[0], b[1], b[2], b[3]]))),
        8 => Some(f64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])),
        _ => None,
    }
}

/// Children `(id, payload)` of a master element held in memory.
fn children(mut b: &[u8]) -> Vec<(u64, &[u8])> {
    let mut out = Vec::new();
    while !b.is_empty() {
        let mut c = Cursor::new(b);
        let Some((id, _)) = vint(&mut c, false) else { break };
        let Some((size, slen)) = vint(&mut c, true) else { break };
        let start = c.position() as usize;
        let len = if unknown_size(size, slen) { b.len() - start } else { size as usize };
        let Some(payload) = b.get(start..start.saturating_add(len)) else { break };
        out.push((id, payload));
        b = &b[start + len..];
    }
    out
}

#[derive(Default)]
struct Mkv {
    timescale: Option<u64>,
    duration: Option<f64>,
    date: Option<i64>,
    title: Option<String>,
    tracks: Vec<MkvTrack>,
}

#[derive(Default)]
struct MkvTrack {
    kind: u64,
    codec: String,
    default_duration: Option<u64>,
    width: Option<u64>,
    height: Option<u64>,
    rate: Option<f64>,
    channels: Option<u64>,
    bits: Option<u64>,
}

fn mkv_info(m: &mut Mkv, b: &[u8]) {
    for (id, p) in children(b) {
        match id {
            0x2AD7B1 => m.timescale = Some(uint(p)),
            0x4489 => m.duration = float(p),
            0x4461 if p.len() == 8 => {
                m.date = Some(i64::from_be_bytes([p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7]]))
            }
            0x7BA9 => m.title = Some(String::from_utf8_lossy(p).into_owned()),
            _ => {}
        }
    }
}

fn mkv_tracks(m: &mut Mkv, b: &[u8]) {
    for (id, entry) in children(b) {
        if id != 0xAE {
            continue;
        }
        let mut t = MkvTrack::default();
        for (cid, p) in children(entry) {
            match cid {
                0x83 => t.kind = uint(p),
                0x86 => t.codec = String::from_utf8_lossy(p).trim_end_matches('\0').to_string(),
                0x23E383 => t.default_duration = Some(uint(p)),
                0xE0 => {
                    for (vid, v) in children(p) {
                        match vid {
                            0xB0 => t.width = Some(uint(v)),
                            0xBA => t.height = Some(uint(v)),
                            _ => {}
                        }
                    }
                }
                0xE1 => {
                    for (aid, a) in children(p) {
                        match aid {
                            0xB5 => t.rate = float(a),
                            0x9F => t.channels = Some(uint(a)),
                            0x6264 => t.bits = Some(uint(a)),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        m.tracks.push(t);
    }
}

fn mkv_codec(id: &str) -> Option<&'static str> {
    Some(match id {
        "V_MPEG4/ISO/AVC" => "H.264",
        "V_MPEGH/ISO/HEVC" => "HEVC",
        "V_VP9" => "VP9",
        "V_VP8" => "VP8",
        "V_AV1" => "AV1",
        "V_MPEG4/ISO/ASP" | "V_MPEG4/ISO/SP" => "MPEG-4 Visual",
        "V_MPEG2" => "MPEG-2",
        "V_THEORA" => "Theora",
        "A_AAC" | "A_AAC/MPEG4/LC" | "A_AAC/MPEG2/LC" => "AAC",
        "A_OPUS" => "Opus",
        "A_VORBIS" => "Vorbis",
        "A_FLAC" => "FLAC",
        "A_AC3" => "AC-3",
        "A_EAC3" => "E-AC-3",
        "A_DTS" => "DTS",
        "A_MPEG/L3" => "MP3",
        "A_PCM/INT/LIT" | "A_PCM/INT/BIG" => "PCM",
        _ => return None,
    })
}

/// Matroska/WebM properties.
pub fn extract_matroska(path: &Path, limits: ExtractLimits, out: &mut Props) {
    let Ok(mut r) = open(path, limits) else { return };
    let file_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if let Some(m) = read_mkv(&mut r) {
        mkv_props(&m, file_len, out);
    }
}

fn read_mkv<R: Read + Seek>(r: &mut R) -> Option<Mkv> {
    let (id, _) = vint(r, false)?;
    if id != 0x1A45DFA3 {
        return None;
    }
    let (size, _) = vint(r, true)?;
    r.seek(SeekFrom::Current(i64::try_from(size).ok()?)).ok()?;
    let (seg, _) = vint(r, false)?;
    if seg != 0x18538067 {
        return None;
    }
    vint(r, true)?;
    let mut m = Mkv::default();
    let (mut have_info, mut have_tracks) = (false, false);
    for _ in 0..256 {
        let Some((id, _)) = vint(r, false) else { break };
        let Some((size, slen)) = vint(r, true) else { break };
        let unknown = unknown_size(size, slen);
        match id {
            0x1549A966 | 0x1654AE6B if !unknown && size <= MAX_HEADER_BOX => {
                let mut buf = vec![0u8; size as usize];
                r.read_exact(&mut buf).ok()?;
                if id == 0x1549A966 {
                    mkv_info(&mut m, &buf);
                    have_info = true;
                } else {
                    mkv_tracks(&mut m, &buf);
                    have_tracks = true;
                }
            }
            0x1F43B675 if have_info && have_tracks => break,
            _ => {
                if unknown {
                    break;
                }
                r.seek(SeekFrom::Current(i64::try_from(size).ok()?)).ok()?;
            }
        }
        if have_info && have_tracks {
            break;
        }
    }
    (have_info || have_tracks).then_some(m)
}

fn mkv_props(m: &Mkv, file_len: u64, out: &mut Props) {
    let scale = m.timescale.unwrap_or(1_000_000) as f64;
    let ms = m.duration.map(|d| (d * scale / 1e6).round() as i64).filter(|&ms| ms > 0);
    if let Some(ms) = ms {
        out.push((P::LENGTH, Value::Duration(ms)));
        if file_len > 0 {
            out.push((P::TOTAL_BITRATE, Value::Int((file_len as f64 * 8.0 / ms as f64).round() as i64)));
        }
    }
    if let Some(ns) = m.date {
        // Nanoseconds since 2001-01-01 UTC.
        out.push((P::DATE_ENCODED, Value::Date(FileTime::from_unix(978_307_200 + ns.div_euclid(1_000_000_000), 0))));
    }
    if let Some(t) = &m.title {
        push_text(out, P::TITLE, t);
    }
    if !m.tracks.is_empty() {
        out.push((P::STREAM_COUNT, Value::Int(m.tracks.len() as i64)));
    }
    if let Some(v) = m.tracks.iter().find(|t| t.kind == 1) {
        if let (Some(w), Some(h)) = (v.width, v.height) {
            out.push((P::FRAME_WIDTH, Value::Int(w as i64)));
            out.push((P::FRAME_HEIGHT, Value::Int(h as i64)));
        }
        if let Some(d) = v.default_duration.filter(|d| *d > 0) {
            out.push((P::FRAME_RATE, Value::Float((1e9 / d as f64 * 1000.0).round() / 1000.0)));
        }
        if let Some(n) = mkv_codec(&v.codec) {
            push_text(out, P::VIDEO_CODEC, n);
            push_text(out, P::COMPRESSION, n);
        }
    }
    if let Some(a) = m.tracks.iter().find(|t| t.kind == 2) {
        if let Some(n) = mkv_codec(&a.codec) {
            push_text(out, P::AUDIO_CODEC, n);
        }
        if let Some(r) = a.rate.filter(|r| *r > 0.0) {
            out.push((P::SAMPLE_RATE, Value::Int(r.round() as i64)));
        }
        if let Some(c) = a.channels.filter(|c| *c > 0) {
            out.push((P::CHANNELS, Value::Int(c as i64)));
        }
        if let Some(b) = a.bits.filter(|b| *b > 0) {
            out.push((P::BITS_PER_SAMPLE, Value::Int(b as i64)));
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::extract::testutil::file;

    fn mbox(typ: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(typ);
        v.extend_from_slice(body);
        v
    }

    fn full(typ: &[u8; 4], version: u8, body: &[u8]) -> Vec<u8> {
        let mut b = vec![version, 0, 0, 0];
        b.extend_from_slice(body);
        mbox(typ, &b)
    }

    /// MP4 with one 1280×720 video track: 250 frames at 25 fps (10 s), `mdat` before
    /// `moov` (camera layout).
    pub(crate) fn mp4() -> Vec<u8> {
        let be = |v: u32| v.to_be_bytes();
        let ftyp = mbox(b"ftyp", b"isom\0\0\x02\0isomiso2mp41");
        let mut mvhd = Vec::new();
        mvhd.extend_from_slice(&be(3_800_000_000)); // created (2024)
        mvhd.extend_from_slice(&be(3_800_000_000));
        mvhd.extend_from_slice(&be(1000)); // timescale
        mvhd.extend_from_slice(&be(10_000)); // duration
        mvhd.extend_from_slice(&be(0x0001_0000));
        mvhd.extend_from_slice(&[1, 0]);
        mvhd.extend_from_slice(&[0; 10]);
        for m in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
            mvhd.extend_from_slice(&be(m));
        }
        mvhd.extend_from_slice(&[0; 24]);
        mvhd.extend_from_slice(&be(2));
        let mvhd = full(b"mvhd", 0, &mvhd);
        let mut tkhd = Vec::new();
        tkhd.extend_from_slice(&be(0));
        tkhd.extend_from_slice(&be(0));
        tkhd.extend_from_slice(&be(1)); // track id
        tkhd.extend_from_slice(&be(0));
        tkhd.extend_from_slice(&be(10_000));
        tkhd.extend_from_slice(&[0; 16]);
        for m in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
            tkhd.extend_from_slice(&be(m));
        }
        tkhd.extend_from_slice(&be(1280 << 16));
        tkhd.extend_from_slice(&be(720 << 16));
        let mut tkhd_box = vec![0, 0, 0, 3];
        tkhd_box.extend_from_slice(&tkhd);
        let tkhd = mbox(b"tkhd", &tkhd_box);
        let mut mdhd = Vec::new();
        mdhd.extend_from_slice(&be(0));
        mdhd.extend_from_slice(&be(0));
        mdhd.extend_from_slice(&be(25_000));
        mdhd.extend_from_slice(&be(250_000));
        mdhd.extend_from_slice(&[0x55, 0xc4, 0, 0]);
        let mdhd = full(b"mdhd", 0, &mdhd);
        let mut hdlr = vec![0; 4];
        hdlr.extend_from_slice(b"vide");
        hdlr.extend_from_slice(&[0; 12]);
        hdlr.extend_from_slice(b"Video\0");
        let hdlr = full(b"hdlr", 0, &hdlr);
        // Visual sample entry without codec configuration: mp4parse keeps it as
        // `Unknown`, so the size comes from `tkhd`.
        let mut entry = vec![0; 6];
        entry.extend_from_slice(&[0, 1]);
        entry.extend_from_slice(&[0; 16]);
        entry.extend_from_slice(&1280u16.to_be_bytes());
        entry.extend_from_slice(&720u16.to_be_bytes());
        entry.extend_from_slice(&[0; 50]);
        let mut stsd = be(1).to_vec();
        stsd.extend_from_slice(&mbox(b"hvc1", &entry));
        let stsd = full(b"stsd", 0, &stsd);
        let mut stts = be(1).to_vec();
        stts.extend_from_slice(&be(250));
        stts.extend_from_slice(&be(1000));
        let stts = full(b"stts", 0, &stts);
        let mut stsz = be(4000).to_vec();
        stsz.extend_from_slice(&be(250));
        let stsz = full(b"stsz", 0, &stsz);
        let stsc = full(b"stsc", 0, &be(0));
        let stco = full(b"stco", 0, &be(0));
        let stbl = mbox(b"stbl", &[stsd, stts, stsc, stsz, stco].concat());
        let minf = mbox(b"minf", &stbl);
        let mdia = mbox(b"mdia", &[mdhd, hdlr, minf].concat());
        let trak = mbox(b"trak", &[tkhd, mdia].concat());
        let moov = mbox(b"moov", &[mvhd, trak].concat());
        let mdat = mbox(b"mdat", &vec![0u8; 4096]);
        [ftyp, mdat, moov].concat()
    }

    fn ebml(id: &[u8], body: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.push(0x01); // 8-byte size
        v.extend_from_slice(&(body.len() as u64).to_be_bytes()[1..]);
        v.extend_from_slice(body);
        v
    }

    /// WebM: 1920×1080 VP9 at 30 fps + Opus 48 kHz stereo, 90 s.
    pub(crate) fn webm() -> Vec<u8> {
        let header = ebml(&[0x1A, 0x45, 0xDF, 0xA3], &ebml(&[0x42, 0x82], b"webm"));
        let info = ebml(
            &[0x15, 0x49, 0xA9, 0x66],
            &[
                ebml(&[0x2A, 0xD7, 0xB1], &1_000_000u32.to_be_bytes()),
                ebml(&[0x44, 0x89], &90_000.0f64.to_be_bytes()),
                ebml(&[0x7B, 0xA9], b"Trip"),
            ]
            .concat(),
        );
        let video = ebml(
            &[0xAE],
            &[
                ebml(&[0x83], &[1]),
                ebml(&[0x86], b"V_VP9"),
                ebml(&[0x23, 0xE3, 0x83], &33_333_333u32.to_be_bytes()),
                ebml(&[0xE0], &[ebml(&[0xB0], &1920u16.to_be_bytes()), ebml(&[0xBA], &1080u16.to_be_bytes())].concat()),
            ]
            .concat(),
        );
        let audio = ebml(
            &[0xAE],
            &[
                ebml(&[0x83], &[2]),
                ebml(&[0x86], b"A_OPUS"),
                ebml(&[0xE1], &[ebml(&[0xB5], &48_000.0f32.to_be_bytes()), ebml(&[0x9F], &[2])].concat()),
            ]
            .concat(),
        );
        let tracks = ebml(&[0x16, 0x54, 0xAE, 0x6B], &[video, audio].concat());
        let cluster = ebml(&[0x1F, 0x43, 0xB6, 0x75], &[0u8; 64]);
        let segment = ebml(&[0x18, 0x53, 0x80, 0x67], &[info, tracks, cluster].concat());
        [header, segment].concat()
    }

    fn get(v: &Props, p: P) -> Option<&Value> {
        v.iter().find(|(q, _)| *q == p).map(|(_, v)| v)
    }

    #[test]
    fn mp4_moov_after_mdat() {
        let (_d, p) = file("clip.mp4", &mp4());
        let boxes = header_boxes(&mut std::fs::File::open(&p).unwrap()).unwrap();
        if let Err(e) = mp4parse::read_mp4(&mut Cursor::new(&boxes)) {
            panic!("mp4parse: {e:?}");
        }
        let mut out = Props::new();
        extract_mp4(&p, ExtractLimits::default(), &mut out);
        assert_eq!(get(&out, P::FRAME_WIDTH), Some(&Value::Int(1280)));
        assert_eq!(get(&out, P::FRAME_HEIGHT), Some(&Value::Int(720)));
        assert_eq!(get(&out, P::LENGTH), Some(&Value::Duration(10_000)));
        assert_eq!(get(&out, P::FRAME_RATE), Some(&Value::Float(25.0)));
        assert_eq!(get(&out, P::VIDEO_BITRATE), Some(&Value::Int(800)));
        assert_eq!(get(&out, P::FOUR_CC), Some(&Value::Text("hvc1".into())));
        assert_eq!(get(&out, P::VIDEO_CODEC), Some(&Value::Text("HEVC".into())));
        assert_eq!(get(&out, P::STREAM_COUNT), Some(&Value::Int(1)));
        assert!(
            matches!(get(&out, P::DATE_ENCODED), Some(Value::Date(d)) if d.to_utc().is_some_and(|t| t.format("%Y").to_string() == "2024"))
        );
    }

    #[test]
    fn webm_tracks() {
        let (_d, p) = file("trip.webm", &webm());
        let mut out = Props::new();
        extract_matroska(&p, ExtractLimits::default(), &mut out);
        assert_eq!(get(&out, P::FRAME_WIDTH), Some(&Value::Int(1920)));
        assert_eq!(get(&out, P::FRAME_HEIGHT), Some(&Value::Int(1080)));
        assert_eq!(get(&out, P::LENGTH), Some(&Value::Duration(90_000)));
        assert_eq!(get(&out, P::FRAME_RATE), Some(&Value::Float(30.0)));
        assert_eq!(get(&out, P::VIDEO_CODEC), Some(&Value::Text("VP9".into())));
        assert_eq!(get(&out, P::AUDIO_CODEC), Some(&Value::Text("Opus".into())));
        assert_eq!(get(&out, P::SAMPLE_RATE), Some(&Value::Int(48_000)));
        assert_eq!(get(&out, P::CHANNELS), Some(&Value::Int(2)));
        assert_eq!(get(&out, P::TITLE), Some(&Value::Text("Trip".into())));
        assert_eq!(get(&out, P::STREAM_COUNT), Some(&Value::Int(2)));
    }

    #[test]
    fn truncated_containers_are_harmless() {
        for data in [mp4(), webm()] {
            for cut in (0..data.len()).step_by(7) {
                let mut out = Props::new();
                let mut c = Cursor::new(&data[..cut]);
                if let Some(b) = header_boxes(&mut c) {
                    mp4_from_boxes(&b, cut as u64, &mut out);
                }
                let mut c = Cursor::new(&data[..cut]);
                if let Some(m) = read_mkv(&mut c) {
                    mkv_props(&m, cut as u64, &mut out);
                }
            }
        }
    }
}

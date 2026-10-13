//! Audio tags and stream properties (`lofty`): MP3, FLAC, Ogg/Opus, MP4/M4A, WAV, AIFF,
//! APE, WavPack, Musepack.

use std::io::Read;
use std::path::Path;

use lofty::config::ParseOptions;
use lofty::prelude::*;
use lofty::probe::Probe;
use lofty::tag::Tag;

use super::{ExtractLimits, Props, open, push_text};
use crate::schema::PropertyId as P;
use crate::value::Value;

/// Tags and audio properties of the file at `path`.
pub fn extract(path: &Path, ext: &str, limits: ExtractLimits, out: &mut Props) {
    let Ok(r) = open(path, limits) else { return };
    let Ok(probe) = Probe::new(r).options(ParseOptions::new().read_cover_art(false)).guess_file_type() else { return };
    let Ok(tf) = probe.read() else { return };
    let props = tf.properties();
    let ms = props.duration().as_millis();
    if ms > 0 {
        out.push((P::LENGTH, Value::Duration(i64::try_from(ms).unwrap_or(i64::MAX))));
    }
    if let Some(b) = props.audio_bitrate().or(props.overall_bitrate()).filter(|b| *b > 0) {
        out.push((P::BIT_RATE, Value::Int(i64::from(b))));
    }
    if let Some(v) = props.sample_rate().filter(|v| *v > 0) {
        out.push((P::SAMPLE_RATE, Value::Int(i64::from(v))));
    }
    if let Some(v) = props.channels().filter(|v| *v > 0) {
        out.push((P::CHANNELS, Value::Int(i64::from(v))));
    }
    if let Some(v) = props.bit_depth().filter(|v| *v > 0) {
        out.push((P::BITS_PER_SAMPLE, Value::Int(i64::from(v))));
    }
    if let Some(t) = tf.primary_tag().or_else(|| tf.first_tag()) {
        tags(t, out);
    }
    if ext.eq_ignore_ascii_case("mp3")
        && let Some(vbr) = mp3_vbr(path, limits)
    {
        out.push((P::IS_VARIABLE_BIT_RATE, Value::Int(i64::from(vbr))));
    }
}

/// Generic tag items.
pub fn tags(t: &Tag, out: &mut Props) {
    if let Some(s) = t.artist() {
        push_text(out, P::ARTIST, &s);
    }
    if let Some(s) = t.title() {
        push_text(out, P::TITLE, &s);
    }
    if let Some(s) = t.album() {
        push_text(out, P::ALBUM, &s);
    }
    if let Some(s) = t.genre() {
        push_text(out, P::GENRE, &s);
    }
    if let Some(s) = t.comment() {
        push_text(out, P::COMMENT, &s);
    }
    if let Some(n) = t.track().filter(|n| *n > 0) {
        out.push((P::TRACK, Value::Int(i64::from(n))));
    }
    if let Some(n) = t.disk().filter(|n| *n > 0) {
        out.push((P::DISC, Value::Int(i64::from(n))));
    }
    let year = t.date().map(|d| i64::from(d.year)).filter(|y| *y > 0).or_else(|| {
        [ItemKey::Year, ItemKey::RecordingDate, ItemKey::ReleaseDate, ItemKey::OriginalReleaseDate]
            .into_iter()
            .find_map(|k| t.get_string(k).and_then(|s| s.trim().get(..4)).and_then(|s| s.parse().ok()))
    });
    if let Some(y) = year {
        out.push((P::YEAR, Value::Int(y)));
    }
    for (key, p) in [
        (ItemKey::AlbumArtist, P::ALBUM_ARTIST),
        (ItemKey::Composer, P::COMPOSER),
        (ItemKey::Conductor, P::CONDUCTOR),
        (ItemKey::Publisher, P::PUBLISHER),
        (ItemKey::Label, P::PUBLISHER),
        (ItemKey::Lyrics, P::LYRICS),
        (ItemKey::UnsyncLyrics, P::LYRICS),
        (ItemKey::Mood, P::MOOD),
        (ItemKey::InitialKey, P::INITIAL_KEY),
        (ItemKey::EncodedBy, P::ENCODED_BY),
        (ItemKey::Director, P::DIRECTOR),
        (ItemKey::Producer, P::PRODUCER),
        (ItemKey::Writer, P::WRITER),
    ] {
        if let Some(s) = t.get_string(key) {
            push_text(out, p, s);
        }
    }
    let bpm = t.get_string(ItemKey::IntegerBpm).or_else(|| t.get_string(ItemKey::Bpm));
    if let Some(b) = bpm.and_then(|s| s.trim().parse::<f64>().ok()).filter(|b| *b > 0.0 && b.is_finite()) {
        out.push((P::BPM, Value::Int(b.round() as i64)));
    }
}

/// `Some(true)` if the first MPEG frame carries a Xing/VBRI header (variable bit rate),
/// `Some(false)` for an `Info` (constant bit rate) header or none.
fn mp3_vbr(path: &Path, limits: ExtractLimits) -> Option<bool> {
    let mut r = open(path, limits).ok()?;
    let mut head = vec![0u8; 10];
    r.read_exact(&mut head).ok()?;
    let mut skip = 0usize;
    if head.starts_with(b"ID3") {
        let size = head[6..10].iter().fold(0usize, |a, &b| (a << 7) | usize::from(b & 0x7f));
        skip = size + if head[5] & 0x10 != 0 { 10 } else { 0 };
    }
    let mut rest = Vec::new();
    r.take((skip + 8192) as u64).read_to_end(&mut rest).ok()?;
    let frame = rest.get(skip.min(rest.len())..)?;
    let window = &frame[..frame.len().min(4096)];
    Some(memchr::memmem::find(window, b"Xing").is_some() || memchr::memmem::find(window, b"VBRI").is_some())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::extract::testutil::file;
    use lofty::config::WriteOptions;
    use lofty::tag::{Accessor, TagType};

    /// 16-bit stereo PCM WAV of `secs` seconds at 8 kHz.
    pub(crate) fn wav(secs: u32) -> Vec<u8> {
        let rate = 8000u32;
        let data_len = rate * 4 * secs;
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&(36 + data_len).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&2u16.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * 4).to_le_bytes());
        v.extend_from_slice(&4u16.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_len.to_le_bytes());
        v.resize(v.len() + data_len as usize, 0);
        v
    }

    /// MPEG-1 Layer III, 128 kbps, 44.1 kHz frames (silence).
    pub(crate) fn mp3_frames(n: usize, xing: bool) -> Vec<u8> {
        let mut v = Vec::new();
        for i in 0..n {
            let start = v.len();
            v.extend_from_slice(&[0xff, 0xfb, 0x90, 0x64]);
            v.resize(start + 417, 0);
            if i == 0 && xing {
                v[start + 36..start + 40].copy_from_slice(b"Xing");
            }
        }
        v
    }

    fn get(v: &Props, p: P) -> Option<&Value> {
        v.iter().find(|(q, _)| *q == p).map(|(_, v)| v)
    }

    #[test]
    fn mp3_with_id3v2() {
        let (_d, path) = file("song.mp3", &mp3_frames(200, false));
        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_artist("John Doe".into());
        tag.set_title("Morning".into());
        tag.set_album("Sunrise".into());
        tag.set_genre("Jazz".into());
        tag.set_track(3);
        tag.set_disk(1);
        tag.insert_text(ItemKey::AlbumArtist, "The Band".into());
        tag.insert_text(ItemKey::Composer, "J. Doe".into());
        tag.insert_text(ItemKey::IntegerBpm, "120".into());
        tag.set_date(lofty::tag::items::Timestamp { year: 2021, ..Default::default() });
        tag.save_to_path(&path, WriteOptions::default()).unwrap();
        let mut out = Props::new();
        extract(&path, "mp3", ExtractLimits::default(), &mut out);
        assert_eq!(get(&out, P::ARTIST), Some(&Value::Text("John Doe".into())));
        assert_eq!(get(&out, P::TITLE), Some(&Value::Text("Morning".into())));
        assert_eq!(get(&out, P::ALBUM), Some(&Value::Text("Sunrise".into())));
        assert_eq!(get(&out, P::GENRE), Some(&Value::Text("Jazz".into())));
        assert_eq!(get(&out, P::TRACK), Some(&Value::Int(3)));
        assert_eq!(get(&out, P::DISC), Some(&Value::Int(1)));
        assert_eq!(get(&out, P::ALBUM_ARTIST), Some(&Value::Text("The Band".into())));
        assert_eq!(get(&out, P::COMPOSER), Some(&Value::Text("J. Doe".into())));
        assert_eq!(get(&out, P::BPM), Some(&Value::Int(120)));
        assert_eq!(get(&out, P::YEAR), Some(&Value::Int(2021)));
        assert_eq!(get(&out, P::SAMPLE_RATE), Some(&Value::Int(44100)));
        assert_eq!(get(&out, P::CHANNELS), Some(&Value::Int(2)));
        assert_eq!(get(&out, P::BIT_RATE), Some(&Value::Int(128)));
        assert_eq!(get(&out, P::IS_VARIABLE_BIT_RATE), Some(&Value::Int(0)));
        match get(&out, P::LENGTH) {
            // 200 frames × 1152 samples / 44.1 kHz ≈ 5.2 s.
            Some(Value::Duration(ms)) => assert!((5000..5500).contains(ms), "{ms}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn wav_properties_and_vbr_flag() {
        let (_d, path) = file("a.wav", &wav(2));
        let mut out = Props::new();
        extract(&path, "wav", ExtractLimits::default(), &mut out);
        assert_eq!(get(&out, P::LENGTH), Some(&Value::Duration(2000)));
        assert_eq!(get(&out, P::SAMPLE_RATE), Some(&Value::Int(8000)));
        assert_eq!(get(&out, P::BITS_PER_SAMPLE), Some(&Value::Int(16)));
        assert_eq!(get(&out, P::CHANNELS), Some(&Value::Int(2)));
        let (_d2, vbr) = file("v.mp3", &mp3_frames(20, true));
        assert_eq!(mp3_vbr(&vbr, ExtractLimits::default()), Some(true));
    }
}

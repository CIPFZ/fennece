//! Image header (dimensions, bit depth — no pixel decoding) and EXIF.

use std::io::{BufRead, Seek, SeekFrom};
use std::path::Path;

use exif::{Context, In, Tag};
use fnc_core::FileTime;

use super::{ExtractLimits, Props, open, push_text};
use crate::schema::PropertyId as P;
use crate::value::Value;

/// Width, height, bit depth and EXIF of the image at `path`.
pub fn extract(path: &Path, limits: ExtractLimits, out: &mut Props) {
    let Ok(mut r) = open(path, limits) else { return };
    header(&mut r, out);
    if r.seek(SeekFrom::Start(0)).is_ok() {
        exif_props(&mut r, out);
    }
}

/// Dimensions and bit depth from the format header.
pub fn header<R: BufRead + Seek>(r: &mut R, out: &mut Props) {
    use ::image::ImageDecoder;
    let Ok(reader) = ::image::ImageReader::new(&mut *r).with_guessed_format() else { return };
    if reader.format().is_none() {
        return;
    }
    let Ok(dec) = reader.into_decoder() else { return };
    let (w, h) = dec.dimensions();
    if w > 0 && h > 0 {
        out.push((P::WIDTH, Value::Int(i64::from(w))));
        out.push((P::HEIGHT, Value::Int(i64::from(h))));
    }
    let bits = dec.original_color_type().bits_per_pixel();
    if bits > 0 {
        out.push((P::BIT_DEPTH, Value::Int(i64::from(bits))));
    }
}

fn ascii(f: &exif::Field) -> Option<String> {
    match &f.value {
        exif::Value::Ascii(v) => {
            let s: Vec<String> =
                v.iter().map(|b| String::from_utf8_lossy(b).trim().to_string()).filter(|s| !s.is_empty()).collect();
            (!s.is_empty()).then(|| s.join(" "))
        }
        exif::Value::Undefined(b, _) | exif::Value::Byte(b) => {
            let s = String::from_utf8_lossy(b).trim_matches(char::from(0)).trim().to_string();
            (!s.is_empty()).then_some(s)
        }
        _ => None,
    }
}

fn rational(f: &exif::Field, i: usize) -> Option<f64> {
    let v = match &f.value {
        exif::Value::Rational(v) => v.get(i).filter(|r| r.denom != 0).map(|r| r.to_f64()),
        exif::Value::SRational(v) => v.get(i).filter(|r| r.denom != 0).map(|r| r.to_f64()),
        other => other.get_uint(i).map(f64::from),
    }?;
    v.is_finite().then_some(v)
}

/// UTF-16LE text of the Windows `XP*` tags.
fn xp_text(f: &exif::Field) -> Option<String> {
    let b = match &f.value {
        exif::Value::Byte(b) | exif::Value::Undefined(b, _) => b,
        _ => return None,
    };
    let units: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).take_while(|&u| u != 0).collect();
    let s = String::from_utf16_lossy(&units).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// EXIF date `YYYY:MM:DD HH:MM:SS` (local time, or UTC offset when recorded).
fn exif_date(f: &exif::Field, offset: Option<&exif::Field>) -> Option<FileTime> {
    let exif::Value::Ascii(v) = &f.value else { return None };
    let mut dt = exif::DateTime::from_ascii(v.first()?).ok()?;
    if let Some(exif::Value::Ascii(o)) = offset.map(|o| &o.value)
        && let Some(o) = o.first()
    {
        let _ = dt.parse_offset(o);
    }
    let date = chrono::NaiveDate::from_ymd_opt(i32::from(dt.year), u32::from(dt.month), u32::from(dt.day))?;
    let naive = date.and_hms_opt(u32::from(dt.hour), u32::from(dt.minute), u32::from(dt.second))?;
    Some(match dt.offset {
        Some(minutes) => FileTime::from_utc(naive.and_utc() - chrono::Duration::minutes(i64::from(minutes))),
        None => FileTime::from_local(naive),
    })
}

/// EXIF properties (camera, exposure, GPS, Windows XP tags, rating).
pub fn exif_props<R: BufRead + Seek>(r: &mut R, out: &mut Props) {
    let Ok(ex) = exif::Reader::new().read_from_container(r) else { return };
    let get = |t: Tag| ex.get_field(t, In::PRIMARY);
    let tiff = |n: u16| ex.get_field(Tag(Context::Tiff, n), In::PRIMARY);
    if let Some(f) = get(Tag::Make).and_then(ascii) {
        push_text(out, P::CAMERA_MAKER, &f);
    }
    if let Some(f) = get(Tag::Model).and_then(ascii) {
        push_text(out, P::CAMERA_MODEL, &f);
    }
    let taken = get(Tag::DateTimeOriginal)
        .and_then(|f| exif_date(f, get(Tag::OffsetTimeOriginal)))
        .or_else(|| get(Tag::DateTime).and_then(|f| exif_date(f, get(Tag::OffsetTime))));
    if let Some(t) = taken {
        out.push((P::DATE_TAKEN, Value::Date(t)));
    }
    if let Some(v) = get(Tag::ExposureTime).and_then(|f| rational(f, 0)) {
        out.push((P::EXPOSURE_TIME, Value::Float(v)));
    }
    if let Some(v) = get(Tag::FNumber).and_then(|f| rational(f, 0)) {
        out.push((P::F_STOP, Value::Float(v)));
    }
    if let Some(v) = get(Tag::PhotographicSensitivity).and_then(|f| f.value.get_uint(0)) {
        out.push((P::ISO_SPEED, Value::Int(i64::from(v))));
    }
    if let Some(v) = get(Tag::FocalLength).and_then(|f| rational(f, 0)) {
        out.push((P::FOCAL_LENGTH, Value::Float(v)));
    }
    if let Some(v) = get(Tag::Flash).and_then(|f| f.value.get_uint(0)) {
        out.push((P::FLASH, Value::Int(i64::from(v & 1))));
    }
    if let Some(v) = get(Tag::Orientation).and_then(|f| f.value.get_uint(0)).filter(|v| (1..=8).contains(v)) {
        out.push((P::ORIENTATION, Value::Int(i64::from(v))));
    }
    let per_cm = get(Tag::ResolutionUnit).and_then(|f| f.value.get_uint(0)) == Some(3);
    for (tag, p) in [(Tag::XResolution, P::HORIZONTAL_RESOLUTION), (Tag::YResolution, P::VERTICAL_RESOLUTION)] {
        if let Some(v) = get(tag).and_then(|f| rational(f, 0)).filter(|v| *v > 0.0) {
            out.push((p, Value::Float(if per_cm { v * 2.54 } else { v })));
        }
    }
    let coord = |val: Tag, reference: Tag, neg: u8| {
        let f = get(val)?;
        let (d, m, s) = (rational(f, 0)?, rational(f, 1).unwrap_or(0.0), rational(f, 2).unwrap_or(0.0));
        let mut v = d + m / 60.0 + s / 3600.0;
        if get(reference).and_then(ascii).is_some_and(|r| r.as_bytes().first() == Some(&neg)) {
            v = -v;
        }
        Some(v)
    };
    if let Some(v) = coord(Tag::GPSLatitude, Tag::GPSLatitudeRef, b'S') {
        out.push((P::LATITUDE, Value::Float(v)));
    }
    if let Some(v) = coord(Tag::GPSLongitude, Tag::GPSLongitudeRef, b'W') {
        out.push((P::LONGITUDE, Value::Float(v)));
    }
    if let Some(mut v) = get(Tag::GPSAltitude).and_then(|f| rational(f, 0)) {
        if get(Tag::GPSAltitudeRef).and_then(|f| f.value.get_uint(0)) == Some(1) {
            v = -v;
        }
        out.push((P::ALTITUDE, Value::Float(v)));
    }
    exif_more(&get, out);
    if let Some(s) = get(Tag::Copyright).and_then(ascii) {
        push_text(out, P::COPYRIGHT, &s);
    }
    // Windows: XPTitle/ImageDescription → title, XPSubject → subject, XPComment →
    // comments, XPAuthor/Artist → authors, XPKeywords → tags.
    if let Some(s) = tiff(0x9c9b).and_then(xp_text).or_else(|| get(Tag::ImageDescription).and_then(ascii)) {
        push_text(out, P::TITLE, &s);
    }
    if let Some(s) = tiff(0x9c9f).and_then(xp_text) {
        push_text(out, P::SUBJECT, &s);
    }
    if let Some(s) = tiff(0x9c9c).and_then(xp_text).or_else(|| get(Tag::UserComment).and_then(user_comment)) {
        push_text(out, P::COMMENT, &s);
    }
    if let Some(s) = tiff(0x9c9d).and_then(xp_text).or_else(|| get(Tag::Artist).and_then(ascii))
        && let Some(v) = Value::multi(s.split(';'))
    {
        out.push((P::AUTHOR, v));
    }
    if let Some(s) = tiff(0x9c9e).and_then(xp_text)
        && let Some(v) = Value::multi(s.split(';'))
    {
        out.push((P::TAGS, v));
    }
    if let Some(v) = tiff(0x4746).and_then(|f| f.value.get_uint(0)).filter(|v| *v <= 5) {
        out.push((P::RATING, Value::Int(i64::from(v))));
    }
    // Dimensions for formats the header reader does not know (HEIF, RAW).
    if !out.iter().any(|(p, _)| *p == P::WIDTH)
        && let (Some(w), Some(h)) = (
            get(Tag::PixelXDimension).and_then(|f| f.value.get_uint(0)),
            get(Tag::PixelYDimension).and_then(|f| f.value.get_uint(0)),
        )
    {
        out.push((P::WIDTH, Value::Int(i64::from(w))));
        out.push((P::HEIGHT, Value::Int(i64::from(h))));
    }
}

/// The further EXIF/GPS tags of appendix B (lens, exposure program, metering, GPS
/// direction/speed/destination, …), in the units of the Windows Property System.
fn exif_more<'a>(get: &dyn Fn(Tag) -> Option<&'a exif::Field>, out: &mut Props) {
    let uint = |t: Tag| get(t).and_then(|f| f.value.get_uint(0));
    let real = |t: Tag| get(t).and_then(|f| rational(f, 0));
    for (tag, p) in [
        (Tag::LensMake, P::LENS_MAKER),
        (Tag::LensModel, P::LENS_MODEL),
        (Tag::BodySerialNumber, P::CAMERA_SERIAL_NUMBER),
        (Tag::Software, P::SOFTWARE_USED),
        (Tag::ImageUniqueID, P::IMAGE_ID),
        (Tag::RelatedSoundFile, P::RELATED_SOUND_FILE),
        (Tag::GPSImgDirectionRef, P::GPS_IMG_DIRECTION_REF),
        (Tag::GPSSpeedRef, P::GPS_SPEED_REF),
        (Tag::GPSTrackRef, P::GPS_TRACK_REF),
        (Tag::GPSSatellites, P::GPS_SATELLITES),
        (Tag::GPSStatus, P::GPS_STATUS),
        (Tag::GPSMeasureMode, P::GPS_MEASURE_MODE),
        (Tag::GPSMapDatum, P::GPS_MAP_DATUM),
    ] {
        if let Some(s) = get(tag).and_then(ascii) {
            push_text(out, p, &s);
        }
    }
    if let Some(f) = get(Tag::ExifVersion)
        && let exif::Value::Undefined(b, _) = &f.value
        && b.len() == 4
        && b.iter().all(u8::is_ascii_digit)
    {
        // "0231" → "2.31" (Explorer's form).
        let s = String::from_utf8_lossy(b);
        let (major, minor) = s.split_at(2);
        push_text(out, P::EXIF_VERSION, &format!("{}.{minor}", major.trim_start_matches('0')));
    }
    for (tag, p) in [
        (Tag::ApertureValue, P::APERTURE),
        (Tag::MaxApertureValue, P::MAX_APERTURE),
        (Tag::ShutterSpeedValue, P::SHUTTER_SPEED),
        (Tag::BrightnessValue, P::BRIGHTNESS),
        (Tag::ExposureBiasValue, P::EXPOSURE_BIAS),
        (Tag::ExposureIndex, P::EXPOSURE_INDEX),
        (Tag::FlashEnergy, P::FLASH_ENERGY),
        (Tag::SubjectDistance, P::SUBJECT_DISTANCE),
        (Tag::FocalPlaneXResolution, P::FOCAL_PLANE_X_RESOLUTION),
        (Tag::FocalPlaneYResolution, P::FOCAL_PLANE_Y_RESOLUTION),
        (Tag::CompressedBitsPerPixel, P::COMPRESSED_BITS_PER_PIXEL),
        (Tag::GPSImgDirection, P::GPS_IMG_DIRECTION),
        (Tag::GPSSpeed, P::GPS_SPEED),
        (Tag::GPSTrack, P::GPS_TRACK),
        (Tag::GPSDOP, P::GPS_DOP),
        (Tag::GPSDestBearing, P::GPS_DEST_BEARING),
        (Tag::GPSDestDistance, P::GPS_DEST_DISTANCE),
    ] {
        if let Some(v) = real(tag) {
            out.push((p, Value::Float(v)));
        }
    }
    // A digital zoom ratio of 0 means "not used".
    if let Some(v) = real(Tag::DigitalZoomRatio).filter(|v| *v > 0.0) {
        out.push((P::DIGITAL_ZOOM, Value::Float(v)));
    }
    if let Some(v) = uint(Tag::FocalLengthIn35mmFilm).filter(|v| *v > 0) {
        out.push((P::FOCAL_LENGTH_35MM, Value::Int(i64::from(v))));
    }
    for (tag, p) in [
        (Tag::ExposureProgram, P::EXPOSURE_PROGRAM),
        (Tag::MeteringMode, P::METERING_MODE),
        (Tag::WhiteBalance, P::WHITE_BALANCE),
        (Tag::Contrast, P::CONTRAST),
        (Tag::Saturation, P::SATURATION),
        (Tag::Sharpness, P::SHARPNESS),
        (Tag::GainControl, P::GAIN_CONTROL),
        (Tag::ColorSpace, P::COLOR_SPACE),
        (Tag::Compression, P::IMAGE_COMPRESSION),
        (Tag::ResolutionUnit, P::RESOLUTION_UNIT),
        (Tag::GPSDifferential, P::GPS_DIFFERENTIAL),
    ] {
        if let Some(v) = uint(tag) {
            out.push((p, Value::Int(i64::from(v))));
        }
    }
    let coord = |val: Tag, reference: Tag, neg: u8| {
        let f = get(val)?;
        let (d, m, s) = (rational(f, 0)?, rational(f, 1).unwrap_or(0.0), rational(f, 2).unwrap_or(0.0));
        let v = d + m / 60.0 + s / 3600.0;
        let negative = get(reference).and_then(ascii).is_some_and(|r| r.as_bytes().first() == Some(&neg));
        Some(if negative { -v } else { v })
    };
    if let Some(v) = coord(Tag::GPSDestLatitude, Tag::GPSDestLatitudeRef, b'S') {
        out.push((P::GPS_DEST_LATITUDE, Value::Float(v)));
    }
    if let Some(v) = coord(Tag::GPSDestLongitude, Tag::GPSDestLongitudeRef, b'W') {
        out.push((P::GPS_DEST_LONGITUDE, Value::Float(v)));
    }
    // GPSDateStamp `YYYY:MM:DD` + GPSTimeStamp (UTC h, m, s rationals).
    if let Some(d) = get(Tag::GPSDateStamp).and_then(ascii)
        && let Ok(date) = chrono::NaiveDate::parse_from_str(d.trim(), "%Y:%m:%d")
    {
        let t = get(Tag::GPSTimeStamp);
        let part = |i| t.and_then(|f| rational(f, i)).unwrap_or(0.0);
        let secs = (part(0) * 3600.0 + part(1) * 60.0 + part(2)).clamp(0.0, 86_399.0);
        if let Some(dt) = date.and_hms_opt(0, 0, 0) {
            let utc = dt.and_utc() + chrono::Duration::milliseconds((secs * 1000.0) as i64);
            out.push((P::GPS_DATE, Value::Date(FileTime::from_utc(utc))));
        }
    }
}

/// EXIF `UserComment`: 8-byte character code then text.
fn user_comment(f: &exif::Field) -> Option<String> {
    let exif::Value::Undefined(b, _) = &f.value else { return None };
    let (code, text) = b.split_at_checked(8)?;
    let s = if code.starts_with(b"UNICODE") {
        let u: Vec<u16> = text.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
        String::from_utf16_lossy(&u)
    } else {
        String::from_utf8_lossy(text).into_owned()
    };
    let s = s.trim_matches(char::from(0)).trim().to_string();
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::extract::testutil::file;
    use exif::experimental::Writer;
    use exif::{Field, Rational};

    pub(crate) fn png(w: u32, h: u32) -> Vec<u8> {
        let img = ::image::RgbaImage::new(w, h);
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, ::image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn field(tag: Tag, value: exif::Value) -> Field {
        Field { tag, ifd_num: In::PRIMARY, value }
    }

    /// A tiny JPEG with an EXIF APP1 segment.
    pub(crate) fn jpeg_with_exif() -> Vec<u8> {
        let img = ::image::RgbImage::new(16, 8);
        let mut jpg = std::io::Cursor::new(Vec::new());
        img.write_to(&mut jpg, ::image::ImageFormat::Jpeg).unwrap();
        let jpg = jpg.into_inner();
        let r = |num, denom| Rational { num, denom };
        let xp = |s: &str| exif::Value::Byte(s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect());
        let fields = vec![
            field(Tag::Make, exif::Value::Ascii(vec![b"Canon".to_vec()])),
            field(Tag::Model, exif::Value::Ascii(vec![b"EOS R5".to_vec()])),
            field(Tag::DateTimeOriginal, exif::Value::Ascii(vec![b"2026:07:02 10:52:00".to_vec()])),
            field(Tag::OffsetTimeOriginal, exif::Value::Ascii(vec![b"+02:00".to_vec()])),
            field(Tag::ExposureTime, exif::Value::Rational(vec![r(1, 250)])),
            field(Tag::FNumber, exif::Value::Rational(vec![r(28, 10)])),
            field(Tag::PhotographicSensitivity, exif::Value::Short(vec![400])),
            field(Tag::FocalLength, exif::Value::Rational(vec![r(50, 1)])),
            field(Tag::Flash, exif::Value::Short(vec![0x19])),
            field(Tag::Orientation, exif::Value::Short(vec![6])),
            field(Tag::XResolution, exif::Value::Rational(vec![r(300, 1)])),
            field(Tag::YResolution, exif::Value::Rational(vec![r(300, 1)])),
            field(Tag::ResolutionUnit, exif::Value::Short(vec![2])),
            field(Tag::GPSLatitudeRef, exif::Value::Ascii(vec![b"S".to_vec()])),
            field(Tag::GPSLatitude, exif::Value::Rational(vec![r(33, 1), r(30, 1), r(0, 1)])),
            field(Tag::GPSLongitudeRef, exif::Value::Ascii(vec![b"E".to_vec()])),
            field(Tag::GPSLongitude, exif::Value::Rational(vec![r(151, 1), r(15, 1), r(36, 1)])),
            field(Tag::GPSAltitude, exif::Value::Rational(vec![r(58, 1)])),
            field(Tag::Copyright, exif::Value::Ascii(vec![b"(c) Me".to_vec()])),
            field(Tag(Context::Tiff, 0x9c9b), xp("Harbour")),
            field(Tag(Context::Tiff, 0x9c9e), xp("sea;sun")),
            field(Tag(Context::Tiff, 0x4746), exif::Value::Short(vec![4])),
            field(Tag::LensModel, exif::Value::Ascii(vec![b"RF24-105mm F4 L IS USM".to_vec()])),
            field(Tag::ExposureBiasValue, exif::Value::SRational(vec![exif::SRational { num: -2, denom: 3 }])),
            field(Tag::ExposureProgram, exif::Value::Short(vec![3])),
            field(Tag::MeteringMode, exif::Value::Short(vec![5])),
            field(Tag::WhiteBalance, exif::Value::Short(vec![1])),
            field(Tag::FocalLengthIn35mmFilm, exif::Value::Short(vec![75])),
            field(Tag::ExifVersion, exif::Value::Undefined(b"0231".to_vec(), 0)),
            field(Tag::Software, exif::Value::Ascii(vec![b"Fennec 1.0".to_vec()])),
            field(Tag::GPSImgDirectionRef, exif::Value::Ascii(vec![b"T".to_vec()])),
            field(Tag::GPSImgDirection, exif::Value::Rational(vec![r(2715, 10)])),
            field(Tag::GPSDateStamp, exif::Value::Ascii(vec![b"2026:07:02".to_vec()])),
            field(Tag::GPSTimeStamp, exif::Value::Rational(vec![r(8, 1), r(52, 1), r(0, 1)])),
        ];
        let mut w = Writer::new();
        for f in &fields {
            w.push_field(f);
        }
        let mut tiff = std::io::Cursor::new(Vec::new());
        w.write(&mut tiff, false).unwrap();
        let tiff = tiff.into_inner();
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        let mut out = jpg[..2].to_vec(); // SOI
        out.extend_from_slice(&[0xff, 0xe1]);
        out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&app1);
        out.extend_from_slice(&jpg[2..]);
        out
    }

    fn get(v: &Props, p: P) -> Option<&Value> {
        v.iter().find(|(q, _)| *q == p).map(|(_, v)| v)
    }

    #[test]
    fn png_dimensions_and_depth() {
        let (_d, p) = file("a.png", &png(40, 30));
        let mut out = Props::new();
        extract(&p, ExtractLimits::default(), &mut out);
        assert_eq!(get(&out, P::WIDTH), Some(&Value::Int(40)));
        assert_eq!(get(&out, P::HEIGHT), Some(&Value::Int(30)));
        assert_eq!(get(&out, P::BIT_DEPTH), Some(&Value::Int(32)));
    }

    #[test]
    fn jpeg_exif() {
        let (_d, p) = file("a.jpg", &jpeg_with_exif());
        let mut out = Props::new();
        extract(&p, ExtractLimits::default(), &mut out);
        assert_eq!(get(&out, P::WIDTH), Some(&Value::Int(16)));
        assert_eq!(get(&out, P::BIT_DEPTH), Some(&Value::Int(24)));
        assert_eq!(get(&out, P::CAMERA_MAKER), Some(&Value::Text("Canon".into())));
        assert_eq!(get(&out, P::CAMERA_MODEL), Some(&Value::Text("EOS R5".into())));
        let utc = chrono::NaiveDate::from_ymd_opt(2026, 7, 2).unwrap().and_hms_opt(8, 52, 0).unwrap().and_utc();
        assert_eq!(get(&out, P::DATE_TAKEN), Some(&Value::Date(FileTime::from_utc(utc))));
        assert_eq!(get(&out, P::EXPOSURE_TIME), Some(&Value::Float(0.004)));
        assert_eq!(get(&out, P::F_STOP), Some(&Value::Float(2.8)));
        assert_eq!(get(&out, P::ISO_SPEED), Some(&Value::Int(400)));
        assert_eq!(get(&out, P::FOCAL_LENGTH), Some(&Value::Float(50.0)));
        assert_eq!(get(&out, P::FLASH), Some(&Value::Int(1)));
        assert_eq!(get(&out, P::ORIENTATION), Some(&Value::Int(6)));
        assert_eq!(get(&out, P::HORIZONTAL_RESOLUTION), Some(&Value::Float(300.0)));
        match get(&out, P::LATITUDE) {
            Some(Value::Float(v)) => assert!((v + 33.5).abs() < 1e-9),
            other => panic!("{other:?}"),
        }
        match get(&out, P::LONGITUDE) {
            Some(Value::Float(v)) => assert!((v - 151.26).abs() < 1e-9),
            other => panic!("{other:?}"),
        }
        assert_eq!(get(&out, P::ALTITUDE), Some(&Value::Float(58.0)));
        assert_eq!(get(&out, P::COPYRIGHT), Some(&Value::Text("(c) Me".into())));
        assert_eq!(get(&out, P::TITLE), Some(&Value::Text("Harbour".into())));
        assert_eq!(get(&out, P::TAGS), Value::multi(["sea", "sun"]).as_ref());
        assert_eq!(get(&out, P::RATING), Some(&Value::Int(4)));
        // Appendix B tags.
        assert_eq!(get(&out, P::LENS_MODEL), Some(&Value::Text("RF24-105mm F4 L IS USM".into())));
        match get(&out, P::EXPOSURE_BIAS) {
            Some(Value::Float(v)) => assert!((v + 2.0 / 3.0).abs() < 1e-9),
            other => panic!("{other:?}"),
        }
        assert_eq!(get(&out, P::EXPOSURE_PROGRAM), Some(&Value::Int(3)));
        assert_eq!(get(&out, P::METERING_MODE), Some(&Value::Int(5)));
        assert_eq!(get(&out, P::WHITE_BALANCE), Some(&Value::Int(1)));
        assert_eq!(get(&out, P::FOCAL_LENGTH_35MM), Some(&Value::Int(75)));
        assert_eq!(get(&out, P::EXIF_VERSION), Some(&Value::Text("2.31".into())));
        assert_eq!(get(&out, P::SOFTWARE_USED), Some(&Value::Text("Fennec 1.0".into())));
        assert_eq!(get(&out, P::GPS_IMG_DIRECTION), Some(&Value::Float(271.5)));
        assert_eq!(get(&out, P::GPS_IMG_DIRECTION_REF), Some(&Value::Text("T".into())));
        assert_eq!(get(&out, P::GPS_DATE), Some(&Value::Date(FileTime::from_utc(utc))));
        assert_eq!(get(&out, P::RESOLUTION_UNIT), Some(&Value::Int(2)));
    }

    #[test]
    fn truncated_and_corrupt_images() {
        let full = jpeg_with_exif();
        for cut in [3, 20, 100, full.len() / 2] {
            let (_d, p) = file("t.jpg", &full[..cut]);
            let mut out = Props::new();
            extract(&p, ExtractLimits::default(), &mut out);
        }
        let mut bad = png(4, 4);
        bad[20] ^= 0xff;
        let (_d, p) = file("b.png", &bad);
        let mut out = Props::new();
        extract(&p, ExtractLimits::default(), &mut out);
    }
}

//! Windows Property System fallback for the property index (SPEC ch.1 §8.2): any
//! property with a `System.*` name that the built-in extractors of `fnc-props` cannot
//! read (legacy Office and MSI summary information, ASF/AVI media, `people:`, HEIC, …)
//! is read through `SHGetPropertyStoreFromParsingName` + `IPropertyStore`.
//!
//! Values are converted to Fennec's units (durations in milliseconds, bit rates in kbps,
//! frame rates in frames per second, ratings in stars, GPS coordinates in signed
//! degrees). Other platforms have no provider ([`provider`] returns `None`).

use std::sync::Arc;

use fnc_props::extract::PropertyProvider;
use fnc_props::{PropertyId, Value};

/// The Windows Property System provider, if this platform has one.
pub fn provider() -> Option<Arc<dyn PropertyProvider>> {
    imp::provider()
}

/// Converts a raw Windows property value to Fennec's unit for `p` (pure; tested on
/// every platform).
pub fn convert(p: PropertyId, v: Value) -> Option<Value> {
    use fnc_props::PropType;
    // 100-ns units → milliseconds.
    let ticks_to_ms = |v: &Value| v.as_i64().map(|t| Value::Int(t / 10_000));
    // Some numeric properties are strings in the Windows schema (`System.Music.BeatsPerMinute`).
    let v = match (p.ty(), v) {
        (PropType::Int | PropType::Float, Value::Text(t)) => {
            let n: f64 = t.trim().parse().ok().filter(|n: &f64| n.is_finite())?;
            if p.ty() == PropType::Int { Value::Int(n.round() as i64) } else { Value::Float(n) }
        }
        (_, v) => v,
    };
    Some(match p {
        PropertyId::LENGTH | PropertyId::TOTAL_EDITING_TIME => ticks_to_ms(&v)?,
        // Bits per second → kbps.
        PropertyId::BIT_RATE | PropertyId::VIDEO_BITRATE | PropertyId::TOTAL_BITRATE => {
            Value::Int((v.as_i64()? + 500) / 1000)
        }
        // Frames per 1000 seconds → frames per second.
        PropertyId::FRAME_RATE => Value::Float(v.as_f64()? / 1000.0),
        // 1–99 → 1–5 stars (Explorer's buckets); 0 = unrated.
        PropertyId::RATING => Value::Int(match v.as_i64()? {
            i64::MIN..=0 => 0,
            1..=12 => 1,
            13..=37 => 2,
            38..=62 => 3,
            63..=87 => 4,
            _ => 5,
        }),
        // `System.Video.FourCC` is a `MAKEFOURCC` integer: `avc1`.
        PropertyId::FOUR_CC => match v {
            Value::Int(x) => {
                let b = (x as u32).to_le_bytes();
                let s: String = b.iter().map(|&c| char::from(c)).collect();
                if b.iter().all(|c| c.is_ascii_graphic() || *c == b' ') {
                    Value::text(s)?
                } else {
                    Value::Text(format!("{:#010x}", x as u32).into())
                }
            }
            v => v,
        },
        // EXIF flash bits: bit 0 = fired.
        PropertyId::FLASH => Value::Int(v.as_i64()? & 1),
        _ => v,
    })
}

/// Degrees from `[degrees, minutes, seconds]` and a hemisphere reference (`S`/`W`
/// negative).
pub fn dms_to_degrees(dms: &[f64], reference: &str) -> Option<f64> {
    let d = dms.first()? + dms.get(1).unwrap_or(&0.0) / 60.0 + dms.get(2).unwrap_or(&0.0) / 3600.0;
    let neg = matches!(reference.trim().chars().next(), Some('S' | 's' | 'W' | 'w'));
    d.is_finite().then_some(if neg { -d } else { d })
}

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::sync::Arc;

    use fnc_core::FileTime;
    use fnc_props::extract::PropertyProvider;
    use fnc_props::{PropType, PropertyId, Value};
    use parking_lot::Mutex;
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::System::Com::StructuredStorage::{
        PROPVARIANT, PropVariantClear, PropVariantGetDoubleElem, PropVariantGetElementCount, PropVariantToBoolean,
        PropVariantToDouble, PropVariantToFileTime, PropVariantToInt64, PropVariantToStringAlloc,
        PropVariantToStringVectorAlloc,
    };
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree};
    use windows::Win32::System::Variant::{
        PSTF_UTC, VARENUM, VT_BOOL, VT_BSTR, VT_DATE, VT_EMPTY, VT_FILETIME, VT_LPWSTR, VT_NULL, VT_R4, VT_R8,
        VT_VECTOR,
    };
    use windows::Win32::UI::Shell::PropertiesSystem::{
        GPS_BESTEFFORT, GPS_DEFAULT, IPropertyStore, PSGetPropertyKeyFromName, SHGetPropertyStoreFromParsingName,
    };
    use windows::core::{PCWSTR, PWSTR};

    use super::{convert, dms_to_degrees};

    thread_local! {
        // COM stays initialised for the life of the calling thread (extraction thread,
        // search pool threads); never uninitialised.
        static COM: () = {
            // SAFETY: initialises COM for this thread; an already initialised thread
            // (another apartment model) just gets an error, which is harmless.
            let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        };
    }

    pub fn provider() -> Option<Arc<dyn PropertyProvider>> {
        Some(Arc::new(WindowsProperties::default()))
    }

    /// Reads properties through the Windows Property System.
    #[derive(Default)]
    struct WindowsProperties {
        /// `System.*` name → key (`None` = unknown to this system).
        keys: Mutex<HashMap<&'static str, Option<PROPERTYKEY>>>,
    }

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// Frees a `PROPVARIANT` on drop.
    struct Var(PROPVARIANT);

    impl Drop for Var {
        fn drop(&mut self) {
            // SAFETY: the variant was filled by `IPropertyStore::GetValue` (or is
            // zeroed); clearing frees what it owns exactly once.
            let _ = unsafe { PropVariantClear(&mut self.0) };
        }
    }

    impl Var {
        fn vt(&self) -> VARENUM {
            // SAFETY: `vt` is valid for every PROPVARIANT (the decimal arm overlays
            // only the reserved bytes after it).
            unsafe { self.0.Anonymous.Anonymous.vt }
        }
    }

    /// Takes ownership of a CoTaskMem string.
    fn take_pwstr(p: PWSTR) -> Option<String> {
        if p.is_null() {
            return None;
        }
        // SAFETY: `p` is a NUL-terminated string allocated by the PropVariant helpers.
        let s = unsafe { p.to_string() }.ok();
        // SAFETY: allocated with CoTaskMemAlloc by the helper; freed once here.
        unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
        s
    }

    /// Converts a variant to a value (by the variant's own type, then the property's).
    fn value(v: &Var, ty: PropType) -> Option<Value> {
        let vt = v.vt();
        if vt == VT_EMPTY || vt == VT_NULL {
            return None;
        }
        let pv = &v.0 as *const PROPVARIANT;
        if vt.0 & VT_VECTOR.0 != 0 {
            if VARENUM(vt.0 & !VT_VECTOR.0) == VT_LPWSTR || VARENUM(vt.0 & !VT_VECTOR.0) == VT_BSTR {
                let mut arr: *mut PWSTR = std::ptr::null_mut();
                let mut n = 0u32;
                // SAFETY: `pv` is a valid variant; on success `arr` holds `n` CoTaskMem
                // strings in a CoTaskMem array, all freed below.
                unsafe { PropVariantToStringVectorAlloc(pv, &mut arr, &mut n) }.ok()?;
                let mut items = Vec::with_capacity(n as usize);
                for i in 0..n as usize {
                    // SAFETY: `i < n` elements were written by the helper.
                    let s = unsafe { *arr.add(i) };
                    if let Some(s) = take_pwstr(s) {
                        items.push(s);
                    }
                }
                // SAFETY: the array itself is a CoTaskMem allocation.
                unsafe { CoTaskMemFree(Some(arr as *const _)) };
                return Value::multi(items);
            }
            // Numeric vectors (GPS degrees/minutes/seconds): the first element, or all
            // three as degrees for floating point.
            // SAFETY: valid variant.
            let n = unsafe { PropVariantGetElementCount(pv) };
            let mut xs = Vec::with_capacity(n.min(8) as usize);
            for i in 0..n.min(8) {
                // SAFETY: `i` is below the element count.
                if let Ok(x) = unsafe { PropVariantGetDoubleElem(pv, i) } {
                    xs.push(x);
                }
            }
            return match ty {
                PropType::Float => dms_to_degrees(&xs, "").map(Value::Float),
                _ => xs.first().map(|&x| Value::Float(x)),
            };
        }
        if vt == VT_LPWSTR || vt == VT_BSTR {
            // SAFETY: valid variant; the returned string is owned and freed by `take_pwstr`.
            let s = take_pwstr(unsafe { PropVariantToStringAlloc(pv) }.ok()?)?;
            return Value::text(s);
        }
        if vt == VT_FILETIME || vt == VT_DATE {
            // SAFETY: valid variant.
            let ft = unsafe { PropVariantToFileTime(pv, PSTF_UTC) }.ok()?;
            let t = (i64::from(ft.dwHighDateTime) << 32) | i64::from(ft.dwLowDateTime);
            return (t > 0).then_some(Value::Date(FileTime(t)));
        }
        if vt == VT_BOOL {
            // SAFETY: valid variant.
            let b = unsafe { PropVariantToBoolean(pv) }.ok()?;
            return Some(Value::Int(i64::from(b.as_bool())));
        }
        if vt == VT_R4 || vt == VT_R8 {
            // SAFETY: valid variant.
            return unsafe { PropVariantToDouble(pv) }.ok().map(Value::Float);
        }
        // Integers of any width.
        // SAFETY: valid variant.
        match unsafe { PropVariantToInt64(pv) } {
            Ok(x) => Some(Value::Int(x)),
            // SAFETY: valid variant; last resort for other types (decimal, …).
            Err(_) => unsafe { PropVariantToDouble(pv) }.ok().map(Value::Float),
        }
    }

    /// `true` if this system's property schema knows `name` (`System.Photo.LensModel`).
    #[cfg(test)]
    pub(super) fn known_name(name: &str) -> bool {
        COM.with(|()| {});
        let w = wide(std::ffi::OsStr::new(name));
        let mut key = PROPERTYKEY::default();
        // SAFETY: `w` is NUL-terminated; `key` is an owned out-structure.
        unsafe { PSGetPropertyKeyFromName(PCWSTR(w.as_ptr()), &mut key) }.is_ok()
    }

    /// The schema's `VARTYPE` of `name` (`None` if unknown).
    #[cfg(test)]
    pub(super) fn schema_type(name: &str) -> Option<u16> {
        use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyDescription, PSGetPropertyDescriptionByName};
        use windows_core::Interface;
        COM.with(|()| {});
        let w = wide(std::ffi::OsStr::new(name));
        let mut raw: *mut std::ffi::c_void = std::ptr::null_mut();
        // SAFETY: `w` is NUL-terminated; on success `raw` is an AddRef'd
        // IPropertyDescription that `from_raw` takes ownership of (released on drop).
        unsafe { PSGetPropertyDescriptionByName(PCWSTR(w.as_ptr()), &IPropertyDescription::IID, &mut raw) }.ok()?;
        if raw.is_null() {
            return None;
        }
        // SAFETY: see above; `raw` is a valid interface pointer of the requested IID.
        let desc = unsafe { IPropertyDescription::from_raw(raw) };
        // SAFETY: valid interface.
        unsafe { desc.GetPropertyType() }.ok()
    }

    impl WindowsProperties {
        fn key(&self, name: &'static str) -> Option<PROPERTYKEY> {
            let mut keys = self.keys.lock();
            *keys.entry(name).or_insert_with(|| {
                let w = wide(std::ffi::OsStr::new(name));
                let mut key = PROPERTYKEY::default();
                // SAFETY: `w` is NUL-terminated; `key` is an owned out-structure.
                unsafe { PSGetPropertyKeyFromName(PCWSTR(w.as_ptr()), &mut key) }.ok().map(|()| key)
            })
        }

        fn read(&self, store: &IPropertyStore, name: &'static str, ty: PropType) -> Option<Value> {
            let key = self.key(name)?;
            // SAFETY: valid store and key; the variant is owned by `Var` and cleared on drop.
            let v = Var(unsafe { store.GetValue(&key) }.ok()?);
            value(&v, ty)
        }
    }

    impl PropertyProvider for WindowsProperties {
        fn get(&self, path: &Path, ids: &[PropertyId]) -> Vec<(PropertyId, Value)> {
            COM.with(|()| {});
            let w = wide(path.as_os_str());
            // SAFETY: `w` is NUL-terminated and outlives the call; no bind context.
            let store: IPropertyStore = match unsafe {
                SHGetPropertyStoreFromParsingName(PCWSTR(w.as_ptr()), None, GPS_DEFAULT | GPS_BESTEFFORT)
            } {
                Ok(s) => s,
                Err(e) => {
                    tracing::trace!(path = %path.display(), error = %e, "no Windows property store");
                    return Vec::new();
                }
            };
            let mut out = Vec::new();
            for &p in ids {
                let Some(def) = p.def() else { continue };
                if def.system.is_empty() {
                    continue;
                }
                let raw = match p {
                    // Signed degrees from the D/M/S vector and the hemisphere.
                    PropertyId::LATITUDE
                    | PropertyId::LONGITUDE
                    | PropertyId::GPS_DEST_LATITUDE
                    | PropertyId::GPS_DEST_LONGITUDE => {
                        let refname = match p {
                            PropertyId::LATITUDE => "System.GPS.LatitudeRef",
                            PropertyId::LONGITUDE => "System.GPS.LongitudeRef",
                            PropertyId::GPS_DEST_LATITUDE => "System.GPS.DestLatitudeRef",
                            _ => "System.GPS.DestLongitudeRef",
                        };
                        let r = self.read(&store, refname, PropType::Text);
                        let r = r.as_ref().and_then(Value::as_str).unwrap_or("").to_string();
                        self.read(&store, def.system, PropType::Float)
                            .and_then(|v| v.as_f64())
                            .and_then(|d| dms_to_degrees(&[d], &r))
                            .map(Value::Float)
                    }
                    // Below sea level when `AltitudeRef` is 1 (EXIF).
                    PropertyId::ALTITUDE => self.read(&store, def.system, PropType::Float).map(|v| {
                        let below = self.read(&store, "System.GPS.AltitudeRef", PropType::Int).and_then(|r| r.as_i64())
                            == Some(1);
                        match v {
                            Value::Float(f) if below => Value::Float(-f.abs()),
                            v => v,
                        }
                    }),
                    _ => self.read(&store, def.system, p.ty()),
                };
                if let Some(v) = raw.and_then(|v| convert(p, v)) {
                    out.push((p, v));
                }
            }
            out
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::sync::Arc;

    use fnc_props::extract::PropertyProvider;

    pub fn provider() -> Option<Arc<dyn PropertyProvider>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_conversion() {
        assert_eq!(convert(PropertyId::LENGTH, Value::Int(1_850_000_000)), Some(Value::Int(185_000)));
        assert_eq!(convert(PropertyId::BIT_RATE, Value::Int(320_000)), Some(Value::Int(320)));
        assert_eq!(convert(PropertyId::FRAME_RATE, Value::Int(29_970)), Some(Value::Float(29.97)));
        assert_eq!(convert(PropertyId::RATING, Value::Int(99)), Some(Value::Int(5)));
        assert_eq!(convert(PropertyId::RATING, Value::Int(50)), Some(Value::Int(3)));
        assert_eq!(convert(PropertyId::FLASH, Value::Int(0x19)), Some(Value::Int(1)));
        assert_eq!(convert(PropertyId::WIDTH, Value::Int(5000)), Some(Value::Int(5000)));
        assert_eq!(convert(PropertyId::LENGTH, Value::Text("x".into())), None);
        // String-typed numbers in the Windows schema.
        assert_eq!(convert(PropertyId::BPM, Value::Text(" 128 ".into())), Some(Value::Int(128)));
        assert_eq!(convert(PropertyId::EXPOSURE_BIAS, Value::Text("-0.7".into())), Some(Value::Float(-0.7)));
        assert_eq!(convert(PropertyId::BPM, Value::Text("fast".into())), None);
        assert_eq!(convert(PropertyId::TITLE, Value::Text("12".into())), Some(Value::Text("12".into())));
        let avc1 = i64::from(u32::from_le_bytes(*b"avc1"));
        assert_eq!(convert(PropertyId::FOUR_CC, Value::Int(avc1)), Some(Value::Text("avc1".into())));
        assert_eq!(convert(PropertyId::FOUR_CC, Value::Int(1)), Some(Value::Text("0x00000001".into())));
        let lat = dms_to_degrees(&[47.0, 30.0, 0.0], "S").unwrap();
        assert!((lat + 47.5).abs() < 1e-9);
        assert_eq!(dms_to_degrees(&[], "N"), None);
    }

    /// Every `System.*` name of the property table is known to the Windows property
    /// schema (catches typos in the table; run on the Windows VM).
    #[cfg(windows)]
    #[test]
    fn every_system_name_resolves() {
        let unknown: Vec<&str> =
            fnc_props::PROPERTIES.iter().map(|d| d.system).filter(|n| !n.is_empty() && !imp::known_name(n)).collect();
        assert!(unknown.is_empty(), "unknown Windows property names: {unknown:?}");
        for n in ["System.GPS.LatitudeRef", "System.GPS.DestLongitudeRef", "System.GPS.AltitudeRef"] {
            assert!(imp::known_name(n), "{n}");
        }
    }

    /// The table's value types fit the Windows schema types (after [`convert`]).
    #[cfg(windows)]
    #[test]
    fn value_types_match_the_windows_schema() {
        use fnc_props::PropType;
        const VECTOR: u16 = 0x1000;
        let (lpwstr, filetime, boolean, r4, r8) = (31u16, 64u16, 11u16, 4u16, 5u16);
        let int = |vt: u16| matches!(vt, 2 | 3 | 16 | 17 | 18 | 19 | 20 | 21 | 22 | 23);
        let mut bad = Vec::new();
        for d in fnc_props::PROPERTIES.iter().filter(|d| !d.system.is_empty()) {
            let Some(vt) = imp::schema_type(d.system) else {
                bad.push(format!("{}: no description", d.system));
                continue;
            };
            let elem = vt & !VECTOR;
            let ok = match d.ty {
                // `convert` turns the FourCC integer into text.
                PropType::Text => elem == lpwstr || (d.id == PropertyId::FOUR_CC && int(vt)),
                PropType::MultiText => elem == lpwstr,
                PropType::Date => vt == filetime,
                PropType::Duration => int(vt),
                // Numbers: integers, reals, numeric vectors (GPS), strings parsed by `convert`.
                PropType::Int => int(vt) || vt == lpwstr || vt == r8 || vt == boolean,
                PropType::Float => int(elem) || elem == r8 || elem == r4 || vt == lpwstr,
                PropType::Bool => vt == boolean || int(vt),
            };
            if !ok {
                bad.push(format!("{} ({:?}): VARTYPE {vt:#x}", d.system, d.ty));
            }
        }
        assert!(bad.is_empty(), "type mismatches: {bad:#?}");
    }

    /// The provider reads image dimensions through the Windows Property System
    /// (WIC's property handler); off Windows there is no provider.
    #[test]
    fn provider_reads_png_dimensions() {
        assert_eq!(provider().is_some(), cfg!(windows));
        let Some(p) = provider() else { return };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        std::fs::write(&path, png(7, 5)).unwrap();
        let v = p.get(&path, &[PropertyId::WIDTH, PropertyId::HEIGHT, PropertyId::ARTIST]);
        assert!(v.contains(&(PropertyId::WIDTH, Value::Int(7))), "{v:?}");
        assert!(v.contains(&(PropertyId::HEIGHT, Value::Int(5))), "{v:?}");
        assert!(p.get(&dir.path().join("missing.png"), &[PropertyId::WIDTH]).is_empty());
    }

    /// A minimal valid PNG (`w` × `h`, 8-bit grey, stored deflate block).
    fn png(w: u32, h: u32) -> Vec<u8> {
        fn crc(data: &[u8]) -> u32 {
            let mut c = 0xFFFF_FFFFu32;
            for &b in data {
                c ^= u32::from(b);
                for _ in 0..8 {
                    c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
                }
            }
            !c
        }
        fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            out.extend((data.len() as u32).to_be_bytes());
            let mut body = kind.to_vec();
            body.extend(data);
            out.extend(&body);
            out.extend(crc(&body).to_be_bytes());
        }
        let mut ihdr = Vec::new();
        ihdr.extend(w.to_be_bytes());
        ihdr.extend(h.to_be_bytes());
        ihdr.extend([8, 0, 0, 0, 0]);
        // Raw scanlines (filter byte 0 + w grey pixels), zlib stored block.
        let raw: Vec<u8> =
            (0..h).flat_map(|_| std::iter::once(0).chain(std::iter::repeat_n(128, w as usize))).collect();
        let mut z = vec![0x78, 0x01, 1];
        z.extend((raw.len() as u16).to_le_bytes());
        z.extend((!(raw.len() as u16)).to_le_bytes());
        z.extend(&raw);
        let (mut a, mut b) = (1u32, 0u32);
        for &x in &raw {
            a = (a + u32::from(x)) % 65521;
            b = (b + a) % 65521;
        }
        z.extend(((b << 16) | a).to_be_bytes());
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    }
}

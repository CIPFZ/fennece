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
    // 100-ns units → milliseconds.
    let ticks_to_ms = |v: &Value| v.as_i64().map(|t| Value::Int(t / 10_000));
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
                    PropertyId::LATITUDE | PropertyId::LONGITUDE => {
                        let refname = if p == PropertyId::LATITUDE {
                            "System.GPS.LatitudeRef"
                        } else {
                            "System.GPS.LongitudeRef"
                        };
                        let r = self.read(&store, refname, PropType::Text);
                        let r = r.as_ref().and_then(Value::as_str).unwrap_or("").to_string();
                        self.read(&store, def.system, PropType::Float)
                            .and_then(|v| v.as_f64())
                            .and_then(|d| dms_to_degrees(&[d], &r))
                            .map(Value::Float)
                    }
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
        let lat = dms_to_degrees(&[47.0, 30.0, 0.0], "S").unwrap();
        assert!((lat + 47.5).abs() < 1e-9);
        assert_eq!(dms_to_degrees(&[], "N"), None);
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

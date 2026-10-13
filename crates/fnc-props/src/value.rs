//! Property values and their display form.

use fnc_core::FileTime;

use crate::schema::{PropType, PropertyId, Unit};

/// A property value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// Integer (also booleans as 0/1 and enumerations such as `machine:`).
    Int(i64),
    /// Real number.
    Float(f64),
    /// Text.
    Text(Box<str>),
    /// Date/time (UTC `FILETIME`).
    Date(FileTime),
    /// Duration in milliseconds.
    Duration(i64),
    /// Several texts.
    MultiText(Box<[Box<str>]>),
}

impl Value {
    /// Text value (trimmed; `None` if empty).
    pub fn text(s: impl AsRef<str>) -> Option<Value> {
        let t = clean_text(s.as_ref());
        (!t.is_empty()).then(|| Value::Text(t.into()))
    }

    /// Multi-text value from items (trimmed, empties dropped, `None` if nothing remains).
    pub fn multi<I, S>(items: I) -> Option<Value>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let v: Vec<Box<str>> =
            items.into_iter().map(|s| clean_text(s.as_ref())).filter(|s| !s.is_empty()).map(Into::into).collect();
        (!v.is_empty()).then(|| Value::MultiText(v.into()))
    }

    /// Converts to the storage shape of `ty` (`Int` for booleans, `Text` ⇄ `MultiText`),
    /// `None` if incompatible.
    pub fn coerce(self, ty: PropType) -> Option<Value> {
        Some(match (ty, self) {
            (PropType::Int | PropType::Bool, Value::Int(v)) => {
                Value::Int(if ty == PropType::Bool { i64::from(v != 0) } else { v })
            }
            (PropType::Int, Value::Float(f)) if f.is_finite() => Value::Int(f.round() as i64),
            (PropType::Float, Value::Float(f)) if f.is_finite() => Value::Float(f),
            (PropType::Float, Value::Int(v)) => Value::Float(v as f64),
            (PropType::Text, Value::Text(t)) => Value::Text(t),
            (PropType::Text, Value::MultiText(v)) => Value::Text(v.join("; ").into()),
            (PropType::MultiText, Value::MultiText(v)) => Value::MultiText(v),
            (PropType::MultiText, Value::Text(t)) => {
                return Value::multi(t.split(';'));
            }
            (PropType::Date, Value::Date(d)) if d.is_known() => Value::Date(d),
            (PropType::Duration, Value::Duration(d)) if d >= 0 => Value::Duration(d),
            (PropType::Duration, Value::Int(d)) if d >= 0 => Value::Duration(d),
            _ => return None,
        })
    }

    /// Numeric view (`Int`, `Date` ticks, `Duration` ms), if any.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(v) | Value::Duration(v) => Some(*v),
            Value::Date(d) => Some(d.0),
            Value::Float(f) if f.is_finite() => Some(f.round() as i64),
            _ => None,
        }
    }

    /// Real view of numeric values.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Int(v) | Value::Duration(v) => Some(*v as f64),
            _ => None,
        }
    }

    /// First / only text.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Text(t) => Some(t),
            Value::MultiText(v) => v.first().map(|s| &**s),
            _ => None,
        }
    }
}

/// Trims and collapses control characters (tags often carry NULs and line breaks).
fn clean_text(s: &str) -> String {
    let t: String = s.chars().map(|c| if c.is_control() && c != '\n' { ' ' } else { c }).collect();
    t.trim().trim_end_matches('\0').trim().to_string()
}

/// Display form of a value without unit context: numbers plain, dates `YYYY-MM-DD HH:MM:SS`
/// (local time), durations `h:mm:ss` / `m:ss`, multi-texts joined with `; `.
pub fn display(v: &Value) -> String {
    match v {
        Value::Int(i) => i.to_string(),
        Value::Float(f) => trim_float(*f, 3),
        Value::Text(t) => t.to_string(),
        Value::Date(d) => display_date(*d),
        Value::Duration(ms) => display_duration(*ms),
        Value::MultiText(v) => v.join("; "),
    }
}

/// Local date-time `YYYY-MM-DD HH:MM:SS` (empty if unknown).
pub fn display_date(d: FileTime) -> String {
    d.to_local().map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_default()
}

/// `h:mm:ss` (or `m:ss` below an hour).
pub fn display_duration(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    let (h, m, sec) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{sec:02}") } else { format!("{m}:{sec:02}") }
}

fn trim_float(f: f64, decimals: usize) -> String {
    let s = format!("{f:.decimals$}");
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

impl PropertyId {
    /// Display form with the property's unit and value names (`320 kbps`, `f/2.8`,
    /// `1/250 sec.`, `x64`, `Yes`).
    pub fn format(self, v: &Value) -> String {
        if let Value::Int(i) = v {
            if self.ty() == PropType::Bool {
                return if *i != 0 { "Yes".into() } else { "No".into() };
            }
            if let Some((name, _)) = self.value_names().iter().find(|(_, k)| k == i) {
                return (*name).to_string();
            }
        }
        match (self.unit(), v) {
            (Unit::Pixels, Value::Int(i)) => format!("{i} pixels"),
            (Unit::Dpi, _) => v.as_f64().map_or_else(|| display(v), |f| format!("{} dpi", trim_float(f, 2))),
            (Unit::Bits, Value::Int(i)) => format!("{i} bit"),
            (Unit::Kbps, Value::Int(i)) => format!("{i} kbps"),
            (Unit::Hz, Value::Int(i)) => {
                if *i >= 1000 {
                    format!("{} kHz", trim_float(*i as f64 / 1000.0, 3))
                } else {
                    format!("{i} Hz")
                }
            }
            (Unit::Mm, _) => v.as_f64().map_or_else(|| display(v), |f| format!("{} mm", trim_float(f, 1))),
            (Unit::FNumber, _) => v.as_f64().map_or_else(|| display(v), |f| format!("f/{}", trim_float(f, 1))),
            (Unit::Seconds, _) => v.as_f64().map_or_else(|| display(v), exposure),
            (Unit::Degrees, _) => v.as_f64().map_or_else(|| display(v), |f| format!("{}°", trim_float(f, 6))),
            (Unit::Meters, _) => v.as_f64().map_or_else(|| display(v), |f| format!("{} m", trim_float(f, 1))),
            (Unit::Fps, _) => v.as_f64().map_or_else(|| display(v), |f| format!("{} fps", trim_float(f, 3))),
            (Unit::Stars, Value::Int(i)) => format!("{} stars", i),
            _ => display(v),
        }
    }
}

/// `1/250 sec.` for short exposures, `2 sec.` otherwise.
fn exposure(f: f64) -> String {
    if f > 0.0 && f < 0.5 { format!("1/{} sec.", (1.0 / f).round()) } else { format!("{} sec.", trim_float(f, 2)) }
}

/// Total-order sort key of a value (unknown sorts first): integers, dates and durations
/// by value, floats by IEEE total order, texts by `rank` (a caller-provided ordinal, see
/// `PropertyStore::sort_keys`).
pub fn numeric_sort_key(v: &Value) -> Option<i64> {
    match v {
        Value::Int(i) | Value::Duration(i) => Some(*i),
        Value::Date(d) => Some(d.0),
        Value::Float(f) => Some(float_key(*f)),
        Value::Text(_) | Value::MultiText(_) => None,
    }
}

/// Order-preserving map of `f64` onto `i64` (IEEE 754 total order).
pub fn float_key(f: f64) -> i64 {
    let b = f.to_bits() as i64;
    b ^ ((((b >> 63) as u64) >> 1) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_forms() {
        assert_eq!(display(&Value::Int(5)), "5");
        assert_eq!(display(&Value::Float(2.80)), "2.8");
        assert_eq!(display(&Value::Duration(3_725_000)), "1:02:05");
        assert_eq!(display(&Value::Duration(65_000)), "1:05");
        assert_eq!(display(&Value::MultiText(vec!["a".into(), "b".into()].into())), "a; b");
        assert_eq!(PropertyId::BIT_RATE.format(&Value::Int(320)), "320 kbps");
        assert_eq!(PropertyId::F_STOP.format(&Value::Float(2.8)), "f/2.8");
        assert_eq!(PropertyId::EXPOSURE_TIME.format(&Value::Float(0.004)), "1/250 sec.");
        assert_eq!(PropertyId::MACHINE.format(&Value::Int(0x8664)), "x64");
        assert_eq!(PropertyId::IS_DLL.format(&Value::Int(1)), "Yes");
        assert_eq!(PropertyId::SAMPLE_RATE.format(&Value::Int(44100)), "44.1 kHz");
        assert_eq!(PropertyId::WIDTH.format(&Value::Int(1920)), "1920 pixels");
        assert_eq!(display_date(FileTime::UNKNOWN), "");
    }

    #[test]
    fn constructors_and_coercion() {
        assert_eq!(Value::text("  x\0 "), Some(Value::Text("x".into())));
        assert_eq!(Value::text("   "), None);
        assert_eq!(Value::multi(["a", " ", "b"]), Some(Value::MultiText(vec!["a".into(), "b".into()].into())));
        assert_eq!(Value::Int(5).coerce(PropType::Bool), Some(Value::Int(1)));
        assert_eq!(Value::Text("a;b".into()).coerce(PropType::MultiText).unwrap().as_str(), Some("a"));
        assert_eq!(Value::Float(f64::NAN).coerce(PropType::Float), None);
        assert_eq!(Value::Text("x".into()).coerce(PropType::Int), None);
    }

    #[test]
    fn float_keys_preserve_order() {
        let v = [-10.5, -1.0, -0.0, 0.0, 0.004, 1.0, 2.8, 1e9];
        for w in v.windows(2) {
            assert!(float_key(w[0]) <= float_key(w[1]), "{w:?}");
        }
    }
}

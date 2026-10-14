//! Localisation (Fluent). Every UI string goes through [`t`] / [`t_args`]
//! (AGENT.md §5: no hard-coded UI text).
//!
//! Bundles are embedded at compile time; the active language falls back to English for
//! missing keys and to the key itself as a last resort.

use std::sync::OnceLock;

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource, FluentValue};
use unic_langid::LanguageIdentifier;

const EN: &str = include_str!("../../../assets/languages/en.ftl");
const ZH_CN: &str = include_str!("../../../assets/languages/zh-CN.ftl");
const ZH_TW: &str = include_str!("../../../assets/languages/zh-TW.ftl");
const JA: &str = include_str!("../../../assets/languages/ja.ftl");
const DE: &str = include_str!("../../../assets/languages/de.ftl");
const FR: &str = include_str!("../../../assets/languages/fr.ftl");
const RU: &str = include_str!("../../../assets/languages/ru.ftl");
const PL: &str = include_str!("../../../assets/languages/pl.ftl");
const TR: &str = include_str!("../../../assets/languages/tr.ftl");

/// Shipped languages: `(tag, source)` (SPEC ch.3 §14: the first nine).
pub const LANGUAGES: &[(&str, &str)] = &[
    ("en", EN),
    ("zh-CN", ZH_CN),
    ("zh-TW", ZH_TW),
    ("ja", JA),
    ("de", DE),
    ("fr", FR),
    ("ru", RU),
    ("pl", PL),
    ("tr", TR),
];

struct I18n {
    tag: &'static str,
    active: FluentBundle<FluentResource>,
    fallback: FluentBundle<FluentResource>,
}

static I18N: OnceLock<I18n> = OnceLock::new();

fn bundle(tag: &str, src: &str) -> FluentBundle<FluentResource> {
    let lang: LanguageIdentifier = tag.parse().unwrap_or_default();
    let mut b = FluentBundle::new_concurrent(vec![lang]);
    b.set_use_isolating(false);
    match FluentResource::try_new(src.to_string()) {
        Ok(res) => {
            if let Err(errs) = b.add_resource(res) {
                tracing::warn!(tag, ?errs, "duplicate Fluent messages");
            }
        }
        Err((res, errs)) => {
            tracing::warn!(tag, ?errs, "Fluent syntax errors");
            let _ = b.add_resource(res);
        }
    }
    b
}

/// Picks the best shipped language for a BCP-47 tag (`zh-Hans-CN` → `zh-CN`).
pub fn resolve_language(requested: &str) -> &'static str {
    let req = requested.replace('_', "-").to_ascii_lowercase();
    if let Some((tag, _)) = LANGUAGES.iter().find(|(t, _)| t.to_ascii_lowercase() == req) {
        return tag;
    }
    if req.starts_with("zh") {
        // Traditional script and its regions; everything else is Simplified.
        let traditional = req.contains("hant") || ["zh-tw", "zh-hk", "zh-mo"].iter().any(|r| req.starts_with(r));
        return if traditional { "zh-TW" } else { "zh-CN" };
    }
    let primary = req.split('-').next().unwrap_or("");
    LANGUAGES.iter().find(|(t, _)| t.split('-').next() == Some(primary)).map_or("en", |(t, _)| t)
}

/// Applies the name collation (`auto`: pinyin for a Chinese UI). Call before the index is
/// loaded; changing it later needs a rank rebuild.
pub fn apply_collation(c: fnc_core::settings::NameCollation) {
    use fnc_core::natsort::{Collation, set_collation};
    use fnc_core::settings::NameCollation as N;
    let pinyin = match c {
        N::Pinyin => true,
        N::CodePoint => false,
        N::Auto => active_language().starts_with("zh"),
    };
    set_collation(if pinyin { Collation::Pinyin } else { Collation::CodePoint });
}

/// Applies the date field order and first day of the week used when parsing searches.
pub fn apply_date_locale(order: fnc_core::settings::DateInputOrder, week: fnc_core::settings::WeekStart) {
    use fnc_core::settings::{DateInputOrder as O, WeekStart as W};
    use fnc_platform::locale::DateFieldOrder as F;
    use fnc_query::date::DateOrder;
    let order = match order {
        O::Mdy => DateOrder::Mdy,
        O::Dmy => DateOrder::Dmy,
        O::Ymd => DateOrder::Ymd,
        O::Auto => match fnc_platform::locale::date_order() {
            Some(F::Dmy) => DateOrder::Dmy,
            Some(F::Ymd) => DateOrder::Ymd,
            _ => DateOrder::Mdy,
        },
    };
    let week = match week {
        W::Monday => chrono::Weekday::Mon,
        W::Sunday => chrono::Weekday::Sun,
        W::Saturday => chrono::Weekday::Sat,
        W::Auto => fnc_platform::locale::first_day_of_week()
            .and_then(|d| chrono::Weekday::try_from(d).ok())
            .unwrap_or(chrono::Weekday::Mon),
    };
    fnc_query::date::set_locale_defaults(order, week);
}

/// Tag of the active language (`en` before [`init`]).
pub fn active_language() -> &'static str {
    I18N.get().map_or("en", |i| i.tag)
}

/// Initialises the active language (`""` = the user's UI language). Idempotent.
pub fn init(language: &str) {
    let requested = if language.is_empty() {
        fnc_platform::locale::ui_language().unwrap_or_default()
    } else {
        language.to_string()
    };
    let requested = requested.split('.').next().unwrap_or("").to_string();
    let tag = resolve_language(&requested);
    let src = LANGUAGES.iter().find(|(t, _)| *t == tag).map_or(EN, |(_, s)| s);
    let _ = I18N.set(I18n { tag, active: bundle(tag, src), fallback: bundle("en", EN) });
}

fn format(key: &str, args: Option<&FluentArgs<'_>>) -> String {
    let Some(i) = I18N.get() else { return key.to_string() };
    for b in [&i.active, &i.fallback] {
        if let Some(pattern) = b.get_message(key).and_then(|m| m.value()) {
            let mut errors = Vec::new();
            let s = b.format_pattern(pattern, args, &mut errors);
            if !errors.is_empty() {
                tracing::debug!(key, ?errors, "Fluent formatting errors");
            }
            return s.into_owned();
        }
    }
    key.to_string()
}

/// Localised string for `key`.
pub fn t(key: &str) -> String {
    format(key, None)
}

/// Localised string with arguments.
pub fn t_args(key: &str, args: &[(&str, FluentValue<'_>)]) -> String {
    let mut fa = FluentArgs::new();
    for (k, v) in args {
        fa.set(*k, v.clone());
    }
    format(key, Some(&fa))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_resolution() {
        assert_eq!(resolve_language("zh_CN"), "zh-CN");
        assert_eq!(resolve_language("zh-Hans-CN"), "zh-CN");
        assert_eq!(resolve_language("en-US"), "en");
        assert_eq!(resolve_language("de-DE"), "de");
        assert_eq!(resolve_language("zh-Hant-TW"), "zh-TW");
        assert_eq!(resolve_language("zh_HK"), "zh-TW");
        assert_eq!(resolve_language("ja-JP"), "ja");
        assert_eq!(resolve_language("pt-BR"), "en");
        assert_eq!(resolve_language(""), "en");
    }

    #[test]
    fn all_languages_have_all_keys() {
        let en = FluentResource::try_new(EN.to_string()).unwrap();
        let keys: Vec<String> = en
            .entries()
            .filter_map(|e| match e {
                fluent_syntax::ast::Entry::Message(m) => Some(m.id.name.to_string()),
                _ => None,
            })
            .collect();
        assert!(keys.len() > 20);
        for (tag, src) in LANGUAGES {
            if let Err((_, errs)) = FluentResource::try_new(src.to_string()) {
                panic!("{tag}: Fluent syntax errors {errs:?}");
            }
            let b = bundle(tag, src);
            for k in &keys {
                assert!(b.has_message(k), "{tag} is missing {k}");
            }
        }
    }

    /// Every property of the table (about 300) has a column title in every language.
    #[test]
    fn every_property_has_a_title_in_every_language() {
        for (tag, src) in LANGUAGES {
            let b = bundle(tag, src);
            for p in fnc_props::PropertyId::all() {
                assert!(b.has_message(&format!("prop-{}", p.name())), "{tag} is missing prop-{}", p.name());
            }
        }
    }

    /// The missing-model hint names the menu item that has the download button (it pointed to
    /// an Options page without one; found by the 0.2.0 install pass, REL-08).
    #[test]
    fn missing_model_hint_names_the_semantic_setup_menu() {
        for (tag, src) in LANGUAGES {
            let b = bundle(tag, src);
            let text = |key: &str| {
                let msg = b.get_message(key).and_then(|m| m.value()).unwrap_or_else(|| panic!("{tag}: {key}"));
                let mut args = FluentArgs::new();
                args.set("path", "model.gguf");
                let mut errs = Vec::new();
                b.format_pattern(msg, Some(&args), &mut errs).into_owned()
            };
            let hint = text("sem-err-no-model");
            for key in ["menu-tools", "cmd-semantic-setup"] {
                let label = text(key);
                assert!(hint.contains(&label), "{tag}: sem-err-no-model {hint:?} does not name {key} {label:?}");
            }
        }
    }

    #[test]
    fn formats_with_arguments() {
        init("en");
        assert_eq!(t_args("status-objects", &[("count", 1.into())]), "1 object");
        assert_eq!(t_args("status-objects", &[("count", 1234.into())]), "1234 objects");
        assert_eq!(t("no-such-key"), "no-such-key");
    }
}

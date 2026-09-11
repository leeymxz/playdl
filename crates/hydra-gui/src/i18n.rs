// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Translation, gettext-style: the msgid IS the English string.
//!
//! `tr("Add URL")` returns the active locale's translation, falling back to
//! the English text itself — a missing key can never render as an identifier.
//!
//! Catalogues are flat JSON maps. Built-in languages ship inside the binary
//! from `assets/locale/<tag>.json` (`en.json` is the identity template
//! translators copy); users can add or override with
//! `<app_dir>/locales/<tag>.json` on disk — disk entries win over built-ins
//! for the same tag.
//!
//! Persian deliberately keeps the LTR layout for now (per product decision);
//! full RTL mirroring (Arabic and friends) needs iced-level layout flipping
//! and is tracked as follow-up work.

use std::collections::HashMap;
use std::sync::RwLock;

/// Languages compiled into the binary: (tag, native display name, catalogue).
static BUILTIN: &[(&str, &str, &str)] = &[
    ("ar", "العربية", include_str!("../assets/locale/ar.json")),
    ("cs", "Čeština", include_str!("../assets/locale/cs.json")),
    ("da", "Dansk", include_str!("../assets/locale/da.json")),
    ("de", "German", include_str!("../assets/locale/de.json")),
    ("el", "Ελληνικά", include_str!("../assets/locale/el.json")),
    ("en", "English", include_str!("../assets/locale/en.json")),
    ("es", "Español", include_str!("../assets/locale/es.json")),
    ("fa", "فارسی", include_str!("../assets/locale/fa.json")),
    ("fi", "Suomi", include_str!("../assets/locale/fi.json")),
    ("fr", "Français", include_str!("../assets/locale/fr.json")),
    ("he", "עברית", include_str!("../assets/locale/he.json")),
    ("hi", "हिन्दी", include_str!("../assets/locale/hi.json")),
    ("hu", "Magyar", include_str!("../assets/locale/hu.json")),
    (
        "id",
        "Bahasa Indonesia",
        include_str!("../assets/locale/id.json"),
    ),
    ("it", "Italiano", include_str!("../assets/locale/it.json")),
    ("ja", "日本語", include_str!("../assets/locale/ja.json")),
    ("ko", "한국어", include_str!("../assets/locale/ko.json")),
    ("nl", "Nederlands", include_str!("../assets/locale/nl.json")),
    ("pl", "Polski", include_str!("../assets/locale/pl.json")),
    ("pt", "Português", include_str!("../assets/locale/pt.json")),
    (
        "pt-BR",
        "Português (Brasil)",
        include_str!("../assets/locale/pt-BR.json"),
    ),
    ("ro", "Română", include_str!("../assets/locale/ro.json")),
    ("ru", "Русский", include_str!("../assets/locale/ru.json")),
    ("sv", "Svenska", include_str!("../assets/locale/sv.json")),
    ("th", "ไทย", include_str!("../assets/locale/th.json")),
    ("tr", "Türkçe", include_str!("../assets/locale/tr.json")),
    ("uk", "Українська", include_str!("../assets/locale/uk.json")),
    ("vi", "Tiếng Việt", include_str!("../assets/locale/vi.json")),
    ("zh", "简体中文", include_str!("../assets/locale/zh.json")),
    (
        "zh-Hant",
        "繁體中文",
        include_str!("../assets/locale/zh-hant.json"),
    ),
];

static CATALOGUE: RwLock<Option<HashMap<String, String>>> = RwLock::new(None);

/// Translate one UI string. English in, active locale out.
pub fn tr(msgid: &str) -> String {
    if let Ok(guard) = CATALOGUE.read() {
        if let Some(map) = guard.as_ref() {
            if let Some(t) = map.get(msgid) {
                return t.clone();
            }
        }
    }
    msgid.to_string()
}

/// Locale tags available: built-ins plus any `<app_dir>/locales/*.json`.
pub fn available() -> Vec<String> {
    let mut tags: Vec<String> = BUILTIN.iter().map(|(t, _, _)| t.to_string()).collect();
    let dir = crate::model::app_dir().join("locales");
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(tag) = name.strip_suffix(".json") {
                if !tags.iter().any(|t| t == tag) {
                    tags.push(tag.to_string());
                }
            }
        }
    }
    tags
}

/// Native display name for a tag ("fa" → "فارسی").
pub fn display_name(tag: &str) -> String {
    BUILTIN
        .iter()
        .find(|(t, _, _)| *t == tag)
        .map(|(_, n, _)| n.to_string())
        .unwrap_or_else(|| tag.to_string())
}

/// Detect the user's preferred locale from the OS, returning a built-in tag
/// (e.g. "zh", "en", "ja") or "en" as a safe fallback. Used when no explicit
/// language has been configured yet.
pub fn detect_system_locale() -> String {
    // 1. Windows: read the user's preferred UI language from the registry.
    #[cfg(target_os = "windows")]
    {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;
        if let Ok(hklm) = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(r"Control Panel\International\User Profile")
        {
            if let Ok(lang_list) = hklm.get_value::<String, _>("Languages") {
                for lang in lang_list.split(';') {
                    let l = lang.trim().to_lowercase();
                    if !l.is_empty() {
                        let tag = map_os_locale(&l);
                        if tag != "en" {
                            return tag;
                        }
                    }
                }
            }
        }
        if let Ok(hkcu) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Control Panel\International") {
            if let Ok(locale_name) = hkcu.get_value::<String, _>("LocaleName") {
                let tag = map_os_locale(&locale_name.to_lowercase());
                if tag != "en" {
                    return tag;
                }
            }
        }
    }
    // 2. Unix/macOS/other: environment variables.
    for var in ["LANG", "LC_ALL", "LC_MESSAGES"] {
        if let Ok(v) = std::env::var(var) {
            let l = v.to_lowercase();
            let tag = map_os_locale(&l);
            if tag != "en" {
                return tag;
            }
        }
    }
    "en".into()
}

/// Map an OS locale string (e.g. "zh-cn", "zh-CN", "zh_CN.UTF-8", "pt-BR")
/// to the closest built-in tag.
fn map_os_locale(locale: &str) -> String {
    let l = locale.to_lowercase();
    let base = l
        .split(['_', '.', '-'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if base.is_empty() {
        return "en".into();
    }
    let builtin = [
        "ar", "cs", "da", "de", "el", "en", "es", "fa", "fi", "fr", "he", "hi", "hu", "id",
        "it", "ja", "ko", "nl", "pl", "pt", "ro", "ru", "sk", "sr", "sv", "th", "tr",
        "uk", "vi", "zh",
    ];
    // 繁体中文处理
    if base == "zh" {
        // zh-TW / zh-HK / zh-MO -> 繁体
        let full = locale.to_lowercase();
        if full.contains("tw") || full.contains("hk") || full.contains("mo") || full.contains("hant") {
            return "zh-Hant".into();
        }
        return "zh".into();
    }
    if builtin.contains(&base.as_str()) {
        base
    } else {
        "en".into()
    }
}

/// The locale currently in effect (for menus and display).
pub fn current_locale() -> String {
    CATALOGUE
        .read()
        .map(|g| if g.is_some() { "custom".into() } else { "en".into() })
        .unwrap_or_else(|_| "en".into())
}

/// Switch locale. `"en"` (or legacy `"English"`) clears back to the built-in
/// English base; unknown/broken catalogues fall back to English too.
pub fn set_locale(tag: &str) {    let map = if tag == "en" || tag == "English" {
        None
    } else {
        let mut merged: HashMap<String, String> = BUILTIN
            .iter()
            .find(|(t, _, _)| *t == tag)
            .and_then(|(_, _, json)| serde_json::from_str(json).ok())
            .unwrap_or_default();
        if let Ok(bytes) = std::fs::read(
            crate::model::app_dir()
                .join("locales")
                .join(format!("{tag}.json")),
        ) {
            if let Ok(disk) = serde_json::from_slice::<HashMap<String, String>>(&bytes) {
                merged.extend(disk);
            }
        }
        (!merged.is_empty()).then_some(merged)
    };
    crate::log::debug(&format!("locale -> {tag}"));
    if let Ok(mut guard) = CATALOGUE.write() {
        *guard = map;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_locales_valid_and_complete() {
        let en_raw = include_str!("../assets/locale/en.json");
        let en_map: HashMap<String, String> =
            serde_json::from_str(en_raw).expect("en.json should be valid JSON");

        for &(tag, name, raw) in BUILTIN {
            assert!(!tag.is_empty(), "tag should not be empty");
            assert!(!name.is_empty(), "display name should not be empty");
            let map: Result<HashMap<String, String>, _> = serde_json::from_str(raw);
            assert!(
                map.is_ok(),
                "locale '{tag}' failed to parse as JSON: {:?}",
                map.err()
            );
            let map = map.unwrap();
            for key in en_map.keys() {
                assert!(
                    map.contains_key(key),
                    "locale '{tag}' is missing translation for key: '{key}'"
                );
            }
            // A translation may reorder `{placeholders}`; it may not invent or
            // lose one. Without this a dropped `{tried_rate}` reaches the UI as
            // literal braces, and nothing at build time would say so.
            for (key, en_val) in &en_map {
                let want = placeholders(en_val);
                if want.is_empty() {
                    continue;
                }
                let got = placeholders(&map[key]);
                assert_eq!(
                    want, got,
                    "locale '{tag}' changed the placeholders in '{key}'"
                );
            }
        }
    }

    /// Every `{name}` token in a string, as a set.
    fn placeholders(s: &str) -> std::collections::BTreeSet<&str> {
        let mut out = std::collections::BTreeSet::new();
        let mut rest = s;
        while let Some(open) = rest.find('{') {
            let Some(close) = rest[open..].find('}') else {
                break;
            };
            out.insert(&rest[open..open + close + 1]);
            rest = &rest[open + close + 1..];
        }
        out
    }
}

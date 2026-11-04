//! Complete Electron dictionaries; system locale is supplied by the caller.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde_json::{Map, Value};

static DICTS: LazyLock<BTreeMap<String, Map<String, Value>>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("strings.json")).expect("embedded Shotik dictionaries")
});

pub fn resolve_lang(setting: &str, system_locale: &str) -> &'static str {
    match setting {
        "ru" => "ru",
        "en" => "en",
        _ if system_locale.to_lowercase().starts_with("ru") => "ru",
        _ => "en",
    }
}

/// JSON map passed to the UI, matching i18n.dict().
pub fn dict(lang: &str) -> Map<String, Value> {
    dictionary(lang).clone()
}

fn dictionary(lang: &str) -> &Map<String, Value> {
    DICTS.get(lang).unwrap_or(&DICTS["en"])
}

/// `makeT(lang)(key, ...args)`: replace every occurrence, in argument order.
pub fn t(lang: &str, key: &str, args: &[&str]) -> String {
    let mut text = dictionary(lang)
        .get(key)
        .or_else(|| DICTS["en"].get(key))
        .and_then(Value::as_str)
        .unwrap_or(key)
        .to_owned();
    for (i, arg) in args.iter().enumerate() {
        text = text.replace(&format!("{{{i}}}"), arg);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key missing from one language would show its raw name, and a placeholder missing from a
    /// translation would silently drop a value from the sentence.
    #[test]
    fn both_languages_have_the_same_keys_and_placeholders() {
        let placeholders = |text: &str| {
            let mut found: Vec<String> = text
                .split('{')
                .skip(1)
                .filter_map(|rest| rest.split_once('}').map(|(inside, _)| inside.to_string()))
                .filter(|inside| !inside.is_empty() && inside.chars().all(|c| c.is_ascii_digit()))
                .collect();
            found.sort();
            found.dedup();
            found
        };
        let (en, ru) = (dict("en"), dict("ru"));
        assert!(en.len() >= 232, "strings were lost: {}", en.len());
        assert_eq!(en.keys().collect::<Vec<_>>(), ru.keys().collect::<Vec<_>>());
        for (key, value) in &en {
            let (english, russian) = (value.as_str().unwrap(), ru[key].as_str().unwrap());
            assert!(!english.is_empty() && !russian.is_empty(), "{key} is empty");
            assert_eq!(placeholders(english), placeholders(russian), "{key}");
        }
    }

    #[test]
    fn language_resolution_fallback_and_placeholders() {
        for (setting, locale, expected) in [
            ("en", "ru-RU", "en"),
            ("ru", "en-US", "ru"),
            ("system", "RU-ru", "ru"),
            ("unknown", "ru", "ru"),
            ("system", "de-DE", "en"),
            ("system", "", "en"),
            ("RU", "en", "en"),
        ] {
            assert_eq!(resolve_lang(setting, locale), expected);
        }
        assert_eq!(dict("unknown"), dict("en"));
        assert_eq!(
            t("en", "missing {0} {0} {1}", &["$&", "x"]),
            "missing $& $& x"
        );
        assert_eq!(t("ru", "mcpFailBody", &["7464", "busy"]), "Порт 7464: busy");
        assert_eq!(t("en", "mcpFailBody", &["{1}", "busy"]), "Port busy: busy");
        assert_eq!(t("en", "missing", &[]), "missing");
    }
}

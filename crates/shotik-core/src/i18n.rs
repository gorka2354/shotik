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

    /// Reads the literal string entries of the reference (no Node runtime needed by cargo test).
    /// The source has comments, multiple entries per line and escaped single quotes.
    fn reference_dictionary(lang: &str) -> Map<String, Value> {
        let source = include_str!("../../../src/main/strings.js");
        let start = source.find(&format!("  {lang}: {{")).unwrap();
        let block = source[start..]
            .split_once('{')
            .unwrap()
            .1
            .split("\n  },")
            .next()
            .unwrap();
        let mut chars = block.chars().peekable();
        let mut map = Map::new();
        while let Some(c) = chars.next() {
            if c == '/' && chars.peek() == Some(&'/') {
                for ch in chars.by_ref() {
                    if ch == '\n' {
                        break;
                    }
                }
                continue;
            }
            if !c.is_ascii_alphabetic() && c != '_' {
                continue;
            }
            let mut key = c.to_string();
            while chars
                .peek()
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
            {
                key.push(chars.next().unwrap());
            }
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }
            assert_eq!(chars.next(), Some(':'), "reference key {key}");
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }
            let quote = chars.next().unwrap();
            assert!(matches!(quote, '\'' | '"'));
            let mut value = String::new();
            loop {
                let ch = chars.next().expect("closing quote in strings.js");
                if ch == quote {
                    break;
                }
                value.push(if ch == '\\' {
                    match chars.next().unwrap() {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        ch => ch,
                    }
                } else {
                    ch
                });
            }
            assert!(
                map.insert(key, Value::String(value)).is_none(),
                "duplicate reference key"
            );
        }
        map
    }

    #[test]
    fn every_js_key_and_value_survives_in_both_languages() {
        for lang in ["en", "ru"] {
            let reference = reference_dictionary(lang);
            assert_eq!(
                reference.len(),
                232,
                "reference key count changed for {lang}"
            );
            assert_eq!(
                dict(lang).len(),
                reference.len(),
                "{lang} key count vs strings.js"
            );
            assert_eq!(
                dict(lang),
                reference,
                "{lang} keys and full strings vs strings.js"
            );
        }
        assert_eq!(
            dict("en").keys().collect::<Vec<_>>(),
            dict("ru").keys().collect::<Vec<_>>()
        );
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

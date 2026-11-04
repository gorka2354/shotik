//! Translation policy and response parsing (`src/main/translate.js`). No network implementation.

use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ports::{Http, HttpRequest};
use crate::settings::TranslateSettings;
use crate::text::trim;
use crate::{Error, Result};

const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');
const FORM: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'*')
    .remove(b'-')
    .remove(b'.')
    .remove(b'_');

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Translation {
    pub text: String,
    pub source: String,
    pub provider: String,
}

fn parse_response(status: u16, body: &[u8], provider: &str) -> Result<Value> {
    if !(200..300).contains(&status) {
        return Err(Error::msg(if provider == "deepl" {
            format!("DeepL HTTP {status}")
        } else {
            format!("HTTP {status}")
        }));
    }
    // A response may contain user text; errors never include its body.
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| Error::msg("invalid translation JSON response"))?;
    if value.is_null() {
        return Err(Error::msg("invalid null translation response"));
    }
    Ok(value)
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

pub fn free_translate(http: &dyn Http, text: &str, target: &str) -> Result<Translation> {
    let url = format!(
        "https://translate.googleapis.com/translate_a/single?client=gtx&sl=auto&tl={}&dt=t&q={}",
        utf8_percent_encode(target, COMPONENT),
        utf8_percent_encode(text, COMPONENT)
    );
    let response = http.send(HttpRequest {
        method: "GET",
        url: &url,
        headers: &[
            ("User-Agent", "Mozilla/5.0"),
            ("Accept", "application/json"),
        ],
        body: None,
        timeout_ms: 12000,
    })?;
    let data = parse_response(response.status, &response.body, "free")?;
    let mut out = String::new();
    if let Some(segments) = data.get(0).filter(|value| truthy(value)) {
        let segments = segments
            .as_array()
            .ok_or_else(|| Error::msg("invalid translation segments"))?;
        for segment in segments {
            if let Some(text) = segment.get(0).and_then(Value::as_str) {
                out.push_str(text);
            }
        }
    }
    Ok(Translation {
        text: out,
        source: data
            .get(2)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("auto")
            .into(),
        provider: "free".into(),
    })
}

pub fn deepl_translate(
    http: &dyn Http,
    text: &str,
    target: &str,
    key: &str,
) -> Result<Translation> {
    let key = trim(key);
    let host = if key.ends_with(":fx") {
        "https://api-free.deepl.com"
    } else {
        "https://api.deepl.com"
    };
    let url = format!("{host}/v2/translate");
    let authorization = format!("DeepL-Auth-Key {key}");
    let target = target.to_uppercase();
    let body = format!(
        "text={}&target_lang={}",
        utf8_percent_encode(text, FORM),
        utf8_percent_encode(&target, FORM)
    )
    .replace("%20", "+");
    let response = http.send(HttpRequest {
        method: "POST",
        url: &url,
        headers: &[
            ("Authorization", &authorization),
            ("Content-Type", "application/x-www-form-urlencoded"),
        ],
        body: Some(body.as_bytes()),
        timeout_ms: 12000,
    })?;
    let data = parse_response(response.status, &response.body, "deepl")?;
    let translation = &data["translations"][0];
    Ok(Translation {
        text: translation["text"].as_str().unwrap_or("").into(),
        source: translation["detected_source_language"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("auto")
            .to_lowercase(),
        provider: "deepl".into(),
    })
}

/// `stub` is the caller's `SHOTIK_TRANSLATE_STUB == "1"` value.
pub fn translate(
    http: &dyn Http,
    cfg: &TranslateSettings,
    text: &str,
    target: Option<&str>,
    stub: bool,
) -> Result<Translation> {
    let target = target
        .filter(|s| !s.is_empty())
        .unwrap_or(if cfg.target.is_empty() {
            "en"
        } else {
            &cfg.target
        })
        .to_lowercase();
    if trim(text).is_empty() {
        return Ok(Translation {
            text: String::new(),
            source: "auto".into(),
            provider: "none".into(),
        });
    }
    if stub {
        return Ok(Translation {
            text: format!("‹{target}› {text}"),
            source: "stub".into(),
            provider: "stub".into(),
        });
    }
    let key = trim(&cfg.deepl_key);
    if cfg.provider == "deepl" && !key.is_empty() {
        if let Ok(result) = deepl_translate(http, text, &target, key) {
            return Ok(result);
        }
    }
    free_translate(http, text, &target)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::Path;
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::ports::HttpResponse;
    use crate::settings::defaults;

    #[derive(Debug)]
    struct Request {
        method: String,
        url: String,
        headers: Vec<(String, String)>,
        body: Option<String>,
        timeout_ms: u64,
    }

    struct FakeHttp {
        replies: Mutex<VecDeque<Result<HttpResponse>>>,
        requests: Mutex<Vec<Request>>,
    }

    impl FakeHttp {
        fn new(replies: Vec<(u16, Value)>) -> Self {
            Self {
                replies: Mutex::new(
                    replies
                        .into_iter()
                        .map(|(status, json)| {
                            Ok(HttpResponse {
                                status,
                                body: serde_json::to_vec(&json).unwrap(),
                            })
                        })
                        .collect(),
                ),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    impl Http for FakeHttp {
        fn send(&self, request: HttpRequest<'_>) -> Result<HttpResponse> {
            self.requests.lock().unwrap().push(Request {
                method: request.method.into(),
                url: request.url.into(),
                headers: request
                    .headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                body: request.body.map(|b| String::from_utf8(b.to_vec()).unwrap()),
                timeout_ms: request.timeout_ms,
            });
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected HTTP call")
        }
    }

    fn cfg() -> TranslateSettings {
        defaults(Path::new("pictures"), "ru-RU", "win32", None).translate
    }

    // All ten vectors in test/unit/translate.test.js.
    #[test]
    fn free_single_segment_en_to_ru() {
        let http = FakeHttp::new(vec![(
            200,
            json!([[["привет", "hello", null, null, 10]], null, "en"]),
        )]);
        let result = free_translate(&http, "hello", "ru").unwrap();
        assert_eq!(
            result,
            Translation {
                text: "привет".into(),
                source: "en".into(),
                provider: "free".into()
            }
        );
        let requests = http.requests.lock().unwrap();
        assert!(requests[0].url.contains("tl=ru"));
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].timeout_ms, 12000);
        assert!(requests[0].body.is_none());
        assert_eq!(
            requests[0].headers,
            [
                ("User-Agent".into(), "Mozilla/5.0".into()),
                ("Accept".into(), "application/json".into())
            ]
        );
    }

    #[test]
    fn free_joins_multiple_segments() {
        let http = FakeHttp::new(vec![(
            200,
            json!([[["Привет, ", "Hi, "], ["мир", "world"]], null, "en"]),
        )]);
        assert_eq!(
            free_translate(&http, "Hi, world", "ru").unwrap().text,
            "Привет, мир"
        );
    }

    #[test]
    fn free_same_language_echo() {
        let http = FakeHttp::new(vec![(
            200,
            json!([[["привет", "привет", null, null, 5]], null, "ru"]),
        )]);
        let result = free_translate(&http, "привет", "ru").unwrap();
        assert_eq!(result.text, "привет");
        assert_eq!(result.source, "ru");
    }

    #[test]
    fn free_null_segments_are_empty() {
        for segments in [Value::Null, json!(false), json!(0), json!("")] {
            let http = FakeHttp::new(vec![(200, json!([segments, null, "en"]))]);
            assert_eq!(free_translate(&http, "x", "ru").unwrap().text, "");
        }
    }

    #[test]
    fn free_http_429_is_an_error() {
        let http = FakeHttp::new(vec![(429, json!({}))]);
        assert_eq!(
            free_translate(&http, "hello", "ru")
                .unwrap_err()
                .to_string(),
            "HTTP 429"
        );
    }

    #[test]
    fn deepl_first_translation_and_free_host() {
        let http = FakeHttp::new(vec![(
            200,
            json!({"translations": [{"text": "привет", "detected_source_language": "EN"}]}),
        )]);
        let result = deepl_translate(&http, "hello", "ru", "abc:fx").unwrap();
        assert_eq!(
            result,
            Translation {
                text: "привет".into(),
                source: "en".into(),
                provider: "deepl".into()
            }
        );
        assert!(http.requests.lock().unwrap()[0]
            .url
            .starts_with("https://api-free.deepl.com"));
    }

    #[test]
    fn stub_does_not_send_http() {
        let http = FakeHttp::new(vec![]);
        let result = translate(&http, &cfg(), "hello", Some("ru"), true).unwrap();
        assert_eq!(
            result,
            Translation {
                text: "‹ru› hello".into(),
                source: "stub".into(),
                provider: "stub".into()
            }
        );
    }

    #[test]
    fn empty_input_short_circuits() {
        let result = translate(&FakeHttp::new(vec![]), &cfg(), "   ", None, false).unwrap();
        assert_eq!(
            result,
            Translation {
                text: "".into(),
                source: "auto".into(),
                provider: "none".into()
            }
        );
        assert_eq!(
            translate(&FakeHttp::new(vec![]), &cfg(), "\u{feff}\u{a0}", None, true)
                .unwrap()
                .provider,
            "none"
        );
    }

    #[test]
    fn null_deepl_response_falls_through_to_free_as_in_js() {
        let http = FakeHttp::new(vec![
            (200, Value::Null),
            (200, json!([[["ok", "x"]], null, "en"])),
        ]);
        let mut cfg = cfg();
        cfg.provider = "deepl".into();
        cfg.deepl_key = "test:fx".into();
        assert_eq!(
            translate(&http, &cfg, "x", None, false).unwrap().provider,
            "free"
        );
        assert_eq!(http.requests.lock().unwrap().len(), 2);
    }

    #[test]
    fn settings_target_and_free_provider_end_to_end() {
        let http = FakeHttp::new(vec![(200, json!([[["привет", "hello"]], null, "en"]))]);
        assert_eq!(
            translate(&http, &cfg(), "hello", None, false).unwrap().text,
            "привет"
        );
        assert!(http.requests.lock().unwrap()[0].url.contains("tl=ru"));
    }

    #[test]
    fn deepl_failure_falls_through_to_free() {
        let http = FakeHttp::new(vec![
            (403, json!({})),
            (200, json!([[["привет", "hello"]], null, "en"])),
        ]);
        let mut cfg = cfg();
        cfg.provider = "deepl".into();
        cfg.deepl_key = "bad:fx".into();
        let result = translate(&http, &cfg, "hello", None, false).unwrap();
        assert_eq!(result.text, "привет");
        assert_eq!(result.provider, "free");
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].url.contains("deepl.com"));
        assert!(requests[1].url.contains("googleapis.com"));
    }

    #[test]
    fn url_and_form_encoding_match_js_and_pro_key_is_trimmed() {
        let http = FakeHttp::new(vec![(200, json!([[], null, null])), (200, json!({}))]);
        free_translate(&http, "a +&?#/!~*'() привет", "ru&x").unwrap();
        deepl_translate(&http, "a +&~!*'() привет", "en-us", "  test-pro-key  ").unwrap();
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests[0].url, "https://translate.googleapis.com/translate_a/single?client=gtx&sl=auto&tl=ru%26x&dt=t&q=a%20%2B%26%3F%23%2F!~*'()%20%D0%BF%D1%80%D0%B8%D0%B2%D0%B5%D1%82");
        assert_eq!(requests[1].url, "https://api.deepl.com/v2/translate");
        assert_eq!(requests[1].method, "POST");
        assert_eq!(requests[1].timeout_ms, 12000);
        assert_eq!(requests[1].body.as_deref(), Some("text=a+%2B%26%7E%21*%27%28%29+%D0%BF%D1%80%D0%B8%D0%B2%D0%B5%D1%82&target_lang=EN-US"));
        assert!(requests[1]
            .headers
            .contains(&("Authorization".into(), "DeepL-Auth-Key test-pro-key".into())));
        assert!(requests[1].headers.contains(&(
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into()
        )));
    }

    #[test]
    fn empty_target_falls_back_and_missing_deepl_key_skips_deepl() {
        let mut cfg = cfg();
        cfg.target.clear();
        cfg.provider = "deepl".into();
        cfg.deepl_key = " ".into();
        let http = FakeHttp::new(vec![(200, json!([[["x"]]]))]);
        assert_eq!(
            translate(&http, &cfg, "x", Some(""), false).unwrap().source,
            "auto"
        );
        assert!(http.requests.lock().unwrap()[0].url.contains("tl=en"));
        assert_eq!(
            translate(&FakeHttp::new(vec![]), &cfg, " hi ", Some("RU"), true)
                .unwrap()
                .text,
            "‹ru›  hi "
        );
    }

    #[test]
    fn transport_errors_and_invalid_json_propagate_or_fall_through() {
        let http = FakeHttp {
            replies: Mutex::new(VecDeque::from([
                Err(Error::msg("timeout")),
                Ok(HttpResponse {
                    status: 200,
                    body: b"not JSON".to_vec(),
                }),
            ])),
            requests: Mutex::new(Vec::new()),
        };
        let mut cfg = cfg();
        cfg.provider = "deepl".into();
        cfg.deepl_key = "test-key".into();
        assert!(translate(&http, &cfg, "hello", None, false)
            .unwrap_err()
            .to_string()
            .contains("invalid translation JSON"));
        let http = FakeHttp::new(vec![(200, json!({"translations": []}))]);
        assert_eq!(
            deepl_translate(&http, "x", "ru", "test-key").unwrap().text,
            ""
        );
    }
}

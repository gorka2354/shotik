//! The few JavaScript coercions observable in the reference MCP handlers.
use serde_json::Value;

pub(crate) fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(v)) => *v,
        Some(Value::Number(v)) => v.as_f64().is_some_and(|n| n != 0.0),
        Some(Value::String(v)) => !v.is_empty(),
        _ => true,
    }
}

pub(crate) fn string(value: Option<&Value>) -> String {
    match value {
        None => "undefined".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                if v.is_null() {
                    String::new()
                } else {
                    string(Some(v))
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => "[object Object]".into(),
        Some(Value::Number(n)) => number_text(n.as_f64().unwrap_or(f64::NAN)),
        Some(v) => v.to_string(),
    }
}

pub(crate) fn json(value: Option<&Value>) -> String {
    value.map_or_else(|| "undefined".into(), Value::to_string)
}

pub(crate) fn number(value: Option<&Value>) -> f64 {
    match value {
        None | Some(Value::Object(_)) => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(v)) => {
            if *v {
                1.0
            } else {
                0.0
            }
        }
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(v) => {
            let s = string(Some(v));
            let s = s.trim();
            if s.is_empty() {
                return 0.0;
            }
            for (prefix, radix) in [("0x", 16), ("0o", 8), ("0b", 2)] {
                if let Some(digits) = s.to_lowercase().strip_prefix(prefix) {
                    return u64::from_str_radix(digits, radix).map_or(f64::NAN, |n| n as f64);
                }
            }
            match s {
                "Infinity" | "+Infinity" => f64::INFINITY,
                "-Infinity" => f64::NEG_INFINITY,
                // Rust accepts `inf`; JavaScript Number does not.
                _ if s.to_lowercase().contains("inf") => f64::NAN,
                _ => s.parse().unwrap_or(f64::NAN),
            }
        }
    }
}

pub(crate) fn round(n: f64) -> f64 {
    let low = n.floor();
    if n - low < 0.5 {
        low
    } else {
        low + 1.0
    }
}

pub(crate) fn number_text(n: f64) -> String {
    if n == 0.0 {
        "0".into()
    } else if n == f64::INFINITY {
        "Infinity".into()
    } else if n == f64::NEG_INFINITY {
        "-Infinity".into()
    } else {
        n.to_string()
    }
}

/// JSON.stringify omits a trailing `.0` for integral numbers.
pub(crate) fn number_value(n: f64) -> Value {
    if n.is_finite() && n.fract() == 0.0 {
        if n >= i64::MIN as f64 && n < i64::MAX as f64 {
            return Value::from(n as i64);
        }
        if n >= 0.0 && n < u64::MAX as f64 {
            return Value::from(n as u64);
        }
    }
    serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number)
}

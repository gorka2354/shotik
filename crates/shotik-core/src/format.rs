//! Formatting shared by recording notices and MCP replies.

use chrono::{DateTime, Datelike, Utc};

use crate::{Error, Result};

/// `formatDur` from main.js: rounding does not carry 60 seconds into the minutes.
pub fn format_dur(seconds: f64) -> String {
    let minutes = (seconds / 60.0).floor();
    let seconds = js_round(seconds % 60.0);
    let seconds = if seconds == 0.0 {
        "0".into()
    } else {
        seconds.to_string()
    };
    if minutes > 0.0 {
        let minutes = if minutes.is_infinite() {
            "Infinity".into()
        } else {
            minutes.to_string()
        };
        format!("{minutes}:{seconds:0>2}")
    } else {
        format!("{seconds}s")
    }
}

pub(crate) fn js_round(value: f64) -> f64 {
    if !value.is_finite() || value == 0.0 {
        return value;
    }
    let floor = value.floor();
    if value - floor < 0.5 {
        floor
    } else {
        floor + 1.0
    }
}

/// Milliseconds since the epoch, exactly the UTC shape of `new Date(ts).toISOString()`.
pub fn iso_timestamp(timestamp_ms: i64) -> Result<String> {
    if !(-8_640_000_000_000_000..=8_640_000_000_000_000).contains(&timestamp_ms) {
        return Err(Error::msg("timestamp outside supported ISO date range"));
    }
    // JS Date's range exceeds chrono's. Gregorian dates repeat every 400 years, so format
    // within a safe cycle and restore the year. This also handles negative timestamps.
    const CYCLE_MS: i64 = 146_097 * 86_400_000;
    let cycles = timestamp_ms.div_euclid(CYCLE_MS);
    let date = DateTime::<Utc>::from_timestamp_millis(timestamp_ms.rem_euclid(CYCLE_MS))
        .ok_or_else(|| Error::msg("timestamp outside supported ISO date range"))?;
    // ECMAScript uses a signed six-digit year outside 0000..9999 (including -000001).
    let year = i64::from(date.year()) + cycles * 400;
    let year = if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else {
        format!("{year:+07}")
    };
    Ok(format!("{year}{}", date.format("-%m-%dT%H:%M:%S%.3fZ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_rounding_matches_js_including_sixty_without_carry() {
        for (seconds, expected) in [
            (0.0, "0s"),
            (0.49, "0s"),
            (0.5, "1s"),
            (59.4, "59s"),
            (59.5, "60s"),
            (60.0, "1:00"),
            (65.0, "1:05"),
            (119.5, "1:60"),
            (3601.0, "60:01"),
            (-0.5, "0s"),
            (-0.6, "-1s"),
            (f64::NAN, "NaNs"),
            (f64::INFINITY, "Infinity:NaN"),
        ] {
            assert_eq!(format_dur(seconds), expected, "{seconds}");
        }
    }

    #[test]
    fn iso_timestamps_are_utc_with_exactly_three_fraction_digits() {
        for (ts, expected) in [
            (0, "1970-01-01T00:00:00.000Z"),
            (1, "1970-01-01T00:00:00.001Z"),
            (-1, "1969-12-31T23:59:59.999Z"),
            (1709210096789, "2024-02-29T12:34:56.789Z"),
            (-62167219200000, "0000-01-01T00:00:00.000Z"),
            (-62198755200000, "-000001-01-01T00:00:00.000Z"),
            (253402300800000, "+010000-01-01T00:00:00.000Z"),
        ] {
            assert_eq!(iso_timestamp(ts).unwrap(), expected);
        }
        assert!(iso_timestamp(i64::MAX).is_err());
        assert_eq!(
            iso_timestamp(8_640_000_000_000_000).unwrap(),
            "+275760-09-13T00:00:00.000Z"
        );
        assert_eq!(
            iso_timestamp(-8_640_000_000_000_000).unwrap(),
            "-271821-04-20T00:00:00.000Z"
        );
        assert!(iso_timestamp(8_640_000_000_000_001).is_err());
    }
}

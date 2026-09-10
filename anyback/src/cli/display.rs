//! Date formatting for command output and the terminal inspector.

use chrono::{DateTime, FixedOffset, Utc};
use serde_json::Value;

fn offset_label(offset: FixedOffset) -> String {
    let seconds = offset.local_minus_utc();
    if seconds == 0 {
        return "UTC".to_string();
    }
    let sign = if seconds >= 0 { '+' } else { '-' };
    let abs = seconds.unsigned_abs();
    let hours = abs / 3600;
    let minutes = (abs % 3600) / 60;
    format!("{sign}{hours:02}:{minutes:02}")
}

fn format_datetime_with_tz(dt: DateTime<FixedOffset>) -> String {
    format!(
        "{} {}",
        dt.format("%Y-%m-%d %H:%M:%S"),
        offset_label(*dt.offset())
    )
}

fn format_utc_datetime_with_tz(dt: DateTime<Utc>) -> String {
    format!("{} UTC", dt.format("%Y-%m-%d %H:%M:%S"))
}

pub(super) fn format_datetime_display(value: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(format_datetime_with_tz)
}

pub(super) fn format_last_modified(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if let Some(text) = value.as_str() {
        if let Ok(parsed) = DateTime::parse_from_rfc3339(text) {
            return Some(format_datetime_with_tz(parsed));
        }
        return Some(text.to_string());
    }
    if let Some(raw) = value.as_i64() {
        let dt = if raw > 2_000_000_000_000 {
            DateTime::<Utc>::from_timestamp_millis(raw)
        } else {
            DateTime::<Utc>::from_timestamp(raw, 0)
        };
        return dt.map(format_utc_datetime_with_tz);
    }
    #[allow(clippy::cast_possible_truncation)]
    if let Some(raw) = value.as_f64() {
        return format_last_modified(Some(&Value::Number(serde_json::Number::from(raw as i64))));
    }
    Some(value.to_string())
}

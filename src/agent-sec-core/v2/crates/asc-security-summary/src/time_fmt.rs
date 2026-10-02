//! Timestamp rendering for inline display and for the footer's age line.
//!
//! Migrated from v1 `_format_timestamp` and `_time_since_last_event`.

use asc_security_events::timestamp::{NaivePolicy, parse_iso};
use chrono::{DateTime, Local, Utc};

/// Renders an ISO-8601 timestamp in **local** time for inline display.
///
/// An offset-less value is read as UTC, matching v1's
/// `dt.replace(tzinfo=timezone.utc)`. An unparsable value is returned verbatim
/// rather than replaced with a placeholder — v1 shows the raw string so the
/// operator can see what was actually stored.
#[must_use]
pub fn format_timestamp(value: &str) -> String {
    parse_iso(value, "timestamp", NaivePolicy::Utc).map_or_else(
        |_| value.to_owned(),
        |parsed| {
            parsed
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        },
    )
}

/// Renders the age of `value` relative to `now`.
///
/// The buckets are v1's: under a minute is `just now`, then minutes, then hours,
/// then days, each truncated rather than rounded. A timestamp in the future
/// truncates to zero minutes and therefore also reads as `just now`.
#[must_use]
pub fn time_since(value: &str, now: DateTime<Utc>) -> String {
    let Ok(parsed) = parse_iso(value, "timestamp", NaivePolicy::Utc) else {
        return "unknown".to_owned();
    };
    let minutes = (now - parsed).num_seconds() / 60;
    if minutes < 1 {
        return "just now".to_owned();
    }
    if minutes < 60 {
        return format!("{minutes} min ago");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(hour: u32, minute: u32, second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 2, hour, minute, second)
            .single()
            .expect("timestamp")
    }

    #[test]
    fn an_unparsable_timestamp_is_shown_verbatim() {
        assert_eq!(format_timestamp("not-a-time"), "not-a-time");
        assert_eq!(format_timestamp(""), "");
    }

    #[test]
    fn a_parsable_timestamp_renders_in_local_time() {
        let rendered = format_timestamp("2026-01-02T03:04:05+00:00");
        assert_eq!(
            rendered.len(),
            "2026-01-02 03:04:05".len(),
            "the layout is fixed even though the offset is machine local"
        );
        assert!(rendered.contains(':'));
    }

    #[test]
    fn an_offset_less_timestamp_is_read_as_utc() {
        assert_eq!(
            format_timestamp("2026-01-02T03:04:05"),
            format_timestamp("2026-01-02T03:04:05+00:00")
        );
    }

    #[test]
    fn every_age_bucket_is_reachable() {
        let now = at(12, 0, 0);
        assert_eq!(time_since("2026-01-02T11:59:30+00:00", now), "just now");
        assert_eq!(time_since("2026-01-02T11:59:00+00:00", now), "1 min ago");
        assert_eq!(time_since("2026-01-02T11:01:00+00:00", now), "59 min ago");
        assert_eq!(time_since("2026-01-02T11:00:00+00:00", now), "1h ago");
        assert_eq!(time_since("2026-01-01T13:00:00+00:00", now), "23h ago");
        assert_eq!(time_since("2026-01-01T12:00:00+00:00", now), "1d ago");
        assert_eq!(time_since("2025-12-03T12:00:00+00:00", now), "30d ago");
    }

    #[test]
    fn a_future_timestamp_reads_as_just_now() {
        assert_eq!(
            time_since("2026-01-03T00:00:00+00:00", at(12, 0, 0)),
            "just now"
        );
    }

    #[test]
    fn an_unparsable_timestamp_has_an_unknown_age() {
        assert_eq!(time_since("nope", at(12, 0, 0)), "unknown");
    }
}

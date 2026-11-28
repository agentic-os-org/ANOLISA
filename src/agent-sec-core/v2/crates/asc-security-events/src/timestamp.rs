//! ISO-8601 timestamp normalization mirroring `agent_sec_cli.utils.timestamp`.
//!
//! Byte-level output compatibility with Python is a hard requirement here: the
//! same event written by v1 and v2 must produce identical `timestamp` strings,
//! so this module reproduces `datetime.isoformat()` formatting rules rather
//! than using `chrono`'s RFC 3339 helpers.

use chrono::{
    DateTime, FixedOffset, Local, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Utc,
};

use crate::error::TimestampError;

/// How to interpret a timestamp that carries no UTC offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NaivePolicy {
    /// Interpret as local wall-clock time (v1 default for user-facing input).
    Local,
    /// Interpret as UTC.
    Utc,
    /// Refuse to guess; used at repository boundaries.
    Reject,
}

/// Formats a UTC instant exactly as Python's `datetime.isoformat()` does.
///
/// Python omits the fractional part entirely when `microsecond == 0` and
/// otherwise prints exactly six digits. `chrono`'s `SecondsFormat::Micros`
/// always prints six, and `AutoSi` may print three, so neither matches on its
/// own.
#[must_use]
pub fn format_utc_iso(value: DateTime<Utc>) -> String {
    let format = if value.timestamp_subsec_micros() == 0 {
        SecondsFormat::Secs
    } else {
        SecondsFormat::Micros
    };
    value.to_rfc3339_opts(format, false)
}

/// Returns the current time formatted like v1's `_now_iso()`.
#[must_use]
pub fn now_iso() -> String {
    format_utc_iso(Utc::now())
}

/// Parses an ISO-8601 timestamp and applies *naive* to offset-less input.
///
/// The accepted grammar mirrors v1's `datetime.fromisoformat` (Python 3.11):
/// beyond RFC 3339, a bare date resolves to midnight, a space is accepted as
/// the date-time separator, seconds and the offset's colon may be omitted,
/// the offset may be compact (`+0800`) or hours-only (`+08`), the fractional
/// separator may be a comma, and the basic `20260102T030405` layout parses.
///
/// # Errors
///
/// Returns [`TimestampError::InvalidFormat`] when the value is not ISO-8601,
/// and [`TimestampError::MissingTimezone`] when it lacks an offset and
/// *naive* is [`NaivePolicy::Reject`].
pub fn parse_iso(
    value: &str,
    field_name: &str,
    naive: NaivePolicy,
) -> Result<DateTime<Utc>, TimestampError> {
    let normalized = normalize_wire_form(value);

    if let Some(parsed) = parse_offset_aware(&normalized) {
        return Ok(parsed.with_timezone(&Utc));
    }

    let naive_parsed = parse_naive(&normalized).ok_or_else(|| TimestampError::InvalidFormat {
        field: field_name.to_owned(),
        value: value.to_owned(),
    })?;

    match naive {
        NaivePolicy::Local => Ok(local_to_utc(naive_parsed, field_name)?),
        NaivePolicy::Utc => Ok(Utc.from_utc_datetime(&naive_parsed)),
        NaivePolicy::Reject => Err(TimestampError::MissingTimezone {
            field: field_name.to_owned(),
        }),
    }
}

/// Normalizes an ISO timestamp to a UTC-aware ISO string.
///
/// # Errors
///
/// Propagates [`parse_iso`] failures.
pub fn normalize_iso_to_utc_iso(
    value: &str,
    field_name: &str,
    naive: NaivePolicy,
) -> Result<String, TimestampError> {
    Ok(format_utc_iso(parse_iso(value, field_name, naive)?))
}

/// Converts a UTC-normalized ISO timestamp to epoch seconds.
///
/// Stricter than user-facing parsing on purpose: callers must normalize before
/// crossing the repository boundary, mirroring v1 `utc_iso_to_epoch`. Like
/// v1, the offset-bearing spellings `fromisoformat` accepts (compact, hours
/// only, space separated) parse here before the UTC check rejects them.
///
/// # Errors
///
/// Returns [`TimestampError::MissingTimezone`] for offset-less input,
/// [`TimestampError::NotUtc`] when the offset is not zero, and
/// [`TimestampError::InvalidFormat`] for unparsable input.
pub fn utc_iso_to_epoch(value: &str, field_name: &str) -> Result<f64, TimestampError> {
    let normalized = normalize_wire_form(value);
    let parsed = parse_offset_aware(&normalized).ok_or_else(|| {
        // Distinguish "no offset at all" from "malformed" the way v1 does: it
        // parses first and only then checks the offset.
        if parse_naive(&normalized).is_some() {
            TimestampError::MissingTimezone {
                field: field_name.to_owned(),
            }
        } else {
            TimestampError::InvalidFormat {
                field: field_name.to_owned(),
                value: value.to_owned(),
            }
        }
    })?;

    if parsed.offset().local_minus_utc() != 0 {
        return Err(TimestampError::NotUtc {
            field: field_name.to_owned(),
        });
    }

    let utc = parsed.with_timezone(&Utc);
    let subsec = f64::from(utc.timestamp_subsec_micros()) / 1_000_000.0;
    #[allow(clippy::cast_precision_loss)] // Epoch seconds fit f64 exactly well past year 3000.
    let seconds = utc.timestamp() as f64;
    Ok(seconds + subsec)
}

/// Converts epoch seconds to a UTC-aware ISO timestamp (v1 `epoch_to_utc_iso`).
///
/// # Errors
///
/// Returns [`TimestampError::InvalidEpoch`] when the value is not representable.
pub fn epoch_to_utc_iso(epoch: f64) -> Result<String, TimestampError> {
    // Largest magnitude that survives the f64 -> i64 conversion below; using a
    // literal avoids an `i64 as f64` cast that would itself lose precision.
    const MICROS_LIMIT: f64 = 9.223_372_036_854_775e18;

    let micros = (epoch * 1_000_000.0).round();
    if !micros.is_finite() || micros.abs() > MICROS_LIMIT {
        return Err(TimestampError::InvalidEpoch { epoch });
    }

    #[allow(clippy::cast_possible_truncation)] // Range-checked immediately above.
    let micros_i64 = micros as i64;
    DateTime::from_timestamp_micros(micros_i64)
        .map(format_utc_iso)
        .ok_or(TimestampError::InvalidEpoch { epoch })
}

fn local_to_utc(value: NaiveDateTime, field_name: &str) -> Result<DateTime<Utc>, TimestampError> {
    // `astimezone()` on a naive datetime resolves ambiguity toward the first
    // (earlier) offset, which matches `earliest()` here.
    Local
        .from_local_datetime(&value)
        .earliest()
        .map(|local| local.with_timezone(&Utc))
        .ok_or_else(|| TimestampError::InvalidFormat {
            field: field_name.to_owned(),
            value: value.to_string(),
        })
}

fn parse_naive(value: &str) -> Option<NaiveDateTime> {
    for pattern in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
        "%Y%m%dT%H%M%S%.f",
        "%Y%m%dT%H%M",
    ] {
        if let Ok(parsed) = NaiveDateTime::parse_from_str(value, pattern) {
            return Some(parsed);
        }
    }
    parse_date_midnight(value)
}

/// Python's `fromisoformat` reads a bare date as midnight, in both the
/// extended `2026-01-02` and basic `20260102` spelling.
fn parse_date_midnight(value: &str) -> Option<NaiveDateTime> {
    let extended = if value.len() == 8 && value.bytes().all(|b| b.is_ascii_digit()) {
        format!("{}-{}-{}", &value[0..4], &value[4..6], &value[6..8])
    } else if value.len() == 10
        && value.as_bytes()[4] == b'-'
        && value.as_bytes()[7] == b'-'
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
    {
        value.to_owned()
    } else {
        return None;
    };
    NaiveDate::parse_from_str(&extended, "%Y-%m-%d")
        .ok()?
        .and_hms_opt(0, 0, 0)
}

/// Parses the offset-bearing spellings Python 3.11's `fromisoformat` accepts
/// beyond RFC 3339: the space date-time separator, omitted seconds, compact
/// (`+0800`) and hours-only (`+08`) offsets, and the basic `20260102T030405`
/// layout.
fn parse_offset_aware(value: &str) -> Option<DateTime<FixedOffset>> {
    if let Ok(parsed) = DateTime::<FixedOffset>::parse_from_rfc3339(value) {
        return Some(parsed);
    }
    for pattern in [
        "%Y-%m-%dT%H:%M:%S%.f%#z",
        "%Y-%m-%d %H:%M:%S%.f%#z",
        "%Y-%m-%dT%H:%M%#z",
        "%Y-%m-%d %H:%M%#z",
        "%Y%m%dT%H%M%S%.f%#z",
        "%Y%m%dT%H%M%#z",
    ] {
        if let Ok(parsed) = DateTime::parse_from_str(value, pattern) {
            return Some(parsed);
        }
    }
    None
}

fn normalize_z_suffix(value: &str) -> String {
    if let Some(stripped) = value.strip_suffix('Z') {
        format!("{stripped}+00:00")
    } else {
        value.to_owned()
    }
}

/// Python accepts `Z` and a `,` fractional separator; both become spellings
/// the shared pattern list understands.
fn normalize_wire_form(value: &str) -> String {
    normalize_z_suffix(value).replace(',', ".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_microseconds_omits_fractional_part() {
        // Python: datetime(2026, 1, 2, 3, 4, 5, tzinfo=utc).isoformat()
        //         == "2026-01-02T03:04:05+00:00"
        let parsed = parse_iso("2026-01-02T03:04:05Z", "timestamp", NaivePolicy::Reject).unwrap();
        assert_eq!(format_utc_iso(parsed), "2026-01-02T03:04:05+00:00");
    }

    #[test]
    fn millisecond_input_is_padded_to_six_digits() {
        // Python keeps microsecond resolution, so ".123" round-trips as ".123000".
        let parsed = parse_iso("2026-01-02T03:04:05.123Z", "timestamp", NaivePolicy::Reject)
            .expect("millisecond precision parses");
        assert_eq!(format_utc_iso(parsed), "2026-01-02T03:04:05.123000+00:00");
    }

    #[test]
    fn offset_is_converted_to_utc() {
        let normalized = normalize_iso_to_utc_iso(
            "2026-01-02T11:04:05+08:00",
            "timestamp",
            NaivePolicy::Reject,
        )
        .unwrap();
        assert_eq!(normalized, "2026-01-02T03:04:05+00:00");
    }

    #[test]
    fn naive_input_is_rejected_under_reject_policy() {
        let err = parse_iso("2026-01-02T03:04:05", "timestamp", NaivePolicy::Reject)
            .expect_err("naive input must be rejected");
        assert!(matches!(err, TimestampError::MissingTimezone { .. }));
    }

    #[test]
    fn naive_input_is_treated_as_utc_under_utc_policy() {
        let parsed = parse_iso("2026-01-02T03:04:05", "timestamp", NaivePolicy::Utc).unwrap();
        assert_eq!(format_utc_iso(parsed), "2026-01-02T03:04:05+00:00");
    }

    #[test]
    fn invalid_input_is_reported_with_field_and_value() {
        let err = parse_iso("not-a-time", "timestamp", NaivePolicy::Local)
            .expect_err("garbage must not parse");
        match err {
            TimestampError::InvalidFormat { field, value } => {
                assert_eq!(field, "timestamp");
                assert_eq!(value, "not-a-time");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn epoch_round_trip_keeps_microseconds() {
        let iso = epoch_to_utc_iso(1_767_322_845.123_456).unwrap();
        assert_eq!(iso, "2026-01-02T03:00:45.123456+00:00");
        let epoch = utc_iso_to_epoch(&iso, "timestamp").unwrap();
        assert!((epoch - 1_767_322_845.123_456).abs() < 1e-6);
    }

    #[test]
    fn epoch_conversion_drops_fraction_when_whole_second() {
        assert_eq!(
            epoch_to_utc_iso(1_767_322_845.0).unwrap(),
            "2026-01-02T03:00:45+00:00"
        );
    }

    #[test]
    fn utc_iso_to_epoch_rejects_non_utc_offset() {
        let err = utc_iso_to_epoch("2026-01-02T03:04:05+08:00", "timestamp")
            .expect_err("non-UTC offsets must be rejected");
        assert!(matches!(err, TimestampError::NotUtc { .. }));
    }

    #[test]
    fn utc_iso_to_epoch_rejects_naive_input() {
        let err = utc_iso_to_epoch("2026-01-02T03:04:05", "timestamp")
            .expect_err("naive input must be rejected");
        assert!(matches!(err, TimestampError::MissingTimezone { .. }));
    }

    // Python-oracle cases: `datetime.fromisoformat` (Python 3.11) accepts each
    // spelling below, so v1's `parse_iso_datetime` serves it while v2 rejected
    // it with `InvalidFormat` before these patterns existed.

    #[test]
    fn date_only_input_resolves_to_midnight() {
        // datetime.fromisoformat("2026-01-02") == datetime(2026, 1, 2)
        let normalized =
            normalize_iso_to_utc_iso("2026-01-02", "timestamp", NaivePolicy::Utc).unwrap();
        assert_eq!(normalized, "2026-01-02T00:00:00+00:00");
        let err = parse_iso("2026-01-02", "timestamp", NaivePolicy::Reject)
            .expect_err("a bare date carries no offset");
        assert!(matches!(err, TimestampError::MissingTimezone { .. }));
    }

    #[test]
    fn compact_and_hours_only_offsets_are_converted() {
        for value in [
            "2026-01-02T03:04:05+08",
            "2026-01-02T03:04:05+0800",
            "2026-01-02 03:04:05+0800",
        ] {
            let normalized =
                normalize_iso_to_utc_iso(value, "timestamp", NaivePolicy::Reject).unwrap();
            assert_eq!(normalized, "2026-01-01T19:04:05+00:00", "{value}");
        }
    }

    #[test]
    fn comma_fraction_separator_parses_like_a_dot() {
        let normalized = normalize_iso_to_utc_iso(
            "2026-01-02T03:04:05,123+00:00",
            "timestamp",
            NaivePolicy::Reject,
        )
        .unwrap();
        assert_eq!(normalized, "2026-01-02T03:04:05.123000+00:00");
    }

    #[test]
    fn basic_format_input_parses_like_python() {
        assert_eq!(
            normalize_iso_to_utc_iso("20260102T030405+0800", "timestamp", NaivePolicy::Reject)
                .unwrap(),
            "2026-01-01T19:04:05+00:00"
        );
        assert_eq!(
            normalize_iso_to_utc_iso("20260102T0304+0800", "timestamp", NaivePolicy::Reject)
                .unwrap(),
            "2026-01-01T19:04:00+00:00"
        );
        let parsed = parse_iso("20260102", "timestamp", NaivePolicy::Utc).unwrap();
        assert_eq!(format_utc_iso(parsed), "2026-01-02T00:00:00+00:00");
    }

    #[test]
    fn utc_iso_to_epoch_accepts_v1_offset_spellings() {
        for value in [
            "2026-01-02T03:04:05+00:00",
            "2026-01-02T03:04:05+0000",
            "2026-01-02T03:04:05+00",
        ] {
            let epoch = utc_iso_to_epoch(value, "timestamp").unwrap();
            assert!((epoch - 1_767_323_045.0).abs() < 1e-6, "{value}");
        }
        // A non-zero offset in any spelling is still a NotUtc rejection.
        let err = utc_iso_to_epoch("2026-01-02T03:04:05+0800", "timestamp")
            .expect_err("non-UTC offsets must be rejected");
        assert!(matches!(err, TimestampError::NotUtc { .. }));
        // A bare date carries no offset: the boundary still refuses to guess.
        let err = utc_iso_to_epoch("2026-01-02", "timestamp")
            .expect_err("date-only input has no timezone");
        assert!(matches!(err, TimestampError::MissingTimezone { .. }));
    }
}

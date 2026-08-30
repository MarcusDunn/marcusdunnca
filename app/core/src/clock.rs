//! Time formatting, pinned.
//!
//! Every timestamp written to this table is RFC 3339 in UTC, because sort keys
//! and `created_at` are compared as *strings*. A local-time or non-padded
//! format would sort wrongly and the failure would be invisible until an
//! attempt landed in the wrong place in history. There is exactly one function
//! that produces a timestamp so there is exactly one format.

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// RFC 3339 UTC with millisecond precision, e.g. `2026-08-29T18:04:11.123Z`.
///
/// Sub-second precision matters for attempt sort keys: two submissions in the
/// same second would otherwise collide on `ATTEMPT#<iso>` and the second would
/// overwrite the first. `PutItem` is an upsert, so that loss would be silent.
///
/// Truncated to milliseconds rather than left at the nanosecond precision
/// `time` produces, because every one of these strings is parsed by
/// `Date.parse` in the browser and read by `z.iso.datetime()`. Milliseconds are
/// what JavaScript can represent; the extra six digits are decoration that
/// makes a value look more precise than the system it describes.
pub fn now_iso8601() -> String {
    format_rfc3339(OffsetDateTime::now_utc())
}

/// The same format, for a point computed from a Unix timestamp — presigned URL
/// expiry and session expiry are both derived rather than "now".
pub fn iso_at(unix_seconds: i64) -> String {
    OffsetDateTime::from_unix_timestamp(unix_seconds)
        .map(format_rfc3339)
        // Only reachable for timestamps outside year ±9999. Falling back to now
        // rather than propagating an error: a slightly wrong expiry makes the
        // client re-authenticate, whereas a 500 on the login path locks the
        // user out entirely.
        .unwrap_or_else(|_| now_iso8601())
}

fn format_rfc3339(t: OffsetDateTime) -> String {
    let millis = t.millisecond();
    t.replace_nanosecond(u32::from(millis) * 1_000_000)
        .unwrap_or(t)
        .format(&Rfc3339)
        // `Rfc3339` cannot fail for an `OffsetDateTime` — the format has no
        // components an offset-aware value lacks. Falling back rather than
        // unwrapping so a hypothetical formatter change cannot take down a
        // running function; a submission with a degraded timestamp is better
        // than a 500.
        .unwrap_or_else(|_| format!("{}", t.unix_timestamp()))
}

/// `YYYY-MM-DD` in UTC. The bucket the daily generation cap counts against.
///
/// UTC, not local: the counter row is keyed by this string, and a
/// timezone-dependent key would give the cap a variable-length day and a seam
/// where two rows are live at once.
pub fn today_utc() -> String {
    let now = OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02}",
        now.year(),
        u8::from(now.month()),
        now.day()
    )
}

pub fn unix_now() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_sort_chronologically_as_strings() {
        // The entire attempt sort-key design rests on this.
        let a = "2026-08-29T09:00:00Z";
        let b = "2026-08-29T10:00:00Z";
        let c = "2026-09-01T00:00:00Z";
        assert!(a < b && b < c);
    }

    #[test]
    fn today_is_ten_characters_and_zero_padded() {
        let d = today_utc();
        assert_eq!(d.len(), 10, "day key must be fixed width: {d}");
        assert!(d.as_bytes()[4] == b'-' && d.as_bytes()[7] == b'-');
    }

    #[test]
    fn now_is_rfc3339() {
        assert!(OffsetDateTime::parse(&now_iso8601(), &Rfc3339).is_ok());
    }

    /// The browser parses these with `Date.parse` and validates them with
    /// `z.iso.datetime()`, which wants a `Z`-terminated string with at most
    /// millisecond precision.
    #[test]
    fn timestamps_are_z_terminated_millisecond_precision() {
        for s in [now_iso8601(), iso_at(1_800_000_000)] {
            assert!(s.ends_with('Z'), "not UTC-terminated: {s}");
            if let Some((_, frac)) = s.rsplit_once('.') {
                let digits = frac.trim_end_matches('Z');
                assert!(digits.len() <= 3, "sub-millisecond precision in {s}");
            }
        }
    }
}

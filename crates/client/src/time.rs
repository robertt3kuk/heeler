//! How Hutan writes a point in time for people (`docs/design.md` § Time).
//!
//! One convention everywhere: a time is told **relative** ("3 min ago", [`relative_text`]) and
//! the **absolute local time with its zone** ("2026-10-17 14:31 +0500", [`local_time_text`]) is
//! what a tooltip shows or a command prints next to it. Never UTC without saying so, never a
//! bare clock time that leaves out the day.

use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// `2026-10-17 14:31 +0500`: `ms` in the user's time zone, to the minute.
pub fn local_time_text(ms: u64) -> String {
    format_at(ms, utc_offset_secs(ms), Precision::Minutes)
}

/// `2026-10-17 14:31:07 +0500`: like [`local_time_text`], to the second.
pub fn local_time_seconds_text(ms: u64) -> String {
    format_at(ms, utc_offset_secs(ms), Precision::Seconds)
}

/// `just now`, `3 min ago`, `2 h ago`, `yesterday`, `5 days ago`: how long before `now_ms`
/// `then_ms` was. A time in the future (clocks differ) reads `just now`.
pub fn relative_text(now_ms: u64, then_ms: u64) -> String {
    let secs = now_ms.saturating_sub(then_ms) / 1000;
    match secs {
        0..=59 => "just now".to_owned(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        86_400..=172_799 => "yesterday".to_owned(),
        _ => format!("{} days ago", secs / 86_400),
    }
}

/// `3 min ago` against the clock now.
pub fn relative_to_now(then_ms: u64) -> String {
    relative_text(now_ms(), then_ms)
}

#[derive(Clone, Copy)]
enum Precision {
    Minutes,
    Seconds,
}

/// The offset of the user's time zone from UTC at `ms`, in seconds east.
#[allow(unsafe_code)] // `localtime_r` on a stack value; it reads the time zone, nothing else.
fn utc_offset_secs(ms: u64) -> i64 {
    let Ok(seconds) = libc::time_t::try_from(ms / 1000) else {
        return 0;
    };
    // SAFETY: `tm` is plain integers (and a pointer `localtime_r` fills in or leaves null); a
    // zeroed one is valid. `localtime_r` writes only into the `tm` it is given and reads
    // `seconds` by pointer for the length of the call.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&raw const seconds, &raw mut tm).is_null() {
            return 0;
        }
        tm
    };
    tm.tm_gmtoff
}

fn format_at(ms: u64, offset_secs: i64, precision: Precision) -> String {
    let local = i64::try_from(ms / 1000).unwrap_or(0) + offset_secs;
    let (days, rem) = (local.div_euclid(86_400), local.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let (hour, minute, second) = (rem / 3600, rem % 3600 / 60, rem % 60);
    let sign = if offset_secs < 0 { '-' } else { '+' };
    let off = offset_secs.abs();
    let zone = format!("{sign}{:02}{:02}", off / 3600, off % 3600 / 60);
    match precision {
        Precision::Minutes => format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02} {zone}"),
        Precision::Seconds => {
            format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} {zone}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-10-14T12:31:07Z
    const INSTANT: u64 = 1_791_981_067_000;

    #[test]
    fn the_local_time_carries_its_zone() {
        assert_eq!(
            format_at(INSTANT, 0, Precision::Seconds),
            "2026-10-14 12:31:07 +0000"
        );
        assert_eq!(
            format_at(INSTANT, 5 * 3600, Precision::Minutes),
            "2026-10-14 17:31 +0500"
        );
        // Past midnight in the zone, and a zone with a half hour.
        assert_eq!(
            format_at(INSTANT, 12 * 3600, Precision::Minutes),
            "2026-10-15 00:31 +1200"
        );
        assert_eq!(
            format_at(INSTANT, -(3 * 3600 + 1800), Precision::Minutes),
            "2026-10-14 09:01 -0330"
        );
        assert_eq!(
            format_at(0, 0, Precision::Seconds),
            "1970-01-01 00:00:00 +0000"
        );
        // Leap day.
        assert_eq!(
            format_at(1_709_210_096_000, 0, Precision::Seconds),
            "2024-02-29 12:34:56 +0000"
        );
    }

    #[test]
    fn the_system_zone_text_has_the_same_shape() {
        let text = local_time_text(INSTANT);
        assert_eq!(text.len(), "2026-10-14 12:31 +0000".len(), "{text}");
        assert!(text.starts_with("2026-10-1"), "{text}");
        let seconds = local_time_seconds_text(INSTANT);
        assert_eq!(
            seconds.len(),
            "2026-10-14 12:31:07 +0000".len(),
            "{seconds}"
        );
    }

    #[test]
    fn relative_times_round_down_to_one_unit() {
        let now = 10 * 86_400_000;
        assert_eq!(relative_text(now, now), "just now");
        assert_eq!(relative_text(now, now - 59_000), "just now");
        assert_eq!(relative_text(now, now - 60_000), "1 min ago");
        assert_eq!(relative_text(now, now - 3 * 60_000 - 59_000), "3 min ago");
        assert_eq!(relative_text(now, now - 2 * 3_600_000), "2 h ago");
        assert_eq!(relative_text(now, now - 30 * 3_600_000), "yesterday");
        assert_eq!(relative_text(now, now - 5 * 86_400_000), "5 days ago");
        assert_eq!(relative_text(now, now + 5_000), "just now");
    }
}

//! Calendar-arithmetic helpers: [`today_iso`] (`YYYY-MM-DD`) and
//! [`now_iso`] (`YYYY-MM-DDTHH:MM:SSZ`) from the system clock,
//! using Hinnant's `civil_from_days` algorithm.

use std::time::{SystemTime, UNIX_EPOCH};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Seconds per day.
const SECS_PER_DAY: u64 = 86_400;

// -----------------------------------------------------------------------------
// Timestamp Formatting
// -----------------------------------------------------------------------------

/// Render the current system time as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn now_iso() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(format_iso)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

/// Render today's date as `YYYY-MM-DD`.
pub fn today_iso() -> String {
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return "1970-01-01".to_owned();
    };

    let days_since_epoch = (now.as_secs() / SECS_PER_DAY) as i64;
    let (year, month, day) = civil_from_days(days_since_epoch);

    format!("{year:04}-{month:02}-{day:02}")
}

/// Days since Unix epoch to `(year, month, day)`.
pub fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    (y as i32, m as u32, d as u32)
}

// -----------------------------------------------------------------------------
// ISO Rendering
// -----------------------------------------------------------------------------

/// Format a `Duration`-since-Unix-epoch as `YYYY-MM-DDTHH:MM:SSZ`.
fn format_iso(d: std::time::Duration) -> String {
    let secs = d.as_secs() as i64;
    let days_since_epoch = secs.div_euclid(SECS_PER_DAY as i64);
    let secs_in_day = secs.rem_euclid(SECS_PER_DAY as i64) as u32;

    let (year, month, day) = civil_from_days(days_since_epoch);
    let hour = secs_in_day / 3_600;
    let minute = (secs_in_day % 3_600) / 60;
    let second = secs_in_day % 60;

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1), "epoch day 0 should be 1970-01-01");
        assert_eq!(civil_from_days(11_323), (2001, 1, 1), "day 11323 should be 2001-01-01");
        assert_eq!(civil_from_days(20_575), (2026, 5, 2), "day 20575 should be 2026-05-02");
        assert_eq!(civil_from_days(-1), (1969, 12, 31), "day -1 should be 1969-12-31");
    }

    #[test]
    fn today_iso_format_is_well_shaped() {
        let s = today_iso();
        assert_eq!(s.len(), 10, "today_iso should be `YYYY-MM-DD`, got: {s}");
        let bytes = s.as_bytes();
        assert!(
            bytes[4] == b'-' && bytes[7] == b'-',
            "expected dashes at positions 4 and 7: {s}"
        );
    }

    #[test]
    fn now_iso_format_is_well_shaped() {
        let s = now_iso();
        assert_eq!(s.len(), 20, "now_iso should be `YYYY-MM-DDTHH:MM:SSZ`, got: {s}");
        assert!(s.ends_with('Z'), "now_iso should end with Z (UTC), got: {s}");
        let bytes = s.as_bytes();
        assert_eq!(bytes[10], b'T', "expected T separator at position 10: {s}");
    }

    #[test]
    fn format_iso_round_trips_known_epoch() {
        assert_eq!(
            format_iso(std::time::Duration::from_secs(0)),
            "1970-01-01T00:00:00Z",
            "epoch should format as 1970-01-01T00:00:00Z",
        );
        assert_eq!(
            format_iso(std::time::Duration::from_secs(1_777_189_211)),
            "2026-04-26T07:40:11Z",
            "a known second count should format to its ISO equivalent",
        );
    }
}

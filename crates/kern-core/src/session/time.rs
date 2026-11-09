//! A JS `Date.prototype.toISOString()` for the header's `createdAt`, without a
//! date crate: the caller passes the clock, this only formats it.

/// Milliseconds since the epoch -> `2026-11-10T08:30:00.000Z` (UTC, millisecond precision).
pub fn iso_utc(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        ms % 1000
    )
}

/// Days since 1970-01-01 -> (year, month, day), proleptic Gregorian (H. Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_to_iso_string() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00.000Z");
        // 2024-02-29 (a leap day), 13:05:09.007 UTC.
        assert_eq!(iso_utc(1_709_211_909_007), "2024-02-29T13:05:09.007Z");
        assert_eq!(iso_utc(1_762_732_800_000), "2025-11-10T00:00:00.000Z");
    }
}

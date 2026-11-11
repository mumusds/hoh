//! UTC timestamps in the RFC 3339 form the contracts use (`date-time`).

use std::time::{SystemTime, UNIX_EPOCH};

/// `YYYY-MM-DDTHH:MM:SSZ` for a point in time; times before 1970 are clamped to the epoch.
pub fn rfc3339_utc(at: SystemTime) -> String {
    let secs = at
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rest) = (secs / 86_400, secs % 86_400);
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

/// The current time, see [`rfc3339_utc`].
pub fn now_rfc3339() -> String {
    rfc3339_utc(SystemTime::now())
}

/// `at` in UTC to the millisecond, fixed width (`YYYY-MM-DDTHH:MM:SS.mmmZ`), so two such times compare as text: the
/// supervisor's trigger times, which REQ-021's debounce needs finer than a second (docs/adr/ADR-025.md decision 2).
pub fn rfc3339_millis_utc(at: SystemTime) -> String {
    let base = rfc3339_utc(at);
    let millis = at
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_millis())
        .unwrap_or(0);
    format!("{}.{millis:03}Z", base.trim_end_matches('Z'))
}

/// The instant of a contract `date-time` to the millisecond: [`parse_rfc3339`] and its fraction's first three digits.
pub fn parse_rfc3339_millis(text: &str) -> Option<SystemTime> {
    let whole = parse_rfc3339(text)?;
    let fraction: String = text
        .get(19..)
        .and_then(|rest| rest.strip_prefix('.'))
        .map(|digits| digits.chars().take_while(char::is_ascii_digit).collect())
        .unwrap_or_default();
    let millis: u64 = fraction
        .chars()
        .chain(std::iter::repeat('0'))
        .take(3)
        .collect::<String>()
        .parse()
        .ok()?;
    Some(whole + std::time::Duration::from_millis(millis))
}

/// The latest instant the store's fixed-width form can write: 9999-12-31T23:59:59Z.
const LAST: u64 = 253_402_300_799;

/// The instant of a contract `date-time` (exactly what [`crate::schema::is_date_time`] accepts: RFC 3339
/// `YYYY-MM-DDTHH:MM:SS[.frac](Z|±HH:MM)`, `T`/`Z` in either case, no leap second), the fraction cut to the second;
/// `None` for anything else, a time before 1970, or one past 9999 once in UTC (M2T03R-100, -101). Times callers give
/// are compared only after this (M2T03R-01): as text, an offset other than `Z` would compare wrongly.
pub fn parse_rfc3339(text: &str) -> Option<SystemTime> {
    if !crate::schema::is_date_time(text) {
        return None;
    }
    let b = text.as_bytes();
    let digits = |from: usize, len: usize| -> Option<i64> {
        let part = b.get(from..from + len)?;
        part.iter().try_fold(0_i64, |n, c| {
            c.is_ascii_digit().then(|| n * 10 + i64::from(c - b'0'))
        })
    };
    let sep = |at: usize, c: u8| b.get(at) == Some(&c);
    if !(sep(4, b'-')
        && sep(7, b'-')
        && matches!(b.get(10), Some(b'T' | b't'))
        && sep(13, b':')
        && sep(16, b':'))
    {
        return None;
    }
    let (year, month, day) = (digits(0, 4)?, digits(5, 2)?, digits(8, 2)?);
    let (hour, minute, second) = (digits(11, 2)?, digits(14, 2)?, digits(17, 2)?);
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let days_in = usize::try_from(month - 1)
        .ok()
        .and_then(|m| month_days.get(m))?;
    if !(1..=*days_in).contains(&day) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let mut at = 19;
    if sep(at, b'.') {
        at += 1;
        let start = at;
        while b.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        if at == start {
            return None;
        }
    }
    let offset = match b.get(at) {
        Some(b'Z' | b'z') if at + 1 == b.len() => 0,
        Some(sign @ (b'+' | b'-')) if at + 6 == b.len() && sep(at + 3, b':') => {
            let (oh, om) = (digits(at + 1, 2)?, digits(at + 4, 2)?);
            if oh > 23 || om > 59 {
                return None;
            }
            let minutes = oh * 60 + om;
            if *sign == b'+' { minutes } else { -minutes }
        }
        _ => return None,
    };
    let secs = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second
        - offset * 60;
    u64::try_from(secs)
        .ok()
        .filter(|s| *s <= LAST)
        .map(|s| UNIX_EPOCH + std::time::Duration::from_secs(s))
}

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Proleptic Gregorian date of a day count since 1970-01-01 (H. Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_millisecond_time_round_trips_and_compares_as_text() {
        // docs/adr/ADR-025.md decision 2: the supervisor's trigger times.
        let at = UNIX_EPOCH + Duration::from_millis(1_793_754_123_045);
        assert_eq!(rfc3339_millis_utc(at), "2026-11-04T01:02:03.045Z");
        assert_eq!(parse_rfc3339_millis("2026-11-04T01:02:03.045Z"), Some(at));
        assert_eq!(
            parse_rfc3339_millis("2026-11-04T01:02:03Z"),
            Some(at - Duration::from_millis(45))
        );
        assert_eq!(parse_rfc3339_millis("2026-11-04T01:02:03.0459Z"), Some(at));
        assert!(
            rfc3339_millis_utc(at + Duration::from_millis(955))
                > rfc3339_millis_utc(at + Duration::from_millis(100))
        );
    }

    #[test]
    fn formats_known_instants() {
        let at = |secs: u64| rfc3339_utc(UNIX_EPOCH + Duration::from_secs(secs));
        assert_eq!(at(0), "1970-01-01T00:00:00Z");
        assert_eq!(at(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(at(1_793_754_123), "2026-11-04T01:02:03Z");
        assert_eq!(at(4_102_444_799), "2099-12-31T23:59:59Z");
    }

    #[test]
    fn parses_offsets_fractions_and_refuses_what_is_not_a_date_time() {
        // M2T03R-01: an offset is the same instant in UTC; a fraction is cut to the second (the store keeps seconds).
        let utc = |s: &str| parse_rfc3339(s).map(rfc3339_utc);
        assert_eq!(
            utc("2027-01-15T03:03:00-05:00").as_deref(),
            Some("2027-01-15T08:03:00Z")
        );
        assert_eq!(
            utc("2027-01-15T13:00:05+05:00").as_deref(),
            Some("2027-01-15T08:00:05Z")
        );
        assert_eq!(
            utc("2026-11-04T01:02:03.999Z").as_deref(),
            Some("2026-11-04T01:02:03Z")
        );
        assert_eq!(
            utc("2026-11-04t01:02:03z").as_deref(),
            Some("2026-11-04T01:02:03Z")
        );
        assert_eq!(
            utc("2000-02-29T00:00:00Z").as_deref(),
            Some("2000-02-29T00:00:00Z")
        );
        for bad in [
            "2026-13-01T00:00:00Z",
            "2026-02-29T00:00:00Z",
            "2026-11-04T24:00:00Z",
            "2026-11-04T01:02:03",
            "2026-11-04 01:02:03Z",
            "2026-11-04T01:02:03+5:00",
            "garbage",
            "",
            // M2T03R-101: the schema's date-time has no leap seconds (seconds 00-59).
            "2027-01-15T08:00:60Z",
            // M2T03R-100: past 9999-12-31T23:59:59Z once in UTC, so not in the store's fixed-width form.
            "9999-12-31T23:59:59-23:59",
        ] {
            assert_eq!(parse_rfc3339(bad), None, "{bad}");
        }
        assert_eq!(
            utc("9999-12-31T23:59:59Z").as_deref(),
            Some("9999-12-31T23:59:59Z")
        );
        // The parser takes exactly the schema's date-time grammar.
        for text in [
            "2026-11-04T01:02:03Z",
            "2026-11-04T01:02:03.5+01:30",
            "2026-11-04T01:02:60Z",
            "x",
        ] {
            assert_eq!(
                parse_rfc3339(text).is_some(),
                crate::schema::is_date_time(text),
                "{text}"
            );
        }
        let now = SystemTime::now();
        let whole = parse_rfc3339(&rfc3339_utc(now)).unwrap_or(UNIX_EPOCH);
        assert_eq!(rfc3339_utc(whole), rfc3339_utc(now));
    }

    #[test]
    fn output_is_a_contract_date_time() {
        assert!(crate::schema::is_date_time(&now_rfc3339()));
    }
}

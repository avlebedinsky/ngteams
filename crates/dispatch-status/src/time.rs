//! Milliseconds since the Unix epoch: the clock, and the transcripts' RFC 3339 timestamps.

use std::time::{SystemTime, UNIX_EPOCH};

/// Now, in milliseconds since the epoch.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}

/// A file time, in milliseconds since the epoch.
pub fn system_ms(time: SystemTime) -> Option<i64> {
    let since = time.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(since.as_millis()).ok()
}

/// `2026-10-29T17:20:19.531Z` (a fraction and a `±hh:mm` offset optional) in milliseconds since the epoch.
pub fn parse_rfc3339(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    if !matches!(bytes[10], b'T' | b't' | b' ') {
        return None;
    }
    let year = number(text, 0, 4)?;
    let month = number(text, 5, 7)?;
    let day = number(text, 8, 10)?;
    let hour = number(text, 11, 13)?;
    let minute = number(text, 14, 16)?;
    let second = number(text, 17, 19)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let (millis, rest) = fraction(&text[19..]);
    let offset = offset_minutes(rest)?;
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second - offset * 60;
    Some(seconds * 1_000 + millis)
}

fn number(text: &str, from: usize, to: usize) -> Option<i64> {
    let digits = text.get(from..to)?;
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The milliseconds of a `.123…` fraction, and what follows it.
fn fraction(text: &str) -> (i64, &str) {
    let Some(digits) = text.strip_prefix('.') else {
        return (0, text);
    };
    let end = digits.find(|c: char| !c.is_ascii_digit()).unwrap_or(digits.len());
    let mut millis = 0;
    for (index, digit) in digits[..end].bytes().take(3).enumerate() {
        let scale = [100, 10, 1][index];
        millis += i64::from(digit - b'0') * scale;
    }
    (millis, &digits[end..])
}

/// `Z` (or nothing) is UTC; `+hh:mm` or `-hh:mm` an offset east of it, in minutes.
fn offset_minutes(text: &str) -> Option<i64> {
    if text.is_empty() || text.eq_ignore_ascii_case("z") {
        return Some(0);
    }
    let sign = match text.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    if text.len() != 6 || text.as_bytes()[3] != b':' {
        return None;
    }
    let hours = number(text, 1, 3)?;
    let minutes = number(text, 4, 6)?;
    Some(sign * (hours * 60 + minutes))
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::parse_rfc3339;

    #[test]
    fn reads_the_transcripts_timestamps() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-10-29T17:20:19.531Z"), Some(1_793_294_419_531));
        assert_eq!(parse_rfc3339("2000-02-29T12:00:00.5Z"), Some(951_825_600_500));
        assert_eq!(parse_rfc3339("2026-10-29T19:20:19.531+02:00"), Some(1_793_294_419_531));
    }

    #[test]
    fn refuses_what_is_not_a_timestamp() {
        assert_eq!(parse_rfc3339(""), None);
        assert_eq!(parse_rfc3339("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_rfc3339("2026-10-29 17:20"), None);
        assert_eq!(parse_rfc3339("2026-10-29T17:20:19+0200"), None);
    }
}

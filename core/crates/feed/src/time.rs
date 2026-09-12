//! RFC 3339 UTC timestamps to nanoseconds since the epoch, integer math only.

/// Parses `YYYY-MM-DDTHH:MM:SS[.frac]Z`. Fractions beyond nine digits are truncated.
pub fn parse_rfc3339_ns(s: &str) -> Option<i64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    if d.next().is_some() {
        return None;
    }
    let (hms, frac) = time.split_once('.').unwrap_or((time, ""));
    let mut t = hms.split(':');
    let hour: i64 = t.next()?.parse().ok()?;
    let min: i64 = t.next()?.parse().ok()?;
    let sec: i64 = t.next()?.parse().ok()?;
    if t.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3_600 + min * 60 + sec;
    let nanos = frac_to_ns(frac)?;
    Some(secs * 1_000_000_000 + nanos)
}

fn frac_to_ns(frac: &str) -> Option<i64> {
    if frac.is_empty() {
        return Some(0);
    }
    let digits: String = frac.chars().take(9).collect();
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i64 = digits.parse().ok()?;
    Some(n * 10i64.pow(9 - digits.len() as u32))
}

/// Howard Hinnant's days-from-civil. Valid for the proleptic Gregorian calendar.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_and_known_instants() {
        assert_eq!(parse_rfc3339_ns("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_ns("2000-01-01T00:00:00Z"),
            Some(946_684_800 * 1_000_000_000)
        );
        assert_eq!(
            parse_rfc3339_ns("2026-09-12T15:48:32.340708Z"),
            Some(1_789_228_112 * 1_000_000_000 + 340_708_000)
        );
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_rfc3339_ns("2026-09-12T15:48:32"), None);
        assert_eq!(parse_rfc3339_ns("2026-13-12T15:48:32Z"), None);
        assert_eq!(parse_rfc3339_ns("nope"), None);
    }
}

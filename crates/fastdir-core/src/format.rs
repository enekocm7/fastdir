pub fn bytes(n: u64) -> String {
    bytes_with(n, false)
}

/// Byte formatting with an explicit unit system.
///
/// `decimal` selects SI units (KB/MB, 1000-based, e.g. for users coming from
/// macOS or drive manufacturers); otherwise binary units (KiB-style labels
/// shown as KB/MB, 1024-based, the Windows convention) are used.
pub fn bytes_with(n: u64, decimal: bool) -> String {
    let base = if decimal { 1000.0 } else { 1024.0 };
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if n < 1000 && decimal || n < 1024 && !decimal {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut u = 0;
    while v >= base && u < UNITS.len() - 1 {
        v /= base;
        u += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else if v >= 10.0 {
        format!("{v:.1} {}", UNITS[u])
    } else {
        format!("{v:.2} {}", UNITS[u])
    }
}

pub fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "—".into();
    }
    format!("{:.1}%", part as f64 * 100.0 / whole as f64)
}

pub fn date(unix: i64) -> String {
    date_with(unix, false)
}

/// Timestamp formatting: absolute (`2026-11-02 12:30`) or relative
/// (`3 days ago`), for the file list's "Last Change" column.
pub fn date_with(unix: i64, relative: bool) -> String {
    if unix <= 0 {
        return String::new();
    }
    if !relative {
        return match chrono::DateTime::from_timestamp(unix, 0) {
            Some(dt) => dt
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string(),
            None => String::new(),
        };
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(unix);
    let secs = now.saturating_sub(unix);
    if secs < 10 {
        return "just now".into();
    }
    const MIN: i64 = 60;
    const HOUR: i64 = 3_600;
    const DAY: i64 = 86_400;
    const WEEK: i64 = 7 * DAY;
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;
    let (v, unit) = if secs < HOUR {
        (secs / MIN, "minute")
    } else if secs < DAY {
        (secs / HOUR, "hour")
    } else if secs < WEEK {
        (secs / DAY, "day")
    } else if secs < MONTH {
        (secs / WEEK, "week")
    } else if secs < YEAR {
        (secs / MONTH, "month")
    } else {
        (secs / YEAR, "year")
    };
    let v = v.max(1);
    if v == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{v} {unit}s ago")
    }
}

pub fn duration(d: std::time::Duration) -> String {
    let s = d.as_secs_f64();
    if s < 60.0 {
        format!("{s:.1}s")
    } else {
        format!("{}m {:02}s", (s / 60.0) as u64, (s % 60.0) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_uses_binary_units_and_sensible_precision() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1024), "1.00 KB");
        assert_eq!(bytes(10 * 1024), "10.0 KB");
        assert_eq!(bytes(150 * 1024), "150 KB");
        assert_eq!(bytes(5 * 1024 * 1024 * 1024), "5.00 GB");
    }

    #[test]
    fn bytes_with_supports_decimal_units() {
        assert_eq!(bytes_with(999, true), "999 B");
        assert_eq!(bytes_with(1000, true), "1.00 KB");
        assert_eq!(bytes_with(1500, true), "1.50 KB");
        assert_eq!(bytes_with(1024, true), "1.02 KB");
        assert_eq!(bytes_with(1024, false), "1.00 KB");
        assert_eq!(bytes_with(5_000_000_000, true), "5.00 GB");
    }

    #[test]
    fn count_groups_thousands() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1_000), "1,000");
        assert_eq!(count(1_234_567), "1,234,567");
    }

    #[test]
    fn percent_handles_empty_totals() {
        assert_eq!(percent(0, 0), "—");
        assert_eq!(percent(1, 4), "25.0%");
        assert_eq!(percent(2, 3), "66.7%");
    }

    #[test]
    fn dates_before_the_epoch_are_blank() {
        assert_eq!(date(0), "");
        assert_eq!(date(-1), "");
        assert!(!date(1_700_000_000).is_empty());
    }

    #[test]
    fn relative_dates_count_back_from_now() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert_eq!(date_with(0, true), "");
        assert_eq!(date_with(now, true), "just now");
        assert_eq!(date_with(now - 90, true), "1 minute ago");
        assert_eq!(date_with(now - 3_600, true), "1 hour ago");
        assert_eq!(date_with(now - 3 * 86_400, true), "3 days ago");
        assert_eq!(date_with(now - 14 * 86_400, true), "2 weeks ago");
        assert_eq!(date_with(now - 400 * 86_400, true), "1 year ago");
        // Absolute mode is unaffected by the flag.
        assert!(!date_with(1_700_000_000, false).contains("ago"));
    }

    #[test]
    fn durations_switch_to_minutes() {
        assert_eq!(duration(std::time::Duration::from_millis(1500)), "1.5s");
        assert_eq!(duration(std::time::Duration::from_secs(61)), "1m 01s");
    }

    #[test]
    fn format_edge_cases_are_deterministic() {
        // Binary vs decimal thresholds.
        assert_eq!(bytes(1024 * 1024), "1.00 MB");
        assert_eq!(bytes_with(1_000_000, true), "1.00 MB");
        assert_eq!(bytes_with(1_000_000, false), "977 KB");
        // Very large values stay in PB without panicking.
        assert!(bytes(u64::MAX).ends_with("PB"));
        assert!(bytes_with(u64::MAX, true).ends_with("PB"));
        // Thousands grouping at every magnitude.
        assert_eq!(count(1_000_000), "1,000,000");
        assert_eq!(count(1_000_000_000), "1,000,000,000");
        // Percentages: full, empty and rounding.
        assert_eq!(percent(1, 1), "100.0%");
        assert_eq!(percent(0, 5), "0.0%");
        assert_eq!(percent(1, 3), "33.3%");
        // Durations: sub-minute stays in seconds, minute boundary pads.
        assert_eq!(duration(std::time::Duration::from_secs(0)), "0.0s");
        assert_eq!(duration(std::time::Duration::from_secs(59)), "59.0s");
        assert_eq!(duration(std::time::Duration::from_secs(60)), "1m 00s");
        assert_eq!(duration(std::time::Duration::from_secs(3600)), "60m 00s");
        // Relative dates cover every unit, singular and plural.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert_eq!(date_with(now - 60, true), "1 minute ago");
        assert_eq!(date_with(now - 120, true), "2 minutes ago");
        assert_eq!(date_with(now - 2 * 3_600, true), "2 hours ago");
        assert_eq!(date_with(now - 8 * 86_400, true), "1 week ago");
        assert_eq!(date_with(now - 60 * 86_400, true), "2 months ago");
        assert_eq!(date_with(now - 2 * 365 * 86_400, true), "2 years ago");
    }
}

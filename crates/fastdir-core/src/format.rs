pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
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
    if unix <= 0 {
        return String::new();
    }
    match chrono::DateTime::from_timestamp(unix, 0) {
        Some(dt) => dt
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        None => String::new(),
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
    fn durations_switch_to_minutes() {
        assert_eq!(duration(std::time::Duration::from_millis(1500)), "1.5s");
        assert_eq!(duration(std::time::Duration::from_secs(61)), "1m 01s");
    }
}

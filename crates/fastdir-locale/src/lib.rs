//! UI string tables and locale-aware number/date formatting.
//!
//! FastDir keeps every user-facing string in per-language tables in this
//! crate (see [`en`] for the reference list of keys). UI code looks strings
//! up with [`t`] / [`fmt`], which resolve a configured language code to one
//! of the supported languages and fall back to English for missing keys.
//!
//! Numbers and dates follow the selected locale too: [`format_count`],
//! [`format_percent`], [`format_date`] and [`format_relative`].

mod ar;
mod cs;
mod de;
mod en;
mod es;
mod fr;
mod it;
mod ja;
mod ko;
mod nl;
mod pl;
mod pt;
mod ru;
mod tr;
mod uk;
mod zh;

pub use en::KEYS;

/// Languages the UI can be translated to: `(code, native name)`.
///
/// English comes first; the rest is alphabetical by native name.
pub const SUPPORTED: &[(&str, &str)] = &[
    ("en", "English"),
    ("ar", "العربية"),
    ("cs", "Čeština"),
    ("de", "Deutsch"),
    ("es", "Español"),
    ("fr", "Français"),
    ("it", "Italiano"),
    ("ja", "日本語"),
    ("ko", "한국어"),
    ("nl", "Nederlands"),
    ("pl", "Polski"),
    ("pt", "Português"),
    ("ru", "Русский"),
    ("tr", "Türkçe"),
    ("uk", "Українська"),
    ("zh", "中文"),
];

/// Whether `code` names a supported language (exact match, case-insensitive).
pub fn is_supported(code: &str) -> bool {
    match_code(&code.to_ascii_lowercase().replace('_', "-")).is_some()
}

fn match_code(lower: &str) -> Option<&'static str> {
    if let Some(found) = SUPPORTED.iter().find(|(c, _)| *c == lower) {
        return Some(found.0);
    }
    // Region variants fall back to their base language (`pt-BR` → `pt`).
    if let Some(base) = lower.split('-').next() {
        if let Some(found) = SUPPORTED.iter().find(|(c, _)| *c == base) {
            return Some(found.0);
        }
    }
    None
}

/// Resolve a configured language value to a supported code.
///
/// Empty values, `system` and `auto` follow the OS display language;
/// anything unknown falls back to English.
pub fn resolve(code: &str) -> &'static str {
    let trimmed = code.trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("system")
        || trimmed.eq_ignore_ascii_case("auto")
    {
        return system_language();
    }
    let lower = trimmed.to_ascii_lowercase().replace('_', "-");
    match_code(&lower).unwrap_or("en")
}

/// The OS display language, resolved to a supported code (English fallback).
pub fn system_language() -> &'static str {
    let raw = detect_system_tag();
    if raw.is_empty() {
        return "en";
    }
    match_code(&raw.to_ascii_lowercase().replace('_', "-")).unwrap_or("en")
}

/// Raw OS locale tag such as `de-DE`. The `FASTDIR_LANG` environment variable
/// wins when set (handy for testing); otherwise the Windows display language
/// is queried, falling back to the POSIX `LC_*`/`LANG` variables.
fn detect_system_tag() -> String {
    if let Ok(tag) = std::env::var("FASTDIR_LANG") {
        if !tag.trim().is_empty() {
            return tag;
        }
    }
    #[cfg(windows)]
    {
        let tag = windows_display_language();
        if !tag.is_empty() {
            return tag;
        }
    }
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(tag) = std::env::var(var) {
            let tag = tag.trim().to_string();
            if !tag.is_empty() {
                return tag;
            }
        }
    }
    String::new()
}

#[cfg(windows)]
fn windows_display_language() -> String {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
    // LOCALE_NAME_MAX_LENGTH (85).
    const LEN: i32 = 85;
    let mut buf = vec![0u16; LEN as usize];
    // SAFETY: `buf` has room for `LEN` UTF-16 code units as required.
    let n = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), LEN) };
    if n <= 1 {
        return String::new();
    }
    let end = (n as usize - 1).min(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Look up `key` for `lang`, falling back to English and then to the key.
/// Static keys always yield static strings, so the result can be moved into
/// `'static` dialog closures.
pub fn t(lang: &str, key: &'static str) -> &'static str {
    let code = resolve(lang);
    if code != "en" {
        if let Some(s) = lookup(code, key) {
            return s;
        }
    }
    en::get(key).unwrap_or(key)
}

/// Look up a template and substitute `{name}` placeholders.
pub fn fmt(lang: &str, key: &'static str, args: &[(&str, &str)]) -> String {
    let mut out = t(lang, key).to_string();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

fn lookup(code: &str, key: &str) -> Option<&'static str> {
    match code {
        "ar" => ar::get(key),
        "cs" => cs::get(key),
        "de" => de::get(key),
        "es" => es::get(key),
        "fr" => fr::get(key),
        "it" => it::get(key),
        "ja" => ja::get(key),
        "ko" => ko::get(key),
        "nl" => nl::get(key),
        "pl" => pl::get(key),
        "pt" => pt::get(key),
        "ru" => ru::get(key),
        "tr" => tr::get(key),
        "uk" => uk::get(key),
        "zh" => zh::get(key),
        _ => None,
    }
}

/// Native display name for a supported code (`de` → `Deutsch`).
pub fn native_name(code: &str) -> &'static str {
    SUPPORTED
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, n)| *n)
        .unwrap_or("English")
}

/// Thousands separator used when grouping digits.
pub fn thousands_sep(lang: &str) -> char {
    match resolve(lang) {
        "fr" => '\u{202f}',
        "de" | "it" | "pt" | "es" | "nl" | "pl" | "ru" | "uk" | "tr" | "cs" => '.',
        _ => ',',
    }
}

/// Decimal separator used for fractional values such as percentages.
pub fn decimal_sep(lang: &str) -> char {
    match resolve(lang) {
        "es" | "fr" | "de" | "it" | "pt" | "nl" | "pl" | "ru" | "uk" | "tr" | "cs" => ',',
        _ => '.',
    }
}

/// Integer with locale digit grouping (`1234567` → `1,234,567` in English,
/// `1.234.567` in German, `1 234 567` in French).
pub fn format_count(n: u64, lang: &str) -> String {
    let sep = thousands_sep(lang);
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(sep);
        }
        out.push(ch);
    }
    out
}

/// Percentage with one decimal in the locale's decimal separator.
/// A zero total renders as an em dash (same as before, in every language).
pub fn format_percent(part: u64, whole: u64, lang: &str) -> String {
    if whole == 0 {
        return "—".into();
    }
    let text = format!("{:.1}%", part as f64 * 100.0 / whole as f64);
    let sep = decimal_sep(lang);
    if sep == '.' {
        text
    } else {
        text.replace('.', &sep.to_string())
    }
}

/// Absolute timestamp in the locale's date order (`en` keeps the previous
/// ISO `YYYY-MM-DD HH:MM`; most of Europe uses `DD.MM.YYYY` / `DD/MM/YYYY`).
/// Timestamps at or before the epoch render as an empty string, as before.
pub fn format_date(unix: i64, lang: &str) -> String {
    if unix <= 0 {
        return String::new();
    }
    let dt = chrono::DateTime::from_timestamp(unix, 0);
    let Some(dt) = dt else {
        return String::new();
    };
    let local = dt.with_timezone(&chrono::Local);
    use chrono::Datelike as _;
    use chrono::Timelike as _;
    let (y, m, d) = (local.year(), local.month(), local.day());
    let (hh, mm) = (local.hour(), local.minute());
    let date = match resolve(lang) {
        "zh" | "ko" | "en" => format!("{y:04}-{m:02}-{d:02}"),
        "ja" => format!("{y:04}/{m:02}/{d:02}"),
        "nl" => format!("{d:02}-{m:02}-{y:04}"),
        _ => {
            let sep = match resolve(lang) {
                "es" | "fr" | "it" | "pt" | "ar" => '/',
                _ => '.',
            };
            format!("{d:02}{sep}{m:02}{sep}{y:04}")
        }
    };
    format!("{date} {hh:02}:{mm:02}")
}

/// CLDR-style plural category for a (non-negative) count.
fn plural_form(lang: &str, n: i64) -> &'static str {
    let n = n.max(0);
    match resolve(lang) {
        "ru" | "uk" => {
            let (d10, d100) = (n % 10, n % 100);
            if d10 == 1 && d100 != 11 {
                "one"
            } else if (2..=4).contains(&d10) && !(12..=14).contains(&d100) {
                "few"
            } else {
                "many"
            }
        }
        "pl" => {
            if n == 1 {
                "one"
            } else {
                let (d10, d100) = (n % 10, n % 100);
                if (2..=4).contains(&d10) && !(12..=14).contains(&d100) {
                    "few"
                } else {
                    "many"
                }
            }
        }
        "cs" => match n {
            1 => "one",
            2..=4 => "few",
            _ => "other",
        },
        "ar" => match n {
            1 => "one",
            2 => "two",
            3..=10 => "few",
            11..=99 => "many",
            _ => "other",
        },
        _ => {
            if n == 1 {
                "one"
            } else {
                "other"
            }
        }
    }
}

fn unit_word(lang: &str, unit: &str, n: i64) -> String {
    let code = resolve(lang);
    let form = plural_form(lang, n);
    let key = format!("rel.{unit}.{form}");
    if code != "en" {
        if let Some(word) = lookup(code, key.as_str()) {
            return word.to_string();
        }
    }
    let fallback = format!("rel.{unit}.other");
    en::get(key.as_str())
        .or_else(|| en::get(fallback.as_str()))
        .unwrap_or("?")
        .to_string()
}

/// Relative age (`3 days ago`) in the selected language, from a duration in
/// seconds. Thresholds match the previous English-only behaviour.
pub fn format_relative_secs(secs: i64, lang: &str) -> String {
    if secs < 10 {
        return t(lang, "rel.now").into();
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
    let u = unit_word(lang, unit, v);
    fmt(lang, "rel.past", &[("v", &v.to_string()), ("u", &u)])
}

/// Relative timestamp (`unix` seconds since the epoch) in the selected
/// language. Non-positive timestamps render as an empty string, as before.
pub fn format_relative(unix: i64, lang: &str) -> String {
    if unix <= 0 {
        return String::new();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(unix);
    format_relative_secs(now.saturating_sub(unix), lang)
}

/// Localized display name for a canonical pseudo-item id, or `None` for
/// real names. The canonical ids mirror `FREE_SPACE_NAME` / `UNKNOWN_NAME` in
/// `fastdir_core::model` (the tree's identity); this only affects display.
pub fn special_display(canonical: &str, lang: &str) -> Option<&'static str> {
    match canonical {
        "<Free space>" => Some(lookup_static(lang, "list.free_space")),
        "<Unknown>" => Some(lookup_static(lang, "list.unknown")),
        _ => None,
    }
}

/// Localized name for a drive-kind id (`fixed`, `removable`, `network`,
/// `optical`, `ram`, `unknown`; see `DriveKind::id` in `fastdir_core`).
pub fn drive_kind(id: &str, lang: &str) -> &'static str {
    let key = match id {
        "fixed" => "drive.fixed",
        "removable" => "drive.removable",
        "network" => "drive.network",
        "optical" => "drive.optical",
        "ram" => "drive.ram",
        _ => "drive.unknown",
    };
    lookup_static(lang, key)
}

/// [`t`] for keys known at compile time; the result is always `'static`
/// because every table (including English) defines these keys.
fn lookup_static(lang: &str, key: &'static str) -> &'static str {
    let code = resolve(lang);
    if code != "en" {
        if let Some(s) = lookup(code, key) {
            return s;
        }
    }
    en::get(key).unwrap_or(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_language_covers_every_key() {
        for (code, _) in SUPPORTED {
            for key in KEYS {
                let hit = if *code == "en" {
                    en::get(key)
                } else {
                    lookup(code, key)
                };
                assert!(
                    hit.is_some(),
                    "language {code} is missing key {key}"
                );
            }
        }
    }

    #[test]
    fn resolve_handles_variants_and_fallbacks() {
        assert_eq!(resolve("en"), "en");
        assert_eq!(resolve("EN"), "en");
        assert_eq!(resolve("es-MX"), "es");
        assert_eq!(resolve("pt_BR"), "pt");
        assert_eq!(resolve("zh-Hant-TW"), "zh");
        assert_eq!(resolve("xx"), "en");
        assert_eq!(resolve(""), system_language());
        assert_eq!(resolve("system"), system_language());
    }

    #[test]
    fn missing_keys_fall_back_to_english() {
        assert_eq!(t("es", "app.name"), "FastDir");
        assert_eq!(t("de", "no.such.key"), "no.such.key");
        let updated = fmt("fr", "tb.update_to", &[("version", "1.0")]);
        assert!(updated.contains("1.0"), "placeholder filled: {updated}");
        assert_eq!(updated, t("fr", "tb.update_to").replace("{version}", "1.0"));
    }

    #[test]
    fn counts_group_per_locale() {
        assert_eq!(format_count(0, "en"), "0");
        assert_eq!(format_count(999, "en"), "999");
        assert_eq!(format_count(1_000, "en"), "1,000");
        assert_eq!(format_count(1_234_567, "en"), "1,234,567");
        assert_eq!(format_count(1_234_567, "de"), "1.234.567");
        assert_eq!(format_count(1_234_567, "es"), "1.234.567");
        assert_eq!(
            format_count(1_234_567, "fr"),
            "1\u{202f}234\u{202f}567"
        );
        assert_eq!(format_count(1_234_567, "zh"), "1,234,567");
    }

    #[test]
    fn percents_use_the_locale_decimal_separator() {
        assert_eq!(format_percent(0, 0, "en"), "—");
        assert_eq!(format_percent(1, 4, "en"), "25.0%");
        assert_eq!(format_percent(1, 4, "de"), "25,0%");
        assert_eq!(format_percent(2, 3, "fr"), "66,7%");
    }

    #[test]
    fn relative_times_translate_and_pluralize() {
        assert_eq!(format_relative_secs(5, "en"), "just now");
        assert_eq!(format_relative_secs(90, "en"), "1 minute ago");
        assert_eq!(format_relative_secs(120, "en"), "2 minutes ago");
        assert_eq!(format_relative_secs(120, "de"), t("de", "rel.past")
            .replace("{v}", "2")
            .replace("{u}", t("de", "rel.minute.other")));
        // Slavic few/many forms kick in.
        assert!(format_relative_secs(3 * 86_400, "ru").contains(t("ru", "rel.day.few")));
        assert!(format_relative_secs(5 * 86_400, "ru").contains(t("ru", "rel.day.many")));
        assert!(format_relative_secs(300, "ar").contains(t("ar", "rel.minute.few")));
        assert!(format_relative_secs(120, "ar").contains(t("ar", "rel.minute.two")));
        assert_eq!(format_relative(0, "en"), "");
        assert_eq!(format_relative(-1, "en"), "");
    }

    #[test]
    fn dates_follow_the_locale_order() {
        // 2026-11-02 12:30 UTC; local zone may shift the day, so only check shape.
        let en = format_date(1_794_007_800, "en");
        assert!(en.contains('-'), "en keeps ISO order: {en}");
        let de = format_date(1_794_007_800, "de");
        assert!(de.contains('.'), "de uses dots: {de}");
        let fr = format_date(1_794_007_800, "fr");
        assert!(fr.contains('/'), "fr uses slashes: {fr}");
        assert_eq!(format_date(0, "en"), "");
    }
}

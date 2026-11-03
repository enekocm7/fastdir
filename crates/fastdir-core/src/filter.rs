//! Name/glob and attribute filters for the directory tree.
//!
//! The UI keeps the raw text the user typed and calls [`Filter::parse`] when
//! it applies a filter. Matching itself only looks at one node at a time, so
//! the UI can walk the tree and keep the ancestors of every match.

use crate::model::{Node, NodeKind};
use chrono::{Local, NaiveDate, TimeZone};

/// A parsed, ready-to-use filter. An all-default filter matches every real
/// node (drive pseudo-items are always excluded).
#[derive(Clone, Debug, Default)]
pub struct Filter {
    query: String,
    /// Lower-cased query, for the common substring case.
    query_lower: String,
    /// The query contains `*` or `?` and is matched as a glob.
    glob: bool,
    min_size: u64,
    /// Normalised, lower-case extension including the dot, e.g. `.iso`.
    extension: Option<String>,
    /// Unix seconds; both bounds are inclusive and optional.
    modified_after: Option<i64>,
    modified_before: Option<i64>,
}

impl Filter {
    /// Build a filter from the raw text of the filter controls.
    ///
    /// Empty fields mean "no restriction". The query is either a
    /// case-insensitive name substring or, when it contains `*` or `?`, a
    /// case-insensitive glob that must match the whole name.
    pub fn parse(
        query: &str,
        min_size: &str,
        extension: &str,
        modified_after: &str,
        modified_before: &str,
    ) -> Result<Self, String> {
        Self::parse_in(query, min_size, extension, modified_after, modified_before, "en")
    }

    /// [`Filter::parse`] with localized error messages.
    pub fn parse_in(
        query: &str,
        min_size: &str,
        extension: &str,
        modified_after: &str,
        modified_before: &str,
        lang: &str,
    ) -> Result<Self, String> {
        let min_size = if min_size.trim().is_empty() {
            0
        } else {
            parse_size(min_size).ok_or_else(|| {
                fastdir_locale::fmt(lang, "filter.err_size", &[("input", min_size.trim())])
            })?
        };
        let extension = normalise_extension(extension);
        let modified_after = parse_day_in(modified_after, false, lang)?;
        let modified_before = parse_day_in(modified_before, true, lang)?;
        if let (Some(after), Some(before)) = (modified_after, modified_before)
            && after > before
        {
            return Err(fastdir_locale::t(lang, "filter.err_range").into());
        }
        Ok(Self::new(
            query,
            min_size,
            extension,
            modified_after,
            modified_before,
        ))
    }

    /// A filter from already-parsed values.
    pub fn new(
        query: &str,
        min_size: u64,
        extension: Option<String>,
        modified_after: Option<i64>,
        modified_before: Option<i64>,
    ) -> Self {
        let query = query.trim().to_string();
        let glob = query.contains(['*', '?']);
        Self {
            query_lower: query.to_lowercase(),
            query,
            glob,
            min_size,
            extension,
            modified_after,
            modified_before,
        }
    }

    /// Whether any restriction is set.
    pub fn is_active(&self) -> bool {
        !self.query.is_empty()
            || self.min_size > 0
            || self.extension.is_some()
            || self.modified_after.is_some()
            || self.modified_before.is_some()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// Whether `node` (named `name`) passes every restriction in this filter.
    ///
    /// `ext` is the node's extension from the tree (`None` for directories and
    /// files without one). Drive pseudo-items never match.
    pub fn matches(&self, name: &str, node: &Node, ext: Option<&str>) -> bool {
        if node.is_special() {
            return false;
        }
        if node.size < self.min_size {
            return false;
        }
        if let Some(wanted) = &self.extension
            && (node.kind() != NodeKind::File || ext != Some(wanted.as_str()))
        {
            return false;
        }
        if self.modified_after.is_some() || self.modified_before.is_some() {
            let modified = node.modified();
            // Unknown timestamps cannot satisfy a date restriction.
            if modified == 0 {
                return false;
            }
            if self.modified_after.is_some_and(|after| modified < after) {
                return false;
            }
            if self.modified_before.is_some_and(|before| modified > before) {
                return false;
            }
        }
        if self.query.is_empty() {
            return true;
        }
        if self.glob {
            glob_match(&self.query, name)
        } else {
            name.to_lowercase().contains(&self.query_lower)
        }
    }
}

/// Case-insensitive glob match supporting `*` (any run, including empty) and
/// `?` (exactly one character). The whole name must match.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().flat_map(char::to_lowercase).collect();
    let name: Vec<char> = name.chars().flat_map(char::to_lowercase).collect();
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<usize> = None;
    let mut star_name = 0;
    while ni < name.len() {
        if pi < pattern.len() && (pattern[pi] == '?' || pattern[pi] == name[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < pattern.len() && pattern[pi] == '*' {
            star = Some(pi);
            pi += 1;
            star_name = ni;
        } else if let Some(star_pi) = star {
            // Let the last `*` swallow one more character and try again.
            star_name += 1;
            ni = star_name;
            pi = star_pi + 1;
        } else {
            return false;
        }
    }
    pattern[pi..].iter().all(|&c| c == '*')
}

/// Parse a byte size such as `500`, `1.5 GB`, `20MB` or `2 TiB`. Units are
/// binary (1 KB = 1024 bytes) and case-insensitive.
pub fn parse_size(input: &str) -> Option<u64> {
    let compact: String = input
        .trim()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    if compact.is_empty() {
        return None;
    }
    let split = compact
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(compact.len());
    if split == 0 {
        return None;
    }
    let value: f64 = compact[..split].parse().ok()?;
    let multiplier = match &compact[split..] {
        "" | "b" => 1.0,
        "k" | "kb" | "kib" => 1024.0,
        "m" | "mb" | "mib" => 1024.0 * 1024.0,
        "g" | "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" | "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        "p" | "pb" | "pib" => 1024.0 * 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    let bytes = value * multiplier;
    (bytes.is_finite() && bytes >= 0.0).then_some(bytes as u64)
}

/// Trim, lower-case and add the leading dot to an extension filter.
fn normalise_extension(input: &str) -> Option<String> {
    // `archive.tar.gz` and `.gz` both mean the tree's `.gz` bucket.
    let trimmed = input.trim().rsplit('.').next().unwrap_or("").trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(format!(".{}", trimmed.to_lowercase()))
}

/// Parse `YYYY-MM-DD` in local time. `end` selects the last second of the day,
/// so "before 2024-01-01" includes that whole day.
fn parse_day_in(input: &str, end: bool, lang: &str) -> Result<Option<i64>, String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(None);
    }
    let date = NaiveDate::parse_from_str(input, "%Y-%m-%d")
        .map_err(|_| fastdir_locale::fmt(lang, "filter.err_date_fmt", &[("input", input)]))?;
    let time = if end {
        date.and_hms_opt(23, 59, 59)
    } else {
        date.and_hms_opt(0, 0, 0)
    }
    .expect("valid time of day");
    let local = Local
        .from_local_datetime(&time)
        .earliest()
        .ok_or_else(|| fastdir_locale::fmt(lang, "filter.err_date_zone", &[("input", input)]))?;
    Ok(Some(local.timestamp()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DirBuilder, Subtree, Tree};
    use std::path::PathBuf;

    fn node(size: u64, modified: i64) -> Node {
        let mut arena = crate::model::Arena::default();
        let mut root = DirBuilder::new(0);
        root.add_file("f", size, size, modified);
        let root = root.commit(&mut arena);
        let tree = Tree::new(Subtree::new(arena, "root", root), PathBuf::from("/root"));
        *tree.children(tree.root()).first().unwrap()
    }

    #[test]
    fn glob_matches_whole_names_case_insensitively() {
        assert!(glob_match("*.ISO", "disc.iso"));
        assert!(glob_match("disc.???", "DISC.iso"));
        assert!(glob_match("*report*", "Q3-REPORT-final.pdf"));
        assert!(glob_match("*", "anything"));
        assert!(!glob_match("*.iso", "disc.iso.bak"));
        assert!(!glob_match("a?c", "abbc"));
        assert!(!glob_match("a*c", "abbd"));
    }

    #[test]
    fn query_is_a_substring_unless_it_contains_wildcards() {
        let n = node(10, 0);
        let f = Filter::parse("read", "", "", "", "").unwrap();
        assert!(f.matches("README.txt", &n, Some(".txt")));
        let f = Filter::parse("*.txt", "", "", "", "").unwrap();
        assert!(f.matches("README.txt", &n, Some(".txt")));
        assert!(!f.matches("README.md", &n, Some(".md")));
    }

    #[test]
    fn size_extension_and_date_restrictions_combine() {
        let n = node(2_000, 1_700_000_000);
        let f = Filter::parse("", "1 KB", "TXT", "", "").unwrap();
        assert!(f.matches("a.TXT", &n, Some(".txt")));
        assert!(!f.matches("a.txt", &n, None));
        assert!(f.is_active());

        let f = Filter::parse("", "3 KB", "", "", "").unwrap();
        assert!(!f.matches("a.txt", &n, Some(".txt")));
        let f = Filter::parse("", "", "", "2023-11-01", "2023-11-30").unwrap();
        assert!(f.matches("a.txt", &n, Some(".txt")));
        let f = Filter::parse("", "", "", "2024-01-01", "").unwrap();
        assert!(!f.matches("a.txt", &n, Some(".txt")));

        // Unknown timestamps do not satisfy a date filter.
        let unknown = node(10, 0);
        let f = Filter::parse("", "", "", "2020-01-01", "").unwrap();
        assert!(!f.matches("a.txt", &unknown, Some(".txt")));
    }

    #[test]
    fn parse_size_accepts_binary_units() {
        assert_eq!(parse_size("100"), Some(100));
        assert_eq!(parse_size("1.5 KB"), Some(1536));
        assert_eq!(parse_size("20MB"), Some(20 * 1024 * 1024));
        assert_eq!(parse_size("2 GiB"), Some(2 * 1024 * 1024 * 1024));
        assert_eq!(parse_size("nope"), None);
        assert_eq!(parse_size("10 XB"), None);
        assert_eq!(parse_size(""), None);
    }

    #[test]
    fn bad_dates_and_inverted_ranges_are_reported() {
        assert!(Filter::parse("", "", "", "yesterday", "").is_err());
        assert!(Filter::parse("", "", "", "2024-02-01", "2024-01-01").is_err());
        assert!(!Filter::parse("", "", "", "", "").unwrap().is_active());
    }
}

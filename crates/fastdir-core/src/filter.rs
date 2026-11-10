//! Name/glob and attribute filters for the directory tree.
//!
//! The UI keeps the raw text the user typed and calls [`Filter::parse`] when
//! it applies a filter. Matching itself only looks at one node at a time, so
//! the UI can walk the tree and keep the ancestors of every match.

use crate::model::{Node, NodeKind, Tree};
use chrono::{Local, NaiveDate, TimeZone};
use std::collections::HashSet;

/// A parsed, ready-to-use filter. An all-default filter matches every real
/// node (drive pseudo-items are always excluded).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    query: String,
    /// Lower-cased query, for the common substring case.
    query_lower: String,
    /// The query contains `*` or `?` and is matched as a glob.
    glob: bool,
    min_size: u64,
    /// Normalised, lower-case extensions including the dot, e.g. `.iso`.
    /// Empty means every extension; otherwise any one of them matches.
    extensions: Vec<String>,
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
        Self::parse_in(
            query,
            min_size,
            extension,
            modified_after,
            modified_before,
            "en",
        )
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
        let extensions: Vec<String> = (!extension.trim().is_empty())
            .then(|| extension.to_string())
            .into_iter()
            .collect();
        let modified_after = parse_day_in(modified_after, false, lang)?;
        let modified_before = parse_day_in(modified_before, true, lang)?;
        if let (Some(after), Some(before)) = (modified_after, modified_before)
            && after > before
        {
            return Err(fastdir_locale::t(lang, "filter.err_range").into());
        }
        Ok(Self::with_extensions(
            query,
            min_size,
            &extensions,
            modified_after,
            modified_before,
        ))
    }

    /// A filter from already-parsed values, with one extension (as accepted
    /// by [`Filter::parse`]).
    pub fn new(
        query: &str,
        min_size: u64,
        extension: Option<String>,
        modified_after: Option<i64>,
        modified_before: Option<i64>,
    ) -> Self {
        let extensions: Vec<String> = extension.into_iter().collect();
        Self::with_extensions(
            query,
            min_size,
            &extensions,
            modified_after,
            modified_before,
        )
    }

    /// A filter from already-parsed values. Every extension in `extensions`
    /// is normalised (lower-case, leading dot); an empty list means every
    /// extension.
    pub fn with_extensions(
        query: &str,
        min_size: u64,
        extensions: &[String],
        modified_after: Option<i64>,
        modified_before: Option<i64>,
    ) -> Self {
        let mut normalized: Vec<String> = Vec::with_capacity(extensions.len());
        for extension in extensions {
            if let Some(extension) = normalise_extension(extension)
                && !normalized.contains(&extension)
            {
                normalized.push(extension);
            }
        }
        let query = query.trim().to_string();
        let glob = query.contains(['*', '?']);
        Self {
            query_lower: query.to_lowercase(),
            query,
            glob,
            min_size,
            extensions: normalized,
            modified_after,
            modified_before,
        }
    }

    /// Whether any restriction is set.
    pub fn is_active(&self) -> bool {
        !self.query.is_empty()
            || self.min_size > 0
            || !self.extensions.is_empty()
            || self.modified_after.is_some()
            || self.modified_before.is_some()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn min_size(&self) -> u64 {
        self.min_size
    }

    pub fn extensions(&self) -> &[String] {
        &self.extensions
    }

    pub fn modified_after(&self) -> Option<i64> {
        self.modified_after
    }

    pub fn modified_before(&self) -> Option<i64> {
        self.modified_before
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
        if !self.extensions.is_empty()
            && (node.kind() != NodeKind::File
                || !ext.is_some_and(|ext| self.extensions.iter().any(|wanted| wanted == ext)))
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

/// The entries a [`Filter`] keeps when the ancestors of every match are shown
/// too, as in the GUI's filtered tree.
///
/// Build one per tree, then ask [`FilterView::shows`] for each node. Folder
/// totals and other node fields are untouched: they still describe the whole
/// subtree.
pub struct FilterView<'a> {
    filter: &'a Filter,
    /// Blocks of directories with at least one match somewhere below them.
    dirs: HashSet<u32>,
}

impl<'a> FilterView<'a> {
    pub fn new(tree: &Tree, filter: &'a Filter) -> Self {
        let mut dirs = HashSet::new();
        fn walk(tree: &Tree, filter: &Filter, node: &Node, dirs: &mut HashSet<u32>) -> bool {
            let mut any = false;
            for child in tree.children(node) {
                let below = child.is_dir() && walk(tree, filter, child, dirs);
                if below || filter.matches(tree.name(child), child, tree.ext_name(child)) {
                    any = true;
                }
            }
            if any {
                dirs.insert(node.block_id());
            }
            any
        }
        walk(tree, filter, tree.root(), &mut dirs);
        Self { filter, dirs }
    }

    /// Whether `node` itself matches or is an ancestor of a match.
    pub fn shows(&self, tree: &Tree, node: &Node) -> bool {
        if node.is_special() {
            return false;
        }
        if node.is_dir() && self.dirs.contains(&node.block_id()) {
            return true;
        }
        self.filter
            .matches(tree.name(node), node, tree.ext_name(node))
    }
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

/// Parse a modified-date restriction: `YYYY-MM-DD` in local time, or an age
/// such as `30d`, `6m` or `1y` (d = days, w = weeks, m = 30 days, y = 365
/// days). For a date, `end` selects the last second of the day, so "before
/// 2024-01-01" includes that whole day. Ages are relative to now.
pub fn parse_modified(input: &str, end: bool) -> Option<i64> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if let Some(age) = parse_age(input) {
        return Some(Local::now().timestamp().saturating_sub(age));
    }
    let date = NaiveDate::parse_from_str(input, "%Y-%m-%d").ok()?;
    let time = if end {
        date.and_hms_opt(23, 59, 59)
    } else {
        date.and_hms_opt(0, 0, 0)
    }?;
    Local
        .from_local_datetime(&time)
        .earliest()
        .map(|local| local.timestamp())
}

/// Seconds in an age such as `30d` or `6m` (`d`, `w`, `m` = 30 days, `y` = 365 days).
pub fn parse_age(input: &str) -> Option<i64> {
    let lower = input.trim().to_ascii_lowercase();
    let split = lower.find(|c: char| !c.is_ascii_digit())?;
    let (number, unit) = lower.split_at(split);
    let amount: i64 = number.parse().ok()?;
    let seconds = match unit.trim() {
        "d" | "day" | "days" => 86_400,
        "w" | "week" | "weeks" => 7 * 86_400,
        "m" | "month" | "months" => 30 * 86_400,
        "y" | "year" | "years" => 365 * 86_400,
        _ => return None,
    };
    amount.checked_mul(seconds)
}

/// [`parse_modified`] with a localized error.
fn parse_day_in(input: &str, end: bool, lang: &str) -> Result<Option<i64>, String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(None);
    }
    if parse_age(input).is_some() {
        return Ok(parse_modified(input, end));
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

    #[test]
    fn multiple_extensions_and_ages_are_supported() {
        let now = Local::now().timestamp();
        let n = node(2_000, now - 10 * 86_400);
        let f = Filter::with_extensions("", 0, &["iso".into(), ".IMG".into()], None, None);
        assert!(f.matches("a.iso", &n, Some(".iso")));
        assert!(f.matches("a.img", &n, Some(".img")));
        assert!(!f.matches("a.txt", &n, Some(".txt")));
        assert!(!f.matches("a.iso", &n, None));

        let f = Filter::parse("", "", "", "30d", "").unwrap();
        let after = f.modified_after().expect("an age becomes a timestamp");
        assert!(after <= now - 29 * 86_400 && after >= now - 31 * 86_400);
        assert!(f.matches("a.txt", &n, Some(".txt")));
        let f = Filter::parse("", "", "", "", "1y").unwrap();
        assert!(!f.matches("a.txt", &n, Some(".txt")));
        let old = node(2_000, now - 2 * 365 * 86_400);
        assert!(f.matches("a.txt", &old, Some(".txt")));
        assert!(parse_modified("6m", false).is_some());
        assert!(parse_modified("2w", false).is_some());
        assert!(parse_modified("soon", false).is_none());
    }

    #[test]
    fn filtered_view_keeps_matches_and_their_ancestors() {
        let mut arena = crate::model::Arena::default();
        let mut docs = DirBuilder::new(0);
        docs.add_file("keep.iso", 10, 10, 0);
        docs.add_file("skip.txt", 10, 10, 0);
        let docs = docs.commit(&mut arena);
        let mut root = DirBuilder::new(0);
        root.add_dir("docs", docs);
        root.add_file("other.txt", 10, 10, 0);
        let root = root.commit(&mut arena);
        let tree = Tree::new(Subtree::new(arena, "root", root), PathBuf::from("/root"));

        let filter = Filter::parse("*.iso", "", "", "", "").unwrap();
        let view = FilterView::new(&tree, &filter);
        let child = |parent: &Node, name: &str| {
            tree.children(parent)
                .iter()
                .find(|node| tree.name(node) == name)
                .expect("child exists")
        };
        assert!(view.shows(&tree, tree.root()));
        let docs = child(tree.root(), "docs");
        assert!(view.shows(&tree, docs));
        assert!(view.shows(&tree, child(docs, "keep.iso")));
        assert!(!view.shows(&tree, child(docs, "skip.txt")));
        assert!(!view.shows(&tree, child(tree.root(), "other.txt")));
    }
}

//! File-type categories: a coarse grouping of extensions (video, images,
//! archives, ...) for the extension panel and the treemap.
//!
//! Every extension maps to exactly one category. The built-in table is
//! deliberately static and case-insensitive; users can move or add extensions
//! in `config.toml` (`[categories] video = ["mp4", "m4v"]`), which
//! [`CategoryMap::with_overrides`] applies on top of the defaults. Extensions
//! the table does not know fall into [`FileCategory::Other`].

use std::collections::HashMap;

/// A coarse file-type group. The order is also the order used by totals and
/// the treemap's category palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum FileCategory {
    Video,
    Images,
    Audio,
    Archives,
    Documents,
    Code,
    Executables,
    DiskImages,
    System,
    Other,
}

impl FileCategory {
    /// Every category, in display order.
    pub const ALL: [FileCategory; 10] = [
        FileCategory::Video,
        FileCategory::Images,
        FileCategory::Audio,
        FileCategory::Archives,
        FileCategory::Documents,
        FileCategory::Code,
        FileCategory::Executables,
        FileCategory::DiskImages,
        FileCategory::System,
        FileCategory::Other,
    ];

    /// Number of categories.
    pub const COUNT: usize = Self::ALL.len();

    /// Stable id used in `config.toml` and the locale string table.
    pub fn id(self) -> &'static str {
        match self {
            FileCategory::Video => "video",
            FileCategory::Images => "images",
            FileCategory::Audio => "audio",
            FileCategory::Archives => "archives",
            FileCategory::Documents => "documents",
            FileCategory::Code => "code",
            FileCategory::Executables => "executables",
            FileCategory::DiskImages => "disk_images",
            FileCategory::System => "system",
            FileCategory::Other => "other",
        }
    }

    /// Parse a `config.toml` category name (case-insensitive; `disk-images`
    /// and `diskimages` are accepted too).
    pub fn from_id(id: &str) -> Option<Self> {
        let id = id.trim().to_ascii_lowercase().replace([' ', '-'], "_");
        let id = match id.as_str() {
            "video" | "videos" => "video",
            "image" | "images" | "pictures" => "images",
            "audio" | "sound" => "audio",
            "archive" | "archives" => "archives",
            "document" | "documents" | "docs" => "documents",
            "code" | "source" => "code",
            "executable" | "executables" | "installers" => "executables",
            "disk_image" | "disk_images" | "diskimages" | "images_disk" => "disk_images",
            "system" => "system",
            "other" | "misc" => "other",
            _ => return None,
        };
        Self::ALL.into_iter().find(|c| c.id() == id)
    }

    /// Index into [`Self::ALL`] / a category total array.
    #[inline]
    pub fn index(self) -> usize {
        self as usize
    }

    /// Built-in extensions for this category, without the leading dot.
    /// [`FileCategory::Other`] has no built-in entries.
    pub fn builtin_extensions(self) -> &'static [&'static str] {
        match self {
            FileCategory::Video => VIDEO,
            FileCategory::Images => IMAGES,
            FileCategory::Audio => AUDIO,
            FileCategory::Archives => ARCHIVES,
            FileCategory::Documents => DOCUMENTS,
            FileCategory::Code => CODE,
            FileCategory::Executables => EXECUTABLES,
            FileCategory::DiskImages => DISK_IMAGES,
            FileCategory::System => SYSTEM,
            FileCategory::Other => &[],
        }
    }
}

const VIDEO: &[&str] = &[
    "mp4", "m4v", "mkv", "avi", "mov", "wmv", "flv", "webm", "mpg", "mpeg", "m2ts", "mts", "vob",
    "ogv", "3gp", "3g2", "rm", "rmvb", "asf", "divx", "f4v", "mxf",
];
const IMAGES: &[&str] = &[
    "jpg", "jpeg", "jpe", "png", "gif", "bmp", "tif", "tiff", "webp", "svg", "ico", "heic", "heif",
    "avif", "raw", "cr2", "cr3", "nef", "arw", "dng", "orf", "rw2", "psd", "ai", "eps", "xcf",
];
const AUDIO: &[&str] = &[
    "mp3", "wav", "flac", "aac", "ogg", "oga", "m4a", "wma", "opus", "aiff", "aif", "alac", "ape",
    "mid", "midi", "amr", "mka", "dsf", "dff",
];
const ARCHIVES: &[&str] = &[
    "zip", "rar", "7z", "tar", "gz", "bz2", "xz", "zst", "tgz", "tbz2", "txz", "lz", "lzma", "z",
    "jar", "war", "ear", "cpio", "arj", "lha", "lzh", "cab",
];
const DOCUMENTS: &[&str] = &[
    "pdf", "doc", "docx", "docm", "dot", "dotx", "xls", "xlsx", "xlsm", "xlt", "xltx", "ppt",
    "pptx", "pptm", "pot", "potx", "odt", "ods", "odp", "odg", "rtf", "txt", "text", "md",
    "markdown", "csv", "tsv", "epub", "mobi", "azw", "azw3", "fb2", "djvu", "pages", "numbers",
    "key", "tex", "log",
];
const CODE: &[&str] = &[
    "rs",
    "c",
    "h",
    "cc",
    "cpp",
    "cxx",
    "hpp",
    "hxx",
    "hh",
    "m",
    "mm",
    "java",
    "kt",
    "kts",
    "py",
    "pyw",
    "js",
    "jsx",
    "mjs",
    "cjs",
    "ts",
    "tsx",
    "go",
    "rb",
    "php",
    "cs",
    "swift",
    "sh",
    "bash",
    "zsh",
    "fish",
    "ps1",
    "psm1",
    "bat",
    "cmd",
    "html",
    "htm",
    "xhtml",
    "css",
    "scss",
    "sass",
    "less",
    "json",
    "jsonc",
    "xml",
    "yaml",
    "yml",
    "toml",
    "ini",
    "cfg",
    "conf",
    "sql",
    "vue",
    "svelte",
    "dart",
    "lua",
    "r",
    "pl",
    "pm",
    "asm",
    "s",
    "vb",
    "fs",
    "fsx",
    "hs",
    "ex",
    "exs",
    "erl",
    "clj",
    "cljs",
    "scala",
    "gradle",
    "cmake",
    "mk",
    "dockerfile",
    "proto",
];
const EXECUTABLES: &[&str] = &[
    "exe",
    "dll",
    "msi",
    "msix",
    "msixbundle",
    "appx",
    "appxbundle",
    "com",
    "scr",
    "pif",
    "apk",
    "aab",
    "ipa",
    "deb",
    "rpm",
    "appimage",
    "bin",
    "run",
    "so",
    "dylib",
    "o",
    "obj",
    "lib",
    "sys",
    "ocx",
    "cpl",
    "tlb",
];
const DISK_IMAGES: &[&str] = &[
    "iso", "img", "vhd", "vhdx", "vmdk", "vdi", "qcow", "qcow2", "dmg", "cue", "nrg", "toast",
    "mdf", "mds", "ccd", "sub", "bwt", "b5t", "b6t", "udf",
];
const SYSTEM: &[&str] = &[
    "drv", "inf", "cat", "mui", "manifest", "etl", "dmp", "pdb", "swp", "tmp", "bak", "dat",
    "cache", "lock", "reg", "lnk", "url", "desktop", "theme", "pol", "evtx", "pf", "job",
];

/// The extension without a leading dot, lower-cased only when needed.
fn normalize(ext: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    let ext = ext.trim().trim_start_matches('.');
    if ext.bytes().any(|b| b.is_ascii_uppercase()) {
        // Callers with uppercase extensions are rare (config files, tests);
        // the tree already stores lower-case extension names.
        Cow::Owned(ext.to_ascii_lowercase())
    } else {
        Cow::Borrowed(ext)
    }
}

/// The built-in category for one extension (with or without a leading dot).
/// Unknown extensions, and the empty string, are [`FileCategory::Other`].
pub fn builtin_category(ext: &str) -> FileCategory {
    match normalize(ext).as_ref() {
        "mp4" | "m4v" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm" | "mpg" | "mpeg"
        | "m2ts" | "mts" | "vob" | "ogv" | "3gp" | "3g2" | "rm" | "rmvb" | "asf" | "divx"
        | "f4v" | "mxf" => FileCategory::Video,
        "jpg" | "jpeg" | "jpe" | "png" | "gif" | "bmp" | "tif" | "tiff" | "webp" | "svg"
        | "ico" | "heic" | "heif" | "avif" | "raw" | "cr2" | "cr3" | "nef" | "arw" | "dng"
        | "orf" | "rw2" | "psd" | "ai" | "eps" | "xcf" => FileCategory::Images,
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "oga" | "m4a" | "wma" | "opus" | "aiff"
        | "aif" | "alac" | "ape" | "mid" | "midi" | "amr" | "mka" | "dsf" | "dff" => {
            FileCategory::Audio
        }
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "zst" | "tgz" | "tbz2" | "txz"
        | "lz" | "lzma" | "z" | "jar" | "war" | "ear" | "cpio" | "arj" | "lha" | "lzh" | "cab" => {
            FileCategory::Archives
        }
        "pdf" | "doc" | "docx" | "docm" | "dot" | "dotx" | "xls" | "xlsx" | "xlsm" | "xlt"
        | "xltx" | "ppt" | "pptx" | "pptm" | "pot" | "potx" | "odt" | "ods" | "odp" | "odg"
        | "rtf" | "txt" | "text" | "md" | "markdown" | "csv" | "tsv" | "epub" | "mobi" | "azw"
        | "azw3" | "fb2" | "djvu" | "pages" | "numbers" | "key" | "tex" | "log" => {
            FileCategory::Documents
        }
        "rs" | "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hxx" | "hh" | "m" | "mm" | "java"
        | "kt" | "kts" | "py" | "pyw" | "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "go"
        | "rb" | "php" | "cs" | "swift" | "sh" | "bash" | "zsh" | "fish" | "ps1" | "psm1"
        | "bat" | "cmd" | "html" | "htm" | "xhtml" | "css" | "scss" | "sass" | "less" | "json"
        | "jsonc" | "xml" | "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf" | "sql" | "vue"
        | "svelte" | "dart" | "lua" | "r" | "pl" | "pm" | "asm" | "s" | "vb" | "fs" | "fsx"
        | "hs" | "ex" | "exs" | "erl" | "clj" | "cljs" | "scala" | "gradle" | "cmake" | "mk"
        | "dockerfile" | "proto" => FileCategory::Code,
        "exe" | "dll" | "msi" | "msix" | "msixbundle" | "appx" | "appxbundle" | "com" | "scr"
        | "pif" | "apk" | "aab" | "ipa" | "deb" | "rpm" | "appimage" | "bin" | "run" | "so"
        | "dylib" | "o" | "obj" | "lib" | "sys" | "ocx" | "cpl" | "tlb" => {
            FileCategory::Executables
        }
        "iso" | "img" | "vhd" | "vhdx" | "vmdk" | "vdi" | "qcow" | "qcow2" | "dmg" | "cue"
        | "nrg" | "toast" | "mdf" | "mds" | "ccd" | "sub" | "bwt" | "b5t" | "b6t" | "udf" => {
            FileCategory::DiskImages
        }
        "drv" | "inf" | "cat" | "mui" | "manifest" | "etl" | "dmp" | "pdb" | "swp" | "tmp"
        | "bak" | "dat" | "cache" | "lock" | "reg" | "lnk" | "url" | "desktop" | "theme"
        | "pol" | "evtx" | "pf" | "job" => FileCategory::System,
        _ => FileCategory::Other,
    }
}

/// Built-in categories plus the user's `[categories]` overrides.
///
/// An override both moves an extension out of its built-in category and can
/// introduce extensions the built-in table does not know. The last override
/// wins if the same extension appears under two categories.
#[derive(Clone, Debug, Default)]
pub struct CategoryMap {
    overrides: HashMap<Box<str>, FileCategory>,
}

impl CategoryMap {
    /// The built-in table with no overrides.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a map from `(category name, extensions)` pairs, as written in
    /// `config.toml`. Unknown category names are ignored; extensions may be
    /// written with or without the leading dot and in any case.
    pub fn with_overrides(pairs: impl IntoIterator<Item = (String, Vec<String>)>) -> Self {
        let mut map = Self::default();
        for (name, extensions) in pairs {
            let Some(category) = FileCategory::from_id(&name) else {
                continue;
            };
            map.extend(category, &extensions);
        }
        map
    }

    /// Assign every extension in `extensions` to `category`.
    pub fn extend(&mut self, category: FileCategory, extensions: &[String]) {
        for ext in extensions {
            let ext = normalize(ext);
            if !ext.is_empty() {
                self.overrides
                    .insert(ext.into_owned().into_boxed_str(), category);
            }
        }
    }

    /// The category of one extension (with or without a leading dot).
    #[inline]
    pub fn category(&self, ext: &str) -> FileCategory {
        let ext = normalize(ext);
        if let Some(&category) = self.overrides.get(ext.as_ref()) {
            return category;
        }
        builtin_category(&ext)
    }

    /// Whether the built-in table or an override knows this extension.
    pub fn is_known(&self, ext: &str) -> bool {
        let ext = normalize(ext);
        self.overrides.contains_key(ext.as_ref()) || builtin_category(&ext) != FileCategory::Other
    }

    /// Every extension that maps to `category`, sorted. Built-ins whose
    /// override moved them elsewhere are left out of their old category.
    pub fn extensions(&self, category: FileCategory) -> Vec<String> {
        let mut out: Vec<String> = category
            .builtin_extensions()
            .iter()
            .filter(|ext| self.category(ext) == category)
            .map(|ext| (*ext).to_string())
            .collect();
        for (ext, &cat) in &self.overrides {
            if cat == category && !out.iter().any(|e| e == ext.as_ref()) {
                out.push(ext.to_string());
            }
        }
        out.sort_unstable();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_extension_has_exactly_one_category() {
        let mut seen: HashMap<&str, FileCategory> = HashMap::new();
        for category in FileCategory::ALL {
            for ext in category.builtin_extensions() {
                let previous = seen.insert(ext, category);
                assert!(
                    previous.is_none(),
                    "{ext} is listed under {category:?} and {previous:?}"
                );
                assert_eq!(builtin_category(ext), category, "table mismatch for {ext}");
            }
        }
        assert!(!seen.is_empty());
    }

    #[test]
    fn the_builtin_match_covers_every_listed_extension() {
        // Iterating the match arms would need a second table; instead spot
        // check both directions with case and dot variants.
        assert_eq!(builtin_category("mp4"), FileCategory::Video);
        assert_eq!(builtin_category(".MKV"), FileCategory::Video);
        assert_eq!(builtin_category("JPG"), FileCategory::Images);
        assert_eq!(builtin_category(".zip"), FileCategory::Archives);
        assert_eq!(builtin_category("rs"), FileCategory::Code);
        assert_eq!(builtin_category("exe"), FileCategory::Executables);
        assert_eq!(builtin_category("iso"), FileCategory::DiskImages);
        assert_eq!(builtin_category("sys"), FileCategory::Executables);
        assert_eq!(builtin_category("etl"), FileCategory::System);
        assert_eq!(builtin_category("nope"), FileCategory::Other);
        assert_eq!(builtin_category(""), FileCategory::Other);
        assert_eq!(builtin_category("."), FileCategory::Other);
    }

    #[test]
    fn ids_round_trip_and_parse_from_config() {
        for category in FileCategory::ALL {
            assert_eq!(FileCategory::from_id(category.id()), Some(category));
        }
        assert_eq!(FileCategory::from_id("VIDEO"), Some(FileCategory::Video));
        assert_eq!(
            FileCategory::from_id("disk-images"),
            Some(FileCategory::DiskImages)
        );
        assert_eq!(FileCategory::from_id("nope"), None);
    }

    #[test]
    fn overrides_move_and_add_extensions() {
        let map = CategoryMap::with_overrides([
            (
                "video".to_string(),
                vec!["txt".to_string(), ".XYZ".to_string()],
            ),
            ("unknown".to_string(), vec!["abc".to_string()]),
            ("other".to_string(), vec![String::new(), "  ".to_string()]),
        ]);
        // A built-in document moved to video.
        assert_eq!(map.category("txt"), FileCategory::Video);
        assert_eq!(map.category(".TXT"), FileCategory::Video);
        // A new extension, case-insensitively.
        assert_eq!(map.category("xyz"), FileCategory::Video);
        assert_eq!(map.category(".XyZ"), FileCategory::Video);
        // The old category no longer lists it.
        assert!(
            !map.extensions(FileCategory::Documents)
                .contains(&"txt".into())
        );
        assert!(map.extensions(FileCategory::Video).contains(&"txt".into()));
        assert!(map.extensions(FileCategory::Video).contains(&"xyz".into()));
        // Empty entries are ignored, unknown categories too.
        assert_eq!(map.category("abc"), FileCategory::Other);
        assert!(map.is_known("txt"));
        assert!(map.is_known("xyz"));
        assert!(!map.is_known("nope"));
    }

    #[test]
    fn unknown_extensions_are_other() {
        let map = CategoryMap::new();
        assert_eq!(map.category("mp4"), FileCategory::Video);
        assert_eq!(map.category("no-such-ext"), FileCategory::Other);
        assert!(!map.is_known("no-such-ext"));
    }
}

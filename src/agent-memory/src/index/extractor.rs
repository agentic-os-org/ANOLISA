use std::os::fd::AsFd;
use std::path::Path;

use crate::safe_fs;

const MAX_INDEXABLE_BYTES: u64 = 4 * 1024 * 1024;

/// Decide whether `path` should be indexed by BM25, based on extension and
/// size. We only index UTF-8 text-y formats; binaries are skipped silently.
pub fn is_indexable(path: &Path, size: u64) -> bool {
    if size > MAX_INDEXABLE_BYTES {
        return false;
    }
    match path.extension().and_then(|e| e.to_str()) {
        // Common text formats
        Some(
            "md" | "markdown" | "txt" | "rst" | "org" | "json" | "jsonl" | "yaml" | "yml" | "toml"
            | "ini" | "log" | "tex" | "adoc" | "csv" | "tsv",
        ) => true,
        // Source-like
        Some(
            "rs" | "py" | "js" | "ts" | "go" | "java" | "c" | "h" | "cpp" | "hpp" | "sh" | "rb"
            | "php",
        ) => true,
        // No extension is often a README/notes file
        None => true,
        _ => false,
    }
}

/// Read a file as UTF-8 via safe_fs (openat2 RESOLVE_BENEATH|NO_SYMLINKS)
/// so symlinks planted in the mount cannot redirect the read outside.
/// Return None if non-UTF-8 (we don't index binaries or mojibake).
pub fn extract_text(root_fd: impl AsFd, rel: &Path) -> Option<String> {
    safe_fs::read_to_string(root_fd.as_fd(), rel).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn extension_filter() {
        assert!(is_indexable(&PathBuf::from("a.md"), 100));
        assert!(is_indexable(&PathBuf::from("README"), 100));
        assert!(is_indexable(&PathBuf::from("a.rs"), 100));
        assert!(!is_indexable(&PathBuf::from("img.png"), 100));
        assert!(!is_indexable(&PathBuf::from("a.exe"), 100));
    }

    #[test]
    fn size_cap() {
        assert!(!is_indexable(&PathBuf::from("a.md"), 5 * 1024 * 1024));
    }
}

//! Shared descriptor-relative discovery for file-reading tools.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};

use glob::{MatchOptions, Pattern};

use super::workspace_fs::{WorkspaceBatchNode, WorkspaceFs, WorkspaceNode, WorkspacePathNode};

const MAX_MATCHES: usize = 100;

pub(super) struct FileMatch {
    pub path: PathBuf,
    pub file: Result<File, String>,
}

pub(super) struct FileMatches {
    pub files: Vec<FileMatch>,
    pub truncated: bool,
}

pub(super) struct FilePathMatches {
    pub paths: Vec<PathBuf>,
    pub truncated: bool,
    pub skipped: Vec<String>,
}

pub(super) fn expand_file_patterns(
    patterns: &[String],
    cwd: &Path,
    workspace: &WorkspaceFs,
    max_matches: usize,
) -> Result<FileMatches, String> {
    let mut matches = BTreeMap::new();
    let mut truncated = false;
    let max_matches = max_matches.min(MAX_MATCHES);

    for (pattern_index, pattern) in patterns.iter().enumerate() {
        if pattern.trim().is_empty() {
            return Err("file pattern must not be empty".to_string());
        }
        if let Some(node) = workspace.try_open_batch_node(cwd, pattern)? {
            match node {
                WorkspaceBatchNode::Node(WorkspaceNode::File(file)) => {
                    if insert_match(&mut matches, file.display_path, Ok(file.file), max_matches) {
                        truncated = true;
                    }
                }
                WorkspaceBatchNode::Node(WorkspaceNode::Directory(directory)) => {
                    let walked = workspace.walk_files(directory, max_matches, |_| true)?;
                    truncated |= walked.truncated;
                    for file in walked.files {
                        if insert_match(&mut matches, file.display_path, Ok(file.file), max_matches)
                        {
                            truncated = true;
                            break;
                        }
                    }
                }
                WorkspaceBatchNode::InaccessibleFile {
                    display_path,
                    error,
                } => {
                    if insert_match(&mut matches, display_path, Err(error), max_matches) {
                        truncated = true;
                    }
                }
            }
        } else if pattern.chars().any(is_glob_char) {
            let (base, suffix) = split_path_pattern(pattern);
            let base = if base.is_empty() { "." } else { base };
            let matcher = Pattern::new(&suffix)
                .map_err(|error| format!("invalid glob pattern '{pattern}': {error}"))?;
            let directory = match workspace.try_open_node(cwd, base)? {
                Some(WorkspaceNode::Directory(directory)) => directory,
                Some(WorkspaceNode::File(file)) => {
                    return Err(format!("Not a directory: {}", file.display_path.display()));
                }
                None => continue,
            };
            let options = match_options();
            let walked = workspace.walk_files(directory, max_matches, |path| {
                matcher.matches_path_with(path, options)
            })?;
            truncated |= walked.truncated;
            for file in walked.files {
                if insert_match(&mut matches, file.display_path, Ok(file.file), max_matches) {
                    truncated = true;
                    break;
                }
            }
        }
        if truncated {
            break;
        }
        if matches.len() >= max_matches {
            truncated = pattern_index + 1 < patterns.len();
            break;
        }
    }

    Ok(FileMatches {
        files: matches
            .into_iter()
            .map(|(path, file)| FileMatch { path, file })
            .collect(),
        truncated,
    })
}

pub(super) fn expand_file_paths(
    patterns: &[String],
    cwd: &Path,
    workspace: &WorkspaceFs,
    max_matches: usize,
) -> Result<FilePathMatches, String> {
    let mut matches = BTreeSet::new();
    let mut truncated = false;
    let mut skipped = Vec::new();
    let max_matches = max_matches.min(MAX_MATCHES);

    for (pattern_index, pattern) in patterns.iter().enumerate() {
        if pattern.trim().is_empty() {
            return Err("file pattern must not be empty".to_string());
        }
        if let Some(node) = workspace.try_open_path_node(cwd, pattern)? {
            match node {
                WorkspacePathNode::File(path) => {
                    if insert_path_match(&mut matches, path, max_matches) {
                        truncated = true;
                    }
                }
                WorkspacePathNode::Directory(directory) => {
                    let walked = workspace.walk_file_paths(directory, max_matches, |_| true)?;
                    truncated |= walked.truncated;
                    for path in walked.paths {
                        if insert_path_match(&mut matches, path, max_matches) {
                            truncated = true;
                            break;
                        }
                    }
                }
                WorkspacePathNode::InaccessibleDirectory {
                    display_path,
                    error,
                } => skipped.push(format!("{}: {error}", display_path.display())),
            }
        } else if pattern.chars().any(is_glob_char) {
            let (base, suffix) = split_path_pattern(pattern);
            let base = if base.is_empty() { "." } else { base };
            let matcher = Pattern::new(&suffix)
                .map_err(|error| format!("invalid glob pattern '{pattern}': {error}"))?;
            let directory = match workspace.try_open_path_node(cwd, base)? {
                Some(WorkspacePathNode::Directory(directory)) => directory,
                Some(WorkspacePathNode::File(path)) => {
                    return Err(format!("Not a directory: {}", path.display()));
                }
                Some(WorkspacePathNode::InaccessibleDirectory {
                    display_path,
                    error,
                }) => {
                    skipped.push(format!("{}: {error}", display_path.display()));
                    continue;
                }
                None => continue,
            };
            let options = match_options();
            let walked = workspace.walk_file_paths(directory, max_matches, |path| {
                matcher.matches_path_with(path, options)
            })?;
            truncated |= walked.truncated;
            for path in walked.paths {
                if insert_path_match(&mut matches, path, max_matches) {
                    truncated = true;
                    break;
                }
            }
        }
        if truncated {
            break;
        }
        if matches.len() >= max_matches {
            truncated = pattern_index + 1 < patterns.len();
            break;
        }
    }

    Ok(FilePathMatches {
        paths: matches.into_iter().collect(),
        truncated,
        skipped,
    })
}

fn is_glob_char(character: char) -> bool {
    matches!(character, '*' | '?' | '[' | ']')
}

/// Splits a pattern into its literal base and glob suffix.
fn split_path_pattern(pattern: &str) -> (&str, String) {
    let separator = std::path::MAIN_SEPARATOR;
    let separator_len = separator.len_utf8();
    let Some(glob_index) = pattern
        .split(separator)
        .position(|segment| segment.chars().any(is_glob_char))
    else {
        return (pattern, String::new());
    };
    if glob_index == 0 {
        return ("", pattern.to_string());
    }

    let suffix_start = pattern
        .split(separator)
        .take(glob_index)
        .map(|segment| segment.len() + separator_len)
        .sum::<usize>();
    let base_end = suffix_start.saturating_sub(separator_len);
    let base = if base_end == 0 && pattern.starts_with(separator) {
        std::path::MAIN_SEPARATOR_STR
    } else {
        &pattern[..base_end]
    };
    (base, pattern[suffix_start..].to_string())
}

fn match_options() -> MatchOptions {
    MatchOptions {
        case_sensitive: !cfg!(any(target_os = "macos", target_os = "windows")),
        require_literal_separator: false,
        require_literal_leading_dot: false,
    }
}

fn insert_match(
    matches: &mut BTreeMap<PathBuf, Result<File, String>>,
    path: PathBuf,
    file: Result<File, String>,
    max_matches: usize,
) -> bool {
    if matches.contains_key(&path) {
        return false;
    }
    if matches.len() >= max_matches {
        return true;
    }
    matches.insert(path, file);
    false
}

fn insert_path_match(matches: &mut BTreeSet<PathBuf>, path: PathBuf, max_matches: usize) -> bool {
    if matches.contains(&path) {
        return false;
    }
    if matches.len() >= max_matches {
        return true;
    }
    matches.insert(path);
    false
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn expand(
        patterns: &[String],
        cwd: &Path,
        root: &Path,
        max_matches: usize,
    ) -> Result<FileMatches, String> {
        let workspace = WorkspaceFs::new(root)?;
        expand_file_patterns(patterns, cwd, &workspace, max_matches)
    }

    #[test]
    fn expands_exact_files_and_globs_without_duplicates() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("src/nested")).unwrap();
        std::fs::write(directory.path().join("src/lib.rs"), "lib").unwrap();
        std::fs::write(directory.path().join("src/nested/mod.rs"), "mod").unwrap();
        std::fs::write(directory.path().join("src/readme.md"), "readme").unwrap();

        let matches = expand(
            &[
                "src/**/*.rs".to_string(),
                "src/lib.rs".to_string(),
                "src/readme.md".to_string(),
            ],
            directory.path(),
            directory.path(),
            10,
        )
        .unwrap();

        assert_eq!(matches.files.len(), 3);
        assert!(!matches.truncated);
    }

    #[test]
    fn caps_matches() {
        let directory = tempfile::tempdir().unwrap();
        for index in 0..3 {
            std::fs::write(directory.path().join(format!("{index}.txt")), "text").unwrap();
        }

        let matches = expand(
            &["*.txt".to_string()],
            directory.path(),
            directory.path(),
            2,
        )
        .unwrap();

        assert_eq!(matches.files.len(), 2);
        assert!(matches.truncated);
    }

    #[test]
    fn treats_glob_metacharacters_in_cwd_as_literals() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().join("work[tree]");
        std::fs::create_dir(&base).unwrap();
        std::fs::write(base.join("lib.rs"), "lib").unwrap();

        let matches = expand(&["*.rs".to_string()], &base, &base, 10).unwrap();

        assert_eq!(matches.files.len(), 1);
        assert_eq!(
            matches.files[0].path,
            base.canonicalize().unwrap().join("lib.rs")
        );
    }

    #[test]
    fn root_absolute_glob_preserves_literal_base() {
        assert_eq!(split_path_pattern("/*.rs"), ("/", "*.rs".to_string()));
        assert_eq!(
            split_path_pattern("/tmp*/file"),
            ("/", "tmp*/file".to_string())
        );
    }

    #[test]
    fn absolute_glob_uses_resolved_literal_base() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("src");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("lib.rs"), "lib").unwrap();
        std::fs::write(source.join("main.rs"), "main").unwrap();
        std::fs::write(directory.path().join("readme.md"), "readme").unwrap();
        let pattern = source.join("*.rs").to_str().unwrap().to_string();

        let matches = expand(&[pattern], Path::new("/nonexistent"), directory.path(), 10).unwrap();

        assert_eq!(matches.files.len(), 2);
        let source = source.canonicalize().unwrap();
        assert!(matches
            .files
            .iter()
            .all(|file| file.path.starts_with(&source)));
    }

    #[test]
    fn skips_symlinks_that_escape_during_discovery() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("inside.txt"), "inside").unwrap();
        std::fs::write(parent.path().join("outside.txt"), "outside").unwrap();
        symlink(
            parent.path().join("outside.txt"),
            root.join("outside-link.txt"),
        )
        .unwrap();

        let matches = expand(&["*.txt".to_string()], &root, &root, 10).unwrap();

        assert_eq!(matches.files.len(), 1);
        assert!(matches.files[0].path.ends_with("inside.txt"));
    }

    #[test]
    fn existing_path_with_glob_characters_takes_precedence() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("report[1].txt"), "literal").unwrap();
        std::fs::write(directory.path().join("report1.txt"), "glob").unwrap();

        let matches = expand(
            &["report[1].txt".to_string()],
            directory.path(),
            directory.path(),
            10,
        )
        .unwrap();

        assert_eq!(matches.files.len(), 1);
        assert!(matches.files[0].path.ends_with("report[1].txt"));
    }

    #[test]
    fn invalid_globs_are_rejected_before_missing_base_lookup() {
        let directory = tempfile::tempdir().unwrap();
        let patterns = ["missing/[".to_string()];
        let readable_error = expand(&patterns, directory.path(), directory.path(), 10)
            .err()
            .unwrap();
        let workspace = WorkspaceFs::new(directory.path()).unwrap();
        let path_error = expand_file_paths(&patterns, directory.path(), &workspace, 10)
            .err()
            .unwrap();

        assert!(readable_error.contains("invalid glob pattern"));
        assert!(path_error.contains("invalid glob pattern"));
    }

    #[test]
    fn unprocessed_exact_paths_mark_results_truncated() {
        let directory = tempfile::tempdir().unwrap();
        let patterns = (0..3)
            .map(|index| {
                let name = format!("{index}.txt");
                std::fs::write(directory.path().join(&name), "text").unwrap();
                name
            })
            .collect::<Vec<_>>();

        let matches = expand(&patterns, directory.path(), directory.path(), 2).unwrap();

        assert_eq!(matches.files.len(), 2);
        assert!(matches.truncated);
    }
}

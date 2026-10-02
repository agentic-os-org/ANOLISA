//! Bounded regex search over files opened beneath the workspace root.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Cursor, Read};
use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use globset::{GlobBuilder, GlobMatcher};
use regex::bytes::Regex;
use serde_json::Value;

use super::workspace_fs::{WorkspaceFile, WorkspaceFs, WorkspaceNode};
use super::{Tool, ToolContext, ToolKind, ToolResult};

const MAX_FILES: usize = 100;
const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_MATCHES: usize = 100;
const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const IGNORE_RULE_WARNING: &str =
    "Some ignore rules could not be read or parsed; search results may be incomplete.";

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search for a regex pattern in workspace files. Returns at most 100 matches."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The regex pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "Directory or file to search in (default: cwd)"
                },
                "include": {
                    "type": "string",
                    "description": "File glob pattern to include (e.g., '*.rs')"
                }
            },
            "required": ["pattern"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    async fn invoke(&self, params: Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let pattern = params
            .get("pattern")
            .and_then(Value::as_str)
            .ok_or("missing 'pattern' parameter")?
            .to_string();
        let path = params
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or(".")
            .to_string();
        let include = params
            .get("include")
            .and_then(Value::as_str)
            .map(str::to_string);
        let cwd = ctx.cwd.clone();
        let workspace = ctx.workspace()?;

        tokio::task::spawn_blocking(move || {
            grep_workspace(&pattern, &path, include.as_deref(), &cwd, &workspace)
        })
        .await
        .map_err(|error| format!("Grep task failed: {error}"))?
    }
}

fn grep_workspace(
    pattern: &str,
    path: &str,
    include: Option<&str>,
    cwd: &Path,
    workspace: &WorkspaceFs,
) -> Result<ToolResult, String> {
    let matcher = Regex::new(pattern)
        .map_err(|error| format!("invalid regex pattern '{pattern}': {error}"))?;
    let include = include
        .map(IncludeMatcher::new)
        .transpose()
        .map_err(|error| format!("invalid include pattern: {error}"))?;
    let node = match workspace.open_node(cwd, path) {
        Ok(node) => node,
        Err(error) => return Ok(ToolResult::error(error)),
    };

    let respect_ignores = match include.as_ref() {
        Some(pattern) => pattern.excluded,
        None => true,
    };
    let (files, discovery_truncated, ignore_incomplete) = match node {
        WorkspaceNode::File(file) => (vec![GrepInput::Opened(file)], false, false),
        WorkspaceNode::Directory(directory) => {
            let cwd_relative = workspace.resolve_user_path(cwd, "")?;
            let walked = workspace.walk_relative_file_paths(
                directory,
                MAX_FILES,
                respect_ignores,
                |path| {
                    let include_path = path_relative_to_cwd(&cwd_relative, path);
                    include_file(&include, &include_path)
                },
            )?;
            (
                walked.paths.into_iter().map(GrepInput::Relative).collect(),
                walked.truncated,
                walked.ignore_incomplete,
            )
        }
    };

    let mut output = String::new();
    let mut matches = 0;
    let mut match_truncated = false;
    let mut output_truncated = false;
    let mut input_truncated = false;
    let mut file_errors = Vec::new();
    if ignore_incomplete {
        file_errors.push(IGNORE_RULE_WARNING.to_string());
    }
    for input in files {
        let file = match input {
            GrepInput::Opened(file) => file,
            GrepInput::Relative(relative_path) => {
                match workspace.open_relative_node(&relative_path) {
                    Ok(Some(WorkspaceNode::File(file))) => file,
                    Ok(Some(WorkspaceNode::Directory(_))) => {
                        file_errors.push(format!(
                            "File changed into a directory: {}",
                            workspace.display_path(&relative_path).display()
                        ));
                        continue;
                    }
                    Ok(None) => {
                        file_errors.push(format!(
                            "File disappeared before search: {}",
                            workspace.display_path(&relative_path).display()
                        ));
                        continue;
                    }
                    Err(error) => {
                        file_errors.push(error);
                        continue;
                    }
                }
            }
        };
        let scan = scan_file(
            file,
            &matcher,
            MAX_MATCHES.saturating_sub(matches),
            MAX_OUTPUT_BYTES.saturating_sub(output.len()),
        );
        if scan.binary {
            continue;
        }
        output.push_str(&scan.output);
        matches += scan.matches;
        match_truncated |= scan.match_truncated;
        output_truncated |= scan.output_truncated;
        input_truncated |= scan.input_truncated;
        if let Some(error) = scan.error {
            file_errors.push(error);
        }
        if match_truncated || output_truncated {
            break;
        }
    }

    if matches == 0 {
        if discovery_truncated
            || match_truncated
            || output_truncated
            || input_truncated
            || !file_errors.is_empty()
        {
            output.push_str("No matches found in the searched subset.\n");
            if discovery_truncated || match_truncated || output_truncated || input_truncated {
                output.push_str("\n[additional grep results omitted by output limits]\n");
            }
            append_file_errors(&mut output, &file_errors);
            return Ok(ToolResult::success(output));
        }
        return Ok(ToolResult::success("No matches found."));
    }
    if discovery_truncated || match_truncated || output_truncated || input_truncated {
        output.push_str("\n[additional grep results omitted by output limits]\n");
    }
    append_file_errors(&mut output, &file_errors);
    Ok(ToolResult::success(output))
}

enum GrepInput {
    Opened(WorkspaceFile),
    Relative(PathBuf),
}

fn append_file_errors(output: &mut String, errors: &[String]) {
    if errors.is_empty() {
        return;
    }
    output.push_str("\n[file errors]\n");
    output.push_str(&errors.join("\n"));
    output.push('\n');
}

struct IncludeMatcher {
    matcher: GlobMatcher,
    excluded: bool,
    match_basename: bool,
}

impl IncludeMatcher {
    fn new(pattern: &str) -> Result<Self, globset::Error> {
        let (excluded, pattern) = pattern
            .strip_prefix('!')
            .map_or((false, pattern), |pattern| (true, pattern));
        let (anchored, pattern) = pattern
            .strip_prefix('/')
            .map_or((false, pattern), |pattern| (true, pattern));
        let match_basename = !anchored && !pattern.contains('/');
        let matcher = GlobBuilder::new(pattern)
            .case_insensitive(cfg!(any(target_os = "macos", target_os = "windows")))
            .literal_separator(true)
            .build()?
            .compile_matcher();
        Ok(Self {
            matcher,
            excluded,
            match_basename,
        })
    }

    fn includes(&self, path: &Path) -> bool {
        let candidate = if self.match_basename {
            path.file_name().map_or(path, Path::new)
        } else {
            path
        };
        self.matcher.is_match(candidate) != self.excluded
    }
}

fn include_file(include: &Option<IncludeMatcher>, path: &Path) -> bool {
    let Some(pattern) = include else {
        return true;
    };
    pattern.includes(path)
}

fn path_relative_to_cwd(cwd: &Path, path: &Path) -> PathBuf {
    let cwd = normalized_workspace_components(cwd);
    let path = normalized_workspace_components(path);
    let common = cwd
        .iter()
        .zip(&path)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = PathBuf::new();
    for _ in common..cwd.len() {
        relative.push("..");
    }
    for component in &path[common..] {
        relative.push(component);
    }
    if relative.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        relative
    }
}

fn normalized_workspace_components(path: &Path) -> Vec<OsString> {
    let mut normalized = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(component) => normalized.push(component.to_os_string()),
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    normalized
}

struct FileScan {
    output: String,
    matches: usize,
    match_truncated: bool,
    output_truncated: bool,
    input_truncated: bool,
    binary: bool,
    error: Option<String>,
}

fn scan_file(
    file: WorkspaceFile,
    matcher: &Regex,
    max_matches: usize,
    max_output_bytes: usize,
) -> FileScan {
    let display_path = file.display_path;
    // Peek two bytes past the cap so a boundary LF or CRLF can be
    // distinguished from a physical line that continues beyond the cap.
    let mut reader = BufReader::new(file.file.take(MAX_FILE_BYTES + 2));
    let encoding = reader.fill_buf().ok().and_then(|prefix| {
        if prefix.starts_with(&[0xFF, 0xFE]) {
            Some(FileEncoding::Utf16(true))
        } else if prefix.starts_with(&[0xFE, 0xFF]) {
            Some(FileEncoding::Utf16(false))
        } else if prefix.starts_with(&[0xEF, 0xBB, 0xBF]) {
            Some(FileEncoding::Utf8Bom)
        } else {
            None
        }
    });
    let little_endian = match encoding {
        Some(FileEncoding::Utf16(little_endian)) => little_endian,
        Some(FileEncoding::Utf8Bom) => {
            reader.consume(3);
            return scan_reader(
                reader,
                display_path,
                matcher,
                max_matches,
                max_output_bytes,
                MAX_FILE_BYTES - 3,
            );
        }
        None => {
            return scan_reader(
                reader,
                display_path,
                matcher,
                max_matches,
                max_output_bytes,
                MAX_FILE_BYTES,
            );
        }
    };

    let mut raw = Vec::new();
    let read_error = reader
        .read_to_end(&mut raw)
        .err()
        .map(|error| format!("Failed to search {}: {error}", display_path.display()));
    let (decoded, retained_len) = decode_utf16(&raw, little_endian);
    let mut scan = scan_reader(
        BufReader::new(Cursor::new(decoded)),
        display_path,
        matcher,
        max_matches,
        max_output_bytes,
        retained_len,
    );
    if scan.error.is_none() {
        scan.error = read_error;
    }
    scan
}

enum FileEncoding {
    Utf8Bom,
    Utf16(bool),
}

fn scan_reader<R: BufRead>(
    mut reader: R,
    display_path: PathBuf,
    matcher: &Regex,
    max_matches: usize,
    max_output_bytes: usize,
    input_limit: u64,
) -> FileScan {
    let mut output = String::new();
    let mut matches = 0;
    let mut line = Vec::new();
    let mut line_number = 0;
    let mut searched_bytes = 0_u64;
    let mut input_truncated = false;
    let mut match_truncated = false;
    let mut output_truncated = false;
    let mut binary = false;
    let mut read_error = None;
    loop {
        line.clear();
        let read = match reader.read_until(b'\n', &mut line) {
            Ok(read) => read,
            Err(error) => {
                read_error = Some(format!(
                    "Failed to search {}: {error}",
                    display_path.display()
                ));
                break;
            }
        };
        if read == 0 {
            break;
        }
        if line.contains(&0) {
            binary = true;
            output.clear();
            matches = 0;
            match_truncated = false;
            output_truncated = false;
            break;
        }
        let retained_len = (input_limit.saturating_sub(searched_bytes) as usize).min(line.len());
        searched_bytes += read as u64;
        let matched = if retained_len < line.len() {
            input_truncated = true;
            if retained_len == 0 {
                false
            } else if line_terminator_starts_at_boundary(&line, retained_len) {
                truncate_at_line_terminator(&mut line, retained_len);
                matcher.is_match(&line)
            } else {
                let matched = has_match_within_prefix(matcher, &line, retained_len);
                line.truncate(retained_len);
                matched
            }
        } else {
            trim_line_ending(&mut line);
            matcher.is_match(&line)
        };
        line_number += 1;
        if !matched {
            if input_truncated {
                break;
            }
            continue;
        }
        if matches >= max_matches {
            match_truncated = true;
            if input_truncated {
                break;
            }
            continue;
        }
        if output_truncated {
            if input_truncated {
                break;
            }
            continue;
        }
        let rendered = String::from_utf8_lossy(&line);
        let matched_line = format!("{}:{line_number}:{rendered}\n", display_path.display());
        if output.len().saturating_add(matched_line.len()) > max_output_bytes {
            output_truncated = true;
        } else {
            output.push_str(&matched_line);
            matches += 1;
        }
        if input_truncated {
            break;
        }
    }
    FileScan {
        output,
        matches,
        match_truncated,
        output_truncated,
        input_truncated,
        binary,
        error: read_error,
    }
}

fn decode_utf16(raw: &[u8], little_endian: bool) -> (Vec<u8>, u64) {
    let content = raw.get(2..).unwrap_or_default();
    let decoded = decode_utf16_bytes(content, little_endian);
    if raw.len() <= MAX_FILE_BYTES as usize {
        let retained_len = decoded.len() as u64;
        return (decoded.into_bytes(), retained_len);
    }

    let mut retained_end = MAX_FILE_BYTES as usize;
    retained_end -= retained_end.saturating_sub(2) % 2;
    if retained_end >= 4 {
        let last = read_utf16_unit(
            [raw[retained_end - 2], raw[retained_end - 1]],
            little_endian,
        );
        if (0xD800..=0xDBFF).contains(&last) {
            retained_end -= 2;
        }
    }
    let retained = decode_utf16_bytes(&raw[2..retained_end], little_endian);
    (decoded.into_bytes(), retained.len() as u64)
}

fn decode_utf16_bytes(bytes: &[u8], little_endian: bool) -> String {
    let units = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|bytes| read_utf16_unit([bytes[0], bytes[1]], little_endian))
        .collect::<Vec<_>>();
    let has_partial_unit = bytes.len() & 1 == 1;
    let mut decoded = String::from_utf16_lossy(&units);
    if has_partial_unit {
        decoded.push(char::REPLACEMENT_CHARACTER);
    }
    decoded
}

fn read_utf16_unit(bytes: [u8; 2], little_endian: bool) -> u16 {
    if little_endian {
        u16::from_le_bytes(bytes)
    } else {
        u16::from_be_bytes(bytes)
    }
}

fn trim_line_ending(line: &mut Vec<u8>) {
    if line.last() == Some(&b'\n') {
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
    }
}

fn truncate_at_line_terminator(line: &mut Vec<u8>, retained_len: usize) {
    let split_crlf = line.get(retained_len..) == Some(b"\n")
        && retained_len
            .checked_sub(1)
            .and_then(|index| line.get(index))
            == Some(&b'\r');
    line.truncate(retained_len);
    if split_crlf {
        line.pop();
    }
}

fn line_terminator_starts_at_boundary(line: &[u8], retained_len: usize) -> bool {
    let omitted = &line[retained_len..];
    omitted == b"\n" || omitted == b"\r\n"
}

fn has_match_within_prefix(matcher: &Regex, line: &[u8], retained_len: usize) -> bool {
    matcher
        .find_iter(line)
        .any(|matched| matched.end() <= retained_len)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn test_ctx_in(directory: &Path) -> ToolContext {
        ToolContext::new(
            directory.to_path_buf(),
            "test".to_string(),
            directory.to_path_buf(),
        )
    }

    fn mark_git_repository(directory: &Path) {
        std::fs::create_dir(directory.join(".git")).unwrap();
    }

    #[tokio::test]
    async fn grep_finds_pattern() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("test.txt"),
            "hello world\ngoodbye world\n",
        )
        .unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "hello", "path": "."}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("hello"));
    }

    #[tokio::test]
    async fn grep_end_anchor_matches_before_line_endings() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("test.txt"),
            b"unix world\nwindows world\r\nnot world!\n",
        )
        .unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "world$", "path": "."}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("unix world"));
        assert!(result.output.contains("windows world"));
        assert!(!result.output.contains("not world!"));
    }

    #[tokio::test]
    async fn grep_preserves_lone_and_repeated_carriage_returns() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("test.txt"),
            b"double hit\r\r\nclean hit\nlone hit\r",
        )
        .unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "hit$", "path": "."}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(!result.output.contains("double hit"));
        assert!(result.output.contains("clean hit"));
        assert!(!result.output.contains("lone hit"));
    }

    #[test]
    fn cut_through_line_does_not_create_an_artificial_end_match() {
        let matcher = Regex::new("hit$").unwrap();
        let line = b"prefix hitx\n";
        let retained_len = "prefix hit".len();

        assert!(!has_match_within_prefix(&matcher, line, retained_len));
        assert!(!line_terminator_starts_at_boundary(line, retained_len));
    }

    #[test]
    fn boundary_line_ending_preserves_real_end_match() {
        let matcher = Regex::new("hit$").unwrap();
        let mut line = b"prefix hit\r\n".to_vec();
        let retained_len = "prefix hit".len();

        assert!(line_terminator_starts_at_boundary(&line, retained_len));
        truncate_at_line_terminator(&mut line, retained_len);
        assert!(matcher.is_match(&line));
    }

    #[tokio::test]
    async fn grep_no_match() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("test.txt"), "hello world\n").unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "nonexistent_xyz", "path": "."}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("No matches"));
        assert!(!result.output.contains("omitted"));
    }

    #[tokio::test]
    async fn grep_reports_match_limit_only_after_an_extra_match() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("matches.txt");
        let content = (0..MAX_MATCHES)
            .map(|index| format!("hit {index}\n"))
            .collect::<String>();
        std::fs::write(&path, &content).unwrap();

        let complete = GrepTool
            .invoke(
                serde_json::json!({"pattern": "hit", "path": "matches.txt"}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();
        std::fs::write(&path, format!("{content}hit extra\n")).unwrap();
        let truncated = GrepTool
            .invoke(
                serde_json::json!({"pattern": "hit", "path": "matches.txt"}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!complete.is_error, "{}", complete.output);
        assert!(!complete.output.contains("omitted"), "{}", complete.output);
        assert!(!truncated.is_error, "{}", truncated.output);
        assert!(truncated.output.contains("additional grep results omitted"));
    }

    #[tokio::test]
    async fn grep_reports_when_no_match_result_is_incomplete() {
        let directory = tempfile::tempdir().unwrap();
        for index in 0..=MAX_FILES {
            std::fs::write(directory.path().join(format!("{index:03}.txt")), "text\n").unwrap();
        }

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "not-present", "path": "."}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("searched subset"));
        assert!(result.output.contains("additional grep results omitted"));
    }

    #[tokio::test]
    async fn grep_reports_match_omitted_by_output_limit() {
        let directory = tempfile::tempdir().unwrap();
        let content = format!("hit{}\n", "x".repeat(MAX_OUTPUT_BYTES));
        std::fs::write(directory.path().join("large-line.txt"), content).unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "hit", "path": "."}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("searched subset"));
        assert!(result.output.contains("additional grep results omitted"));
    }

    #[tokio::test]
    async fn grep_reports_file_truncated_by_input_limit() {
        let directory = tempfile::tempdir().unwrap();
        let mut content = vec![b'x'; MAX_FILE_BYTES as usize];
        content.extend_from_slice(b"hit\n");
        std::fs::write(directory.path().join("large.txt"), content).unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "hit", "path": "."}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("searched subset"));
        assert!(result.output.contains("additional grep results omitted"));
    }

    #[tokio::test]
    async fn grep_with_include() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("test.rs"), "fn main() {}\n").unwrap();
        std::fs::write(directory.path().join("test.txt"), "fn main() {}\n").unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({
                    "pattern": "fn main",
                    "path": ".",
                    "include": "*.rs"
                }),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("test.rs"));
        assert!(!result.output.contains("test.txt"));
    }

    #[tokio::test]
    async fn grep_matches_include_relative_to_shell_cwd() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().join("sub");
        let source = cwd.join("src");
        let nested = source.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(source.join("main.rs"), "hit\n").unwrap();
        std::fs::write(nested.join("nested.rs"), "hit\n").unwrap();
        let context = ToolContext::new(cwd, "test".to_string(), directory.path().to_path_buf());

        let prefixed = GrepTool
            .invoke(
                serde_json::json!({
                    "pattern": "hit",
                    "path": "src",
                    "include": "src/*.rs"
                }),
                &context,
            )
            .await
            .unwrap();
        let anchored = GrepTool
            .invoke(
                serde_json::json!({
                    "pattern": "hit",
                    "path": "src",
                    "include": "/src/*.rs"
                }),
                &context,
            )
            .await
            .unwrap();
        let basename = GrepTool
            .invoke(
                serde_json::json!({
                    "pattern": "hit",
                    "path": "src",
                    "include": "*.rs"
                }),
                &context,
            )
            .await
            .unwrap();

        assert!(!prefixed.is_error, "{}", prefixed.output);
        assert!(prefixed.output.contains("main.rs"));
        assert!(!prefixed.output.contains("nested.rs"));
        assert!(!anchored.is_error, "{}", anchored.output);
        assert!(anchored.output.contains("main.rs"));
        assert!(!anchored.output.contains("nested.rs"));
        assert!(!basename.is_error, "{}", basename.output);
        assert!(basename.output.contains("main.rs"));
        assert!(basename.output.contains("nested.rs"));
    }

    #[tokio::test]
    async fn grep_does_not_filter_an_explicit_file_with_include() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("main.rs"), "hit\n").unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({
                    "pattern": "hit",
                    "path": "main.rs",
                    "include": "*.txt"
                }),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains("main.rs"));
        assert!(result.output.contains("hit"));
    }

    #[tokio::test]
    async fn grep_include_preserves_braces_and_exclusions() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("test.rs"), "hit\n").unwrap();
        std::fs::write(directory.path().join("test.ts"), "hit\n").unwrap();
        std::fs::write(directory.path().join("test.txt"), "hit\n").unwrap();

        let included = GrepTool
            .invoke(
                serde_json::json!({
                    "pattern": "hit",
                    "path": ".",
                    "include": "*.{rs,ts}"
                }),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();
        let excluded = GrepTool
            .invoke(
                serde_json::json!({
                    "pattern": "hit",
                    "path": ".",
                    "include": "!*.rs"
                }),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(included.output.contains("test.rs"));
        assert!(included.output.contains("test.ts"));
        assert!(!included.output.contains("test.txt"));
        assert!(!excluded.output.contains("test.rs"));
        assert!(excluded.output.contains("test.ts"));
        assert!(excluded.output.contains("test.txt"));
    }

    #[tokio::test]
    async fn grep_skips_hidden_and_gitignored_trees_before_file_limits() {
        let directory = tempfile::tempdir().unwrap();
        mark_git_repository(directory.path());
        let hidden = directory.path().join(".cache");
        let ignored = directory.path().join("build");
        let source = directory.path().join("src");
        std::fs::create_dir(&hidden).unwrap();
        std::fs::create_dir(&ignored).unwrap();
        std::fs::create_dir(&source).unwrap();
        std::fs::write(directory.path().join(".gitignore"), "build/\n").unwrap();
        for index in 0..=MAX_FILES {
            std::fs::write(hidden.join(format!("{index:03}.txt")), "hit\n").unwrap();
            std::fs::write(ignored.join(format!("{index:03}.txt")), "hit\n").unwrap();
        }
        std::fs::write(source.join("main.rs"), "hit\n").unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "hit", "path": "."}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains("src/main.rs"));
        assert!(!result.output.contains(".cache"));
        assert!(!result.output.contains("build/"));
        assert!(!result.output.contains("omitted"));
    }

    #[tokio::test]
    async fn positive_include_overrides_ignore_rules() {
        let directory = tempfile::tempdir().unwrap();
        mark_git_repository(directory.path());
        std::fs::write(directory.path().join(".gitignore"), "generated.rs\n").unwrap();
        std::fs::write(directory.path().join("generated.rs"), "hit\n").unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({
                    "pattern": "hit",
                    "path": ".",
                    "include": "*.rs"
                }),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains("generated.rs"));
    }

    #[tokio::test]
    async fn nested_search_loads_parent_ignore_rules() {
        let directory = tempfile::tempdir().unwrap();
        mark_git_repository(directory.path());
        let generated = directory.path().join("src/generated");
        std::fs::create_dir_all(&generated).unwrap();
        std::fs::write(directory.path().join(".gitignore"), "src/generated/\n").unwrap();
        for index in 0..=MAX_FILES {
            std::fs::write(generated.join(format!("{index:03}.rs")), "hit\n").unwrap();
        }
        std::fs::write(directory.path().join("src/main.rs"), "hit\n").unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "hit", "path": "src"}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains("src/main.rs"));
        assert!(!result.output.contains("src/generated"));
        assert!(!result.output.contains("omitted"));
    }

    #[tokio::test]
    async fn parent_search_normalizes_paths_for_ignore_matching() {
        let directory = tempfile::tempdir().unwrap();
        mark_git_repository(directory.path());
        let source = directory.path().join("src");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(directory.path().join(".gitignore"), "/generated.txt\n").unwrap();
        std::fs::write(source.join(".gitignore"), "/visible.txt\n").unwrap();
        std::fs::write(directory.path().join("generated.txt"), "hit generated\n").unwrap();
        std::fs::write(directory.path().join("visible.txt"), "hit visible\n").unwrap();
        let context = ToolContext::new(source, "test".to_string(), directory.path().to_path_buf());

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "hit", "path": ".."}),
                &context,
            )
            .await
            .unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert!(
            !result.output.contains("hit generated"),
            "{}",
            result.output
        );
        assert!(result.output.contains("hit visible"), "{}", result.output);
    }

    #[tokio::test]
    async fn grep_honors_hidden_path_whitelists() {
        let directory = tempfile::tempdir().unwrap();
        mark_git_repository(directory.path());
        std::fs::write(directory.path().join(".gitignore"), "!.generated.rs\n").unwrap();
        std::fs::write(directory.path().join(".generated.rs"), "hit\n").unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "hit", "path": "."}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains(".generated.rs"));
    }

    #[tokio::test]
    async fn grep_loads_rgignore_after_gitignore() {
        let directory = tempfile::tempdir().unwrap();
        mark_git_repository(directory.path());
        std::fs::write(directory.path().join(".gitignore"), "generated.rs\n").unwrap();
        std::fs::write(directory.path().join(".rgignore"), "!generated.rs\n").unwrap();
        std::fs::write(directory.path().join("generated.rs"), "hit\n").unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "hit", "path": "."}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains("generated.rs"));
    }

    #[tokio::test]
    async fn grep_requires_repository_marker_for_gitignore() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join(".gitignore"), "git-only.txt\n").unwrap();
        std::fs::write(directory.path().join(".ignore"), "always.txt\n").unwrap();
        std::fs::write(directory.path().join("git-only.txt"), "hit git\n").unwrap();
        std::fs::write(directory.path().join("always.txt"), "hit always\n").unwrap();
        std::fs::write(directory.path().join("visible.txt"), "hit visible\n").unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "hit", "path": "."}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains("git-only.txt"));
        assert!(!result.output.contains("always.txt"));
        assert!(result.output.contains("visible.txt"));
    }

    #[tokio::test]
    async fn git_file_marks_worktree_as_a_repository() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join(".git"), "gitdir: ../metadata\n").unwrap();
        std::fs::write(directory.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(directory.path().join("ignored.txt"), "hit ignored\n").unwrap();
        std::fs::write(directory.path().join("visible.txt"), "hit visible\n").unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "hit", "path": "."}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert!(!result.output.contains("ignored.txt"));
        assert!(result.output.contains("visible.txt"));
    }

    #[tokio::test]
    async fn nested_git_repository_stops_parent_gitignore_rules() {
        let directory = tempfile::tempdir().unwrap();
        mark_git_repository(directory.path());
        let nested = directory.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        mark_git_repository(&nested);
        std::fs::write(directory.path().join(".gitignore"), "blocked.txt\n").unwrap();
        let root_blocked = directory.path().join("blocked.txt");
        let nested_blocked = nested.join("blocked.txt");
        std::fs::write(&root_blocked, "hit root\n").unwrap();
        std::fs::write(&nested_blocked, "hit nested\n").unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "hit", "path": "."}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert!(!result.output.contains("hit root"), "{}", result.output);
        assert!(result.output.contains("hit nested"), "{}", result.output);
    }

    #[tokio::test]
    async fn grep_respects_repository_exclude_with_lowest_precedence() {
        let directory = tempfile::tempdir().unwrap();
        mark_git_repository(directory.path());
        std::fs::create_dir_all(directory.path().join(".git/info")).unwrap();
        std::fs::write(
            directory.path().join(".git/info/exclude"),
            "excluded.txt\nrestored.txt\n",
        )
        .unwrap();
        std::fs::write(directory.path().join(".gitignore"), "!restored.txt\n").unwrap();
        std::fs::write(directory.path().join("excluded.txt"), "hit excluded\n").unwrap();
        std::fs::write(directory.path().join("restored.txt"), "hit restored\n").unwrap();
        std::fs::write(directory.path().join("visible.txt"), "hit visible\n").unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "hit", "path": "."}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert!(!result.output.contains("hit excluded"), "{}", result.output);
        assert!(result.output.contains("hit restored"), "{}", result.output);
        assert!(result.output.contains("hit visible"), "{}", result.output);
    }

    #[tokio::test]
    async fn grep_continues_with_non_utf8_ignore_rules() {
        let directory = tempfile::tempdir().unwrap();
        mark_git_repository(directory.path());
        std::fs::write(directory.path().join(".gitignore"), b"\xff\n").unwrap();
        std::fs::write(directory.path().join("main.rs"), "hit\n").unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "hit", "path": "."}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains("main.rs"));
        assert!(result.output.contains(IGNORE_RULE_WARNING));
        assert!(!result.output.contains("output limits"));
    }

    #[tokio::test]
    async fn grep_loads_later_ignore_sources_after_an_unreadable_file() {
        use std::os::unix::fs::PermissionsExt;

        if nix::unistd::Uid::effective().as_raw() == 0 {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        mark_git_repository(directory.path());
        let gitignore = directory.path().join(".gitignore");
        std::fs::write(&gitignore, "unused.txt\n").unwrap();
        std::fs::set_permissions(&gitignore, std::fs::Permissions::from_mode(0o000)).unwrap();
        std::fs::write(directory.path().join(".ignore"), "generated.txt\n").unwrap();
        std::fs::write(directory.path().join("generated.txt"), "hit generated\n").unwrap();
        std::fs::write(directory.path().join("visible.txt"), "hit visible\n").unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "hit", "path": "."}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();
        std::fs::set_permissions(&gitignore, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert!(
            !result.output.contains("hit generated"),
            "{}",
            result.output
        );
        assert!(result.output.contains("hit visible"), "{}", result.output);
        assert!(result.output.contains(IGNORE_RULE_WARNING));
    }

    #[tokio::test]
    async fn grep_reports_malformed_ignore_rules_and_continues() {
        let directory = tempfile::tempdir().unwrap();
        mark_git_repository(directory.path());
        std::fs::write(directory.path().join(".gitignore"), "[z-a]\nignored.txt\n").unwrap();
        std::fs::write(directory.path().join("ignored.txt"), "hit ignored\n").unwrap();
        std::fs::write(directory.path().join("main.rs"), "hit main\n").unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "hit", "path": "."}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains("main.rs"));
        assert!(!result.output.contains("ignored.txt"));
        assert!(result.output.contains(IGNORE_RULE_WARNING));
        assert!(!result.output.contains("output limits"));
    }

    #[tokio::test]
    async fn grep_bounds_ignore_file_memory() {
        let directory = tempfile::tempdir().unwrap();
        mark_git_repository(directory.path());
        std::fs::write(
            directory.path().join(".gitignore"),
            vec![b'x'; 1024 * 1024 + 1],
        )
        .unwrap();
        std::fs::write(directory.path().join("main.rs"), "hit\n").unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "hit", "path": "."}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains("main.rs"));
        assert!(result.output.contains(IGNORE_RULE_WARNING));
    }

    #[tokio::test]
    async fn grep_discards_matches_from_binary_files() {
        let directory = tempfile::tempdir().unwrap();
        let binary = format!("{}\0hit after\n", "hit before\n".repeat(MAX_MATCHES));
        std::fs::write(directory.path().join("binary.dat"), binary).unwrap();
        std::fs::write(directory.path().join("text.txt"), "hit text\n").unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "hit", "path": "."}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert!(!result.output.contains("binary.dat"));
        assert!(result.output.contains("text.txt"));
        assert!(result.output.contains("hit text"));
    }

    #[tokio::test]
    async fn grep_decodes_utf16_bom_files() {
        let directory = tempfile::tempdir().unwrap();
        for (name, little_endian) in [("little.txt", true), ("big.txt", false)] {
            let mut content = if little_endian {
                vec![0xFF, 0xFE]
            } else {
                vec![0xFE, 0xFF]
            };
            for unit in "hit text\n".encode_utf16() {
                let bytes = if little_endian {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                };
                content.extend_from_slice(&bytes);
            }
            std::fs::write(directory.path().join(name), content).unwrap();
        }

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "hit", "path": "."}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains("little.txt"));
        assert!(result.output.contains("big.txt"));
        assert!(result.output.contains("hit text"));
    }

    #[tokio::test]
    async fn grep_strips_utf8_bom_before_matching() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("bom.txt"), b"\xEF\xBB\xBFhello\n").unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "^hello$", "path": "."}),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains("bom.txt:1:hello"));
    }

    #[test]
    fn grep_reports_file_read_errors_as_incomplete_results() {
        let root = Path::new("/proc/self");
        if !root.join("mem").exists() {
            return;
        }
        let Ok(workspace) = WorkspaceFs::new(root) else {
            return;
        };

        let result = grep_workspace("hit", "mem", None, root, &workspace).unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains("searched subset"));
        assert!(result.output.contains("[file errors]"));
    }

    #[tokio::test]
    async fn grep_include_skips_unreadable_nonmatching_files() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("test.rs"), "fn main() {}\n").unwrap();
        let excluded = directory.path().join(".env");
        std::fs::write(&excluded, "SECRET=value\n").unwrap();
        std::fs::set_permissions(&excluded, std::fs::Permissions::from_mode(0o000)).unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({
                    "pattern": "fn main",
                    "path": ".",
                    "include": "*.rs"
                }),
                &test_ctx_in(directory.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains("test.rs"));
        assert!(!result.output.contains(".env"));
    }

    #[tokio::test]
    async fn rejects_exact_symlink_that_escapes_workspace() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(parent.path().join("outside.txt"), "secret\n").unwrap();
        symlink(parent.path().join("outside.txt"), root.join("outside-link")).unwrap();

        let result = GrepTool
            .invoke(
                serde_json::json!({"pattern": "secret", "path": "outside-link"}),
                &test_ctx_in(&root),
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("escapes workspace root"));
        assert!(!result.output.contains("secret\n"));
    }
}

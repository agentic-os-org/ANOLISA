//! Byte-reversible prefix sharing for newline-separated POSIX paths.

use tokenless_protocol::estimate_tokens;

const HEADER: &str =
    "[Path list; all received paths retained; full path = base + each paths entry]\n";

const GROUPED_HEADER: &str = "[Paths grouped by directory; full path = base + Dir + entry";

/// Validated path records and the original line-ending contract.
pub(super) struct PathList<'a> {
    paths: Vec<&'a str>,
    line_ending: &'static str,
    trailing_newline: bool,
    original_len: usize,
}

/// Rejects the whole list when any record or line ending is ambiguous.
pub(super) fn parse(input: &str) -> Option<PathList<'_>> {
    let line_ending = if input.contains("\r\n") { "\r\n" } else { "\n" };
    let trailing_newline = input.ends_with(line_ending);
    let body = input.strip_suffix(line_ending).unwrap_or(input);
    let paths: Vec<_> = body.split(line_ending).collect();
    if paths.len() < 3 || !paths.iter().all(|path| is_path(path)) {
        return None;
    }
    Some(PathList {
        paths,
        line_ending,
        trailing_newline,
        original_len: input.len(),
    })
}

fn is_path(path: &str) -> bool {
    if path.is_empty()
        || path.trim() != path
        || path.contains(':')
        || path.starts_with("//")
        || path.chars().any(char::is_control)
    {
        return false;
    }
    if path.starts_with('/') || path.starts_with("./") || path.starts_with("../") {
        return true;
    }
    // Bare relative roots must be simple identifiers, so prose mentioning a
    // slash does not acquire the path-list output contract.
    path.split_once('/').is_some_and(|(root, _)| {
        !root.is_empty()
            && root
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-'))
    })
}

/// Encodes an exact common-prefix view only when the whole view saves tokens.
pub(super) fn compress(input: &str) -> Option<String> {
    let list = parse(input)?;
    [prefix_view(&list), grouped_view(&list)]
        .into_iter()
        .flatten()
        .filter(|output| {
            output.len() < input.len() && estimate_tokens(output) < estimate_tokens(input)
        })
        // Tokens are the objective; bytes are only a proxy for them.
        .min_by_key(|output| (estimate_tokens(output), output.len()))
}

/// Shares the longest common directory prefix as compact JSON.
fn prefix_view(list: &PathList<'_>) -> Option<String> {
    let end = common_slash_prefix(&list.paths)?;
    let base = &list.paths[0][..end];
    let paths: Vec<_> = list.paths.iter().map(|path| &path[end..]).collect();
    let encoded = serde_json::to_string(&serde_json::json!({
        "base": base,
        "paths": paths,
        "line_ending": list.line_ending,
        "trailing_newline": list.trailing_newline,
    }))
    .ok()?;
    Some(format!("{HEADER}{encoded}"))
}

/// Longest prefix of every path that ends at a slash boundary, as a byte
/// offset into the first path. A shared byte prefix can end in the middle of
/// a Unicode character, but a slash is always a boundary.
fn common_slash_prefix(paths: &[&str]) -> Option<usize> {
    let first = paths.first()?;
    let shared = paths.iter().skip(1).fold(first.len(), |end, path| {
        first.as_bytes()[..end]
            .iter()
            .zip(path.as_bytes())
            .take_while(|(left, right)| left == right)
            .count()
    });
    first.as_bytes()[..shared]
        .iter()
        .rposition(|b| *b == b'/')
        .map(|index| index + 1)
}

/// An entry line that cannot be read as a bare path is JSON-quoted.
fn grouped_entry(entry: &str) -> String {
    let ambiguous = entry.is_empty()
        || entry.starts_with("Base=")
        || entry.starts_with("Dir=")
        || entry.starts_with('"');
    if ambiguous {
        serde_json::Value::String(entry.to_owned()).to_string()
    } else {
        entry.to_owned()
    }
}

/// Shares each directory among its consecutive entries.
///
/// Restores byte for byte: the header states the joining rule and records the
/// line ending and final-newline state once, so an agent can derive any full
/// path without another call.
fn grouped_view(list: &PathList<'_>) -> Option<String> {
    let base_end = common_slash_prefix(&list.paths)?;
    let base = &list.paths[0][..base_end];
    let mut output = String::from(GROUPED_HEADER);
    match (list.line_ending, list.trailing_newline) {
        ("\r\n", true) => {
            output.push_str("; entries LF-separated; restore with CRLF; final newline]\n")
        }
        ("\r\n", false) => {
            output.push_str("; entries LF-separated; restore with CRLF; no final newline]\n")
        }
        ("\n", false) => {
            output.push_str("; entries LF-separated; restore with LF; no final newline]\n")
        }
        _ => output.push_str("; entries LF-separated; restore with LF; final newline]\n"),
    }
    output.push_str(&format!(
        "Base={}\n",
        serde_json::Value::String(base.to_owned())
    ));
    let mut current_dir = None;
    for path in &list.paths {
        let entry = &path[base_end..];
        // Entries directly under the base have no directory of their own;
        // they stay under the implicit empty Dir so `Dir=` always holds a
        // directory (ending in `/`), never a bare filename.
        let dir_end = entry.rfind('/').map_or(0, |index| index + 1);
        let dir = &entry[..dir_end];
        if current_dir != Some(dir) && !(dir.is_empty() && current_dir.is_none()) {
            output.push_str(&format!(
                "Dir={}\n",
                serde_json::Value::String(dir.to_owned())
            ));
            current_dir = Some(dir);
        }
        output.push_str(&grouped_entry(&entry[dir_end..]));
        output.push('\n');
        if output.len() >= list.original_len {
            return None;
        }
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restore(view: &str) -> String {
        if let Some(json) = view.strip_prefix(HEADER) {
            let data: serde_json::Value = serde_json::from_str(json).unwrap();
            let base = data["base"].as_str().unwrap();
            let ending = data["line_ending"].as_str().unwrap();
            let paths = data["paths"].as_array().unwrap();
            let mut output = paths
                .iter()
                .map(|path| format!("{base}{}", path.as_str().unwrap()))
                .collect::<Vec<_>>()
                .join(ending);
            if data["trailing_newline"].as_bool().unwrap() {
                output.push_str(ending);
            }
            return output;
        }
        let mut lines = view.lines();
        let header = lines.next().unwrap();
        assert!(header.starts_with(GROUPED_HEADER), "{header:?}");
        let (line_ending, trailing_newline) = if header.contains("CRLF; final newline") {
            ("\r\n", true)
        } else if header.contains("CRLF; no final newline") {
            ("\r\n", false)
        } else if header.contains("no final newline") {
            ("\n", false)
        } else {
            ("\n", true)
        };
        let base: String =
            serde_json::from_str(lines.next().unwrap().strip_prefix("Base=").unwrap()).unwrap();
        let mut dir = String::new();
        let mut output = String::new();
        let mut entries = 0usize;
        for line in lines {
            if let Some(quoted) = line.strip_prefix("Dir=") {
                dir = serde_json::from_str(quoted).unwrap();
                continue;
            }
            let entry: String = if line.starts_with('"') {
                serde_json::from_str(line).unwrap()
            } else {
                line.to_owned()
            };
            if entries > 0 {
                output.push_str(line_ending);
            }
            entries += 1;
            output.push_str(&base);
            output.push_str(&dir);
            output.push_str(&entry);
        }
        if trailing_newline {
            output.push_str(line_ending);
        }
        output
    }

    #[test]
    fn preserves_all_paths_order_duplicates_and_line_endings() {
        for root in [
            "/workspace/very_long_project_directory/",
            "./项目 中文/",
            "../project/",
            "src/",
            "/workspace/../project//",
        ] {
            for ending in ["\n", "\r\n"] {
                for trailing in [true, false] {
                    let names = [
                        "first file.rs",
                        "中文.rs",
                        "quote\"back\\slash.rs",
                        "first file.rs",
                    ];
                    let paths: Vec<_> = (0..60)
                        .map(|i| {
                            format!(
                                "{root}shared_long_directory_name/nested/{}/{name}",
                                i % 3,
                                name = names[i % names.len()]
                            )
                        })
                        .collect();
                    let input = format!(
                        "{}{}",
                        paths.join(ending),
                        if trailing { ending } else { "" }
                    );
                    let candidate = super::super::SearchResultsCompressor
                        .compress(&input)
                        .unwrap();
                    assert_eq!(restore(&candidate), input);
                    assert!(candidate.len() < input.len());
                    assert!(estimate_tokens(&candidate) < estimate_tokens(&input));
                }
            }
        }
    }

    #[test]
    fn unicode_prefixes_end_only_at_directory_boundaries() {
        let input = ["文", "斌", "文"]
            .map(|name| format!("/long_shared_directory_for_unicode_regression/{name}/file.rs\n"))
            .concat()
            .repeat(10);
        assert_eq!(restore(&compress(&input).unwrap()), input);
    }

    #[test]
    fn rejects_mixed_or_ambiguous_input_including_a_late_bad_record() {
        let valid = "/workspace/long_directory/subdirectory/file.rs\n".repeat(60);
        for bad in [
            "\n",
            "---\n",
            "60\n",
            "find: /private: Permission denied\n",
            "https://example.com/file.rs\n",
            "C:\\project\\file.rs\n",
            "/workspace/file.rs\0",
            "/workspace/file.rs\r\n",
            "/workspace/file\tname.rs\n",
            "some prose about /workspace/file.rs\n",
            "// a comment about /workspace/file.rs\n",
        ] {
            let input = format!("{valid}{bad}");
            assert!(parse(&input).is_none(), "{bad:?}");
            assert!(compress(&input).is_none(), "{bad:?}");
        }
        assert!(parse(&format!("{}{}", valid.repeat(4), "not a path\n")).is_none());
    }

    #[test]
    fn small_or_unrelated_paths_are_not_expanded() {
        for input in [
            String::new(),
            "/long_project_directory/file.rs\n".repeat(2),
            "a/b\na/c\na/d\n".to_owned(),
            (0..60)
                .map(|i| format!("/unique_directory_{i}/file.rs\n"))
                .collect(),
        ] {
            assert!(compress(&input).is_none());
        }
    }

    #[test]
    fn grouped_view_restores_deep_relative_trees_byte_for_byte() {
        // A short shared prefix with deep per-entry directory chains is where
        // grouping doubles the reduction over the prefix view.
        let paths: Vec<_> = [
            "crates/tokenless-ccr/Cargo.toml",
            "crates/tokenless-ccr/src/backends/in_memory.rs",
            "crates/tokenless-ccr/src/backends/mod.rs",
            "crates/tokenless-ccr/src/backends/sqlite.rs",
            "crates/tokenless-ccr/src/key.rs",
            "crates/tokenless-ccr/src/lib.rs",
            "crates/tokenless-protocol/src/lib.rs",
        ]
        .iter()
        .cycle()
        .take(21)
        .copied()
        .collect();
        for ending in ["\n", "\r\n"] {
            for trailing in [true, false] {
                let input = format!(
                    "{}{}",
                    paths.join(ending),
                    if trailing { ending } else { "" }
                );
                let candidate = compress(&input).unwrap();
                assert!(
                    candidate.starts_with(GROUPED_HEADER),
                    "grouping must win on sorted relative trees: {candidate}"
                );
                assert_eq!(restore(&candidate), input);
            }
        }
    }

    #[test]
    fn grouped_entries_ambiguous_as_bare_lines_are_json_quoted() {
        let paths: Vec<_> = [
            "root/shared_directory/Base=forged.json",
            "root/shared_directory/",
            "root/shared_directory/Dir=also_forged.json",
            "root/shared_directory/\"leading_quote.rs",
            "root/shared_directory/normal.rs",
        ]
        .iter()
        .cycle()
        .take(20)
        .copied()
        .collect();
        let input = format!("{}\n", paths.join("\n"));
        let list = parse(&input).unwrap();
        let candidate = grouped_view(&list).unwrap();
        assert_eq!(restore(&candidate), input);
        // The ambiguous entries must appear JSON-quoted, not bare.
        assert!(
            candidate.contains("\n\"Base=forged.json\"\n")
                || candidate.contains("\"Base=forged.json\""),
            "ambiguous entry not quoted:\n{candidate}"
        );
    }

    #[test]
    fn directory_locality_selects_the_smaller_view() {
        // No locality: every directory is unique, so grouping adds header
        // overhead and the prefix view must win (or neither emits).
        let shuffled: Vec<_> = (0..60)
            .map(|i| format!("/workspace/project/unique_directory_{i}/file.rs"))
            .collect();
        let input = format!("{}\n", shuffled.join("\n"));
        let candidate = compress(&input).unwrap();
        assert!(candidate.starts_with(HEADER), "{candidate}");
        assert_eq!(restore(&candidate), input);

        // A single directory below the base restores byte for byte whichever
        // view wins.
        let single_dir: Vec<_> = (0..30)
            .map(|i| format!("/workspace/project/shared_directory/file_{i:03}.rs"))
            .collect();
        let input = format!("{}\n", single_dir.join("\n"));
        let candidate = compress(&input).unwrap();
        assert_eq!(restore(&candidate), input);
    }

    #[test]
    fn entries_directly_under_base_never_make_dir_hold_a_filename() {
        // Every entry sits directly under Base: each Dir= value must remain
        // a directory (ending in '/'), the grouped view must win over the
        // prefix view, and the view restores byte for byte.
        let paths: Vec<_> = (0..30)
            .map(|i| format!("/workspace/project/file_{i:03}.rs"))
            .collect();
        for ending in ["\n", "\r\n"] {
            for trailing in [true, false] {
                let input = format!(
                    "{}{}",
                    paths.join(ending),
                    if trailing { ending } else { "" }
                );
                let candidate = compress(&input).unwrap();
                assert_eq!(restore(&candidate), input);
                assert!(
                    candidate.starts_with(GROUPED_HEADER),
                    "grouping must win on flat listings: {candidate}"
                );
                for line in candidate.lines() {
                    if let Some(quoted) = line.strip_prefix("Dir=") {
                        let dir: String = serde_json::from_str(quoted).unwrap();
                        assert!(
                            dir.is_empty() || dir.ends_with('/'),
                            "Dir= must hold a directory, got {dir:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn grouped_view_wins_on_a_single_flat_directory() {
        // Regression for review finding 1: flat listings used to lose
        // arbitration because each entry emitted its own filename Dir= line.
        let paths: Vec<_> = (0..30)
            .map(|i| format!("/workspace/project/shared_directory/file_{i:03}.rs"))
            .collect();
        let input = format!("{}\n", paths.join("\n"));
        let candidate = compress(&input).unwrap();
        assert!(
            candidate.starts_with(GROUPED_HEADER),
            "single-directory lists must group: {candidate}"
        );
        // The single directory IS the base, so no explicit Dir= line at all.
        assert!(!candidate.contains("Dir="), "{candidate}");
        assert_eq!(restore(&candidate), input);
    }

    #[test]
    fn grouped_and_prefix_views_never_exceed_the_input() {
        // Neither view may emit output that is not strictly smaller in bytes
        // and estimated tokens; restore always remains byte-exact.
        let cases: Vec<String> = vec![
            "/workspace/project/one_directory/file.rs\n".repeat(30),
            (0..30)
                .map(|i| format!("/workspace/p{i}/file.rs",))
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        ];
        for input in cases {
            if let Some(candidate) = compress(&input) {
                assert_eq!(restore(&candidate), input);
                assert!(candidate.len() < input.len());
                assert!(estimate_tokens(&candidate) < estimate_tokens(&input));
            }
        }
    }
}

//! Byte-reversible prefix sharing for newline-separated POSIX paths.

use tokenless_protocol::estimate_tokens;

const HEADER: &str =
    "[Path list; all received paths retained; full path = base + each paths entry]\n";

/// Validated path records and the original line-ending contract.
pub(super) struct PathList<'a> {
    paths: Vec<&'a str>,
    line_ending: &'static str,
    trailing_newline: bool,
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
    let first = list.paths.first()?;
    let shared = list.paths.iter().skip(1).fold(first.len(), |end, path| {
        first.as_bytes()[..end]
            .iter()
            .zip(path.as_bytes())
            .take_while(|(left, right)| left == right)
            .count()
    });
    // Search bytes for an ASCII separator: a shared byte prefix can end in
    // the middle of a Unicode character, but a slash is always a boundary.
    let end = first.as_bytes()[..shared]
        .iter()
        .rposition(|b| *b == b'/')?
        + 1;
    let base = &first[..end];
    let paths: Vec<_> = list.paths.iter().map(|path| &path[end..]).collect();
    let encoded = serde_json::to_string(&serde_json::json!({
        "base": base,
        "paths": paths,
        "line_ending": list.line_ending,
        "trailing_newline": list.trailing_newline,
    }))
    .ok()?;
    let output = format!("{HEADER}{encoded}");
    (output.len() < input.len() && estimate_tokens(&output) < estimate_tokens(input))
        .then_some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restore(view: &str) -> String {
        let data: serde_json::Value =
            serde_json::from_str(view.strip_prefix(HEADER).unwrap()).unwrap();
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
                    let candidate = compress(&input).unwrap();
                    assert_eq!(restore(&candidate), input);
                    let data: serde_json::Value =
                        serde_json::from_str(candidate.strip_prefix(HEADER).unwrap()).unwrap();
                    assert_eq!(data["paths"].as_array().unwrap().len(), 60);
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
}

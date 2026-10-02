//! Complete search listings with repeated paths shared across consecutive rows.

use tokenless_protocol::estimate_tokens;

const HEADER: &str = "[Search results grouped by file; all received lines retained]\n";

/// Stateless path sharing for no-context `path:line:text` search results.
#[derive(Debug, Clone, Copy, Default)]
pub struct SearchResultsCompressor;

impl SearchResultsCompressor {
    /// Returns a smaller, byte-reversible view, or no candidate.
    ///
    /// Requires at least three records, each with a path containing `/` or
    /// `.` and no colon or line ending. Unsupported records reject the whole
    /// input. Row suffixes, including line numbers and line endings, are copied
    /// verbatim; only consecutive identical paths share a JSON-quoted header.
    #[must_use]
    pub fn compress(&self, input: &str) -> Option<String> {
        let mut output = String::from(HEADER);
        let mut previous_path = None;
        let mut records = 0;
        for line in input.split_inclusive('\n') {
            let (path, suffix) = line.split_once(':')?;
            if !(path.contains('/') || path.contains('.')) || path.contains(['\r', '\n']) {
                return None;
            }
            let (number, _) = suffix.split_once(':')?;
            if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            if previous_path != Some(path) {
                output.push_str("File=");
                output.push_str(&serde_json::Value::String(path.to_owned()).to_string());
                output.push('\n');
                previous_path = Some(path);
            }
            output.push_str(suffix);
            records += 1;
            if output.len() >= input.len() {
                return None;
            }
        }
        (records >= 3
            && output.len() < input.len()
            && estimate_tokens(&output) < estimate_tokens(input))
        .then_some(output)
    }
}

#[cfg(test)]
mod tests {
    use super::SearchResultsCompressor;

    fn restore(view: &str) -> String {
        let mut lines = view.split_inclusive('\n');
        assert_eq!(
            lines.next(),
            Some("[Search results grouped by file; all received lines retained]\n")
        );
        let mut path = String::new();
        let mut original = String::new();
        for line in lines {
            if let Some(quoted) = line.strip_prefix("File=") {
                path = serde_json::from_str(quoted).unwrap();
            } else {
                assert!(!path.is_empty());
                original.push_str(&path);
                original.push(':');
                original.push_str(line);
            }
        }
        original
    }

    #[test]
    fn preserves_every_byte_and_consecutive_file_order() {
        let paths = [
            "crates/long_directory_name/src/first_file.rs",
            "crates/中文 空格/with_\"quote\\slash.rs",
            "crates/long_directory_name/src/first_file.rs",
        ];
        for ending in ["\n", "\r\n"] {
            for trailing in [true, false] {
                let mut input = String::new();
                for path in paths {
                    for (number, body) in [
                        ("001", "  indented text  "),
                        ("39", "File=\"literal.rs\""),
                        ("12", ""),
                        ("9", "123:content:with:colons"),
                        ("7", "--"),
                        ("2", "Unicode 中文\tvalue"),
                    ] {
                        input.push_str(&format!("{path}:{number}:{body}{ending}"));
                    }
                }
                if !trailing {
                    input.truncate(input.len() - ending.len());
                }
                let candidate = SearchResultsCompressor.compress(&input).unwrap();
                assert_eq!(restore(&candidate), input);
                assert_eq!(candidate.matches("\nFile=").count(), 3);
                assert!(candidate.len() < input.len());
            }
        }
    }

    #[test]
    fn preserves_long_lines_and_mixed_line_endings() {
        let path = "crates/long_directory_name/src/search_records.rs";
        let input = format!(
            "{path}:1:{} decision_code=737\r\n{path}:2:  value  \n{path}:3:last",
            "x".repeat(10_000)
        );
        let candidate = SearchResultsCompressor.compress(&input).unwrap();
        assert!(candidate.contains("decision_code=737\r\n"));
        assert_eq!(restore(&candidate), input);
    }

    #[test]
    fn rejects_the_whole_input_on_an_unsupported_record() {
        let valid = "crates/long_directory_name/file.rs:1:match\n".repeat(8);
        for unsupported in [
            "--\n",
            "crates/long_directory_name/file.rs-2-context\n",
            "1:no path\n",
            "file.rs:no_number:body\n",
            "file.rs::body\n",
            "C:\\source\\file.rs:1:body\n",
            "folder:with:colon/file.rs:1:body\n",
            "{\"path\":\"file.rs\",\"line\":1}\n",
            "\n",
        ] {
            assert!(
                SearchResultsCompressor
                    .compress(&format!("{valid}{unsupported}"))
                    .is_none()
            );
        }
    }

    #[test]
    fn small_or_path_diverse_results_are_not_expanded() {
        for input in [
            String::new(),
            "x.rs:1:a\nx.rs:2:b\nx.rs:3:c\n".to_owned(),
            "crates/very_long_path/file.rs:1:match\n".repeat(2),
            (0..30)
                .map(|i| format!("crates/unique_directory_{i}/file.rs:{i}:match\n"))
                .collect(),
        ] {
            assert!(SearchResultsCompressor.compress(&input).is_none());
        }
    }
}

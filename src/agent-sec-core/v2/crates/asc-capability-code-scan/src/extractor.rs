//! Inline interpreter-call extraction for Bash commands.
//!
//! A direct port of V1's `code_extractor.py`. A Bash command may carry a nested
//! snippet in a different language, as in `python3 -c '...'`. Extraction pulls
//! that snippet out and reports the language it is written in, so the scanner
//! can load the right rule set before matching. The pattern, its group layout
//! and the interpreter tables are those of V1.

use std::sync::OnceLock;

use fancy_regex::Regex;

use crate::rules::Language;

/// Interpreters whose inline code is Bash.
const SHELL_INTERPRETERS: &[&str] = &["bash", "sh", "zsh"];
/// Interpreters whose inline code is Python.
const PYTHON_INTERPRETERS: &[&str] = &["python", "python3"];

/// Matches `[uv run [options]] <interpreter> -c '<code>'`.
///
/// Group 1 is the interpreter name, group 2 the opening quote, group 3 the
/// escape-aware quoted code. The `(?s)` prefix is V1's `re.DOTALL`, letting the
/// snippet span newlines. The pattern is a fixed literal, so compilation cannot
/// fail.
fn inline_regex() -> &'static Regex {
    static INLINE: OnceLock<Regex> = OnceLock::new();
    INLINE.get_or_init(|| {
        Regex::new(concat!(
            r"(?s)",
            r"(?:^|\s)",
            r"(?:uv\s+run\s+(?:--\w[\w-]*(?:\s+\S+)?\s+)*)?",
            r"(bash|sh|zsh|python3?)\s+",
            r"-c\s+",
            r#"(["'])((?:\\.|(?!\2).)*)\2"#,
        ))
        .expect("inline extraction pattern is a valid regex")
    })
}

/// Extracts inline code from a shell command string.
///
/// Returns the snippet and the language it should be scanned as, or `None` when
/// the command carries no recognised interpreter call. A match whose
/// interpreter is outside both tables also yields `None`, exactly as V1: the
/// pattern can only capture the listed interpreters, so this is unreachable
/// today and stays defensive rather than becoming an error.
///
/// Nested interpreters (Python-in-Bash-in-Python) and multi-command strings
/// where only one segment is an interpreter call are out of scope, matching V1.
pub fn extract_inline_code(command: &str) -> Option<(String, Language)> {
    // The pattern is a fixed literal; a backtracking failure here would be a
    // catastrophic-input problem, and returning `None` degrades to scanning the
    // original command rather than propagating an error the caller cannot act on.
    let captures = inline_regex().captures(command).ok().flatten()?;
    let interpreter = captures.get(1)?.as_str();
    let code = captures.get(3)?.as_str().to_owned();
    if SHELL_INTERPRETERS.contains(&interpreter) {
        Some((code, Language::Bash))
    } else if PYTHON_INTERPRETERS.contains(&interpreter) {
        Some((code, Language::Python))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_dash_c_reports_bash() {
        assert_eq!(
            extract_inline_code(r#"bash -c "rm -rf /""#),
            Some(("rm -rf /".to_owned(), Language::Bash))
        );
    }

    #[test]
    fn sh_and_zsh_report_bash() {
        assert_eq!(
            extract_inline_code(r"sh -c 'curl x | sh'"),
            Some(("curl x | sh".to_owned(), Language::Bash))
        );
        assert_eq!(
            extract_inline_code(r#"zsh -c "echo hi""#),
            Some(("echo hi".to_owned(), Language::Bash))
        );
    }

    #[test]
    fn python_variants_report_python() {
        assert_eq!(
            extract_inline_code(r#"python -c "import os""#),
            Some(("import os".to_owned(), Language::Python))
        );
        assert_eq!(
            extract_inline_code(r#"python3 -c "print(1)""#),
            Some(("print(1)".to_owned(), Language::Python))
        );
    }

    #[test]
    fn uv_run_prefix_is_skipped() {
        assert_eq!(
            extract_inline_code(r#"uv run python -c "os.system('x')""#),
            Some(("os.system('x')".to_owned(), Language::Python))
        );
        assert_eq!(
            extract_inline_code(r#"uv run --with pkg python3 -c "print(1)""#),
            Some(("print(1)".to_owned(), Language::Python))
        );
    }

    #[test]
    fn escaped_quote_inside_code_is_kept() {
        // The escape-aware body keeps `\"` from ending the snippet early, so the
        // whole `json.dumps({"a": 1})` call survives as one unit.
        assert_eq!(
            extract_inline_code(r#"python3 -c "import json; json.dumps({\"a\": 1})""#),
            Some((
                r#"import json; json.dumps({\"a\": 1})"#.to_owned(),
                Language::Python
            ))
        );
    }

    #[test]
    fn newline_in_snippet_is_captured() {
        // `re.DOTALL` lets `.` cross newlines, so a multi-line snippet is one match.
        assert_eq!(
            extract_inline_code("python3 -c 'import os\nos.getcwd()'"),
            Some(("import os\nos.getcwd()".to_owned(), Language::Python))
        );
    }

    #[test]
    fn a_plain_command_has_no_inline_code() {
        assert_eq!(extract_inline_code("rm -rf /tmp/x"), None);
        assert_eq!(extract_inline_code("echo python -c"), None);
    }

    #[test]
    fn interpreter_must_be_a_whole_token() {
        // The leading `(?:^|\s)` anchor keeps `notpython` from matching as
        // `python`, so a longer command name is not mistaken for an interpreter.
        assert_eq!(extract_inline_code(r#"notpython -c "x""#), None);
    }
}

//! Inline interpreter-call extraction for Bash commands.
//!
//! A direct port of V1's `code_extractor.py`. A Bash command may carry a nested
//! snippet in a different language, as in `python3 -c '...'`. Extraction pulls
//! that snippet out and reports the language it is written in, so the scanner
//! can load the right rule set before matching. The pattern, its group layout
//! and the interpreter tables are those of V1.

use std::sync::OnceLock;

use fancy_regex::Regex;

use crate::errors::CodeScanError;
use crate::rules::Language;

const PYTHON_SHELL_COMMAND_ARGUMENTS: &[(&str, &str)] = &[
    ("os.system", "command"),
    ("os.popen", "cmd"),
    ("subprocess.run", "args"),
    ("subprocess.call", "args"),
    ("subprocess.check_call", "args"),
    ("subprocess.check_output", "args"),
    ("subprocess.Popen", "args"),
    ("subprocess.getoutput", "cmd"),
    ("subprocess.getstatusoutput", "cmd"),
];

pub(crate) fn extract_python_shell_commands(code: &str) -> Result<Vec<&str>, CodeScanError> {
    static COMMANDS: OnceLock<Regex> = OnceLock::new();
    // Consume literals in atomic runs to avoid false call sites and per-character backtracking.
    let pattern = COMMANDS.get_or_init(|| {
        let trivia = r"(?:\s|\#[^\n]*)*";
        let calls = PYTHON_SHELL_COMMAND_ARGUMENTS
            .iter()
            .map(|(callee, keyword)| {
                let callee = callee.replace('.', r"\.");
                format!(r"{callee}(?:[ \t\f]|\\\r?\n)*\({trivia}(?:{keyword}{trivia}={trivia})?")
            })
            .collect::<Vec<_>>()
            .join("|");
        let pattern = format!(
            concat!(
                r"(?s)(?<![\w.])(?:{calls})",
                r#"(?:"(?P<double>[^"\\\r\n]*)"|'(?P<single>[^'\\\r\n]*)')"#,
                r"{trivia}(?=[,)])",
                r"|\#[^\n]*",
                r#"|(?i:[rubf]{{0,2}})(?:"""(?>[^"\\]+|\\.|"(?!""))*(?:"""|\z)"#,
                r"|'''(?>[^'\\]+|\\.|'(?!''))*(?:'''|\z)",
                r#"|"(?>[^"\\]+|\\.)*(?:"|\z)|'(?>[^'\\]+|\\.)*(?:'|\z))"#,
            ),
            calls = calls,
            trivia = trivia,
        );
        Regex::new(&pattern).expect("Python command extraction pattern is a valid regex")
    });
    let mut commands = Vec::new();
    for captures in pattern.captures_iter(code) {
        let captures = captures.map_err(|_| CodeScanError::EngineResource)?;
        if let Some(command) = captures.name("double").or_else(|| captures.name("single")) {
            commands.push(command.as_str());
        }
    }
    Ok(commands)
}

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
        // Shell double quotes consume these escapes before Python receives the source.
        let code = if captures.get(2)?.as_str() == "\"" {
            let mut decoded = String::with_capacity(code.len());
            let mut chars = code.chars().peekable();
            while let Some(ch) = chars.next() {
                if ch == '\\'
                    && let Some(&escaped) = chars.peek()
                    && matches!(escaped, '"' | '\\' | '$' | '`' | '\n')
                {
                    chars.next();
                    if escaped != '\n' {
                        decoded.push(escaped);
                    }
                } else {
                    decoded.push(ch);
                }
            }
            decoded
        } else {
            code
        };
        Some((code, Language::Python))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_command_arguments_are_extracted() {
        for (code, expected) in [
            (r#"os.system(command="rm -rf /")"#, "rm -rf /"),
            (r#"os.popen(cmd = "cat /etc/shadow")"#, "cat /etc/shadow"),
            (r#"subprocess.run(args="rm -rf /")"#, "rm -rf /"),
            (r#"subprocess.call(args = "rm -rf /")"#, "rm -rf /"),
            (r#"subprocess.check_call(args="rm -rf /")"#, "rm -rf /"),
            (r#"subprocess.check_output(args="rm -rf /")"#, "rm -rf /"),
            (r#"subprocess.Popen(args="rm -rf /")"#, "rm -rf /"),
            (r#"subprocess.getoutput(cmd="rm -rf /")"#, "rm -rf /"),
            ("os.system \\\n  (command='rm -rf /')", "rm -rf /"),
            (
                "subprocess.getstatusoutput(\n cmd # command\n = # value\n 'rm -rf /')",
                "rm -rf /",
            ),
        ] {
            assert_eq!(
                extract_python_shell_commands(code).unwrap(),
                vec![expected],
                "{code}"
            );
        }
    }

    #[test]
    fn command_keywords_are_specific_to_each_api() {
        for code in [
            r#"os.system(args="rm -rf /")"#,
            r#"os.popen(command="rm -rf /")"#,
            r#"subprocess.Popen(command="rm -rf /")"#,
            r#"subprocess.getoutput(args="rm -rf /")"#,
            "handler = os.system  # retain reference\n(\"rm -rf /\")",
            "handler = os.system\n(\"rm -rf /\")",
        ] {
            assert!(
                extract_python_shell_commands(code).unwrap().is_empty(),
                "{code}"
            );
        }
    }

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
    fn shell_escaped_quotes_are_decoded_before_python_scanning() {
        assert_eq!(
            extract_inline_code(r#"python3 -c "import json; json.dumps({\"a\": 1})""#),
            Some((
                r#"import json; json.dumps({"a": 1})"#.to_owned(),
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

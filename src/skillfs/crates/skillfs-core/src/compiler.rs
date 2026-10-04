//! SKILL.md conditional compiler.
//!
//! Transforms generic SKILL.md content into environment-specific output via two strategies:
//!
//! 1. **Precise compilation** (when `<!-- @if ... -->` directives are present):
//!    Evaluates conditional blocks and emits only the content relevant to the
//!    current environment. Directive lines are stripped from output.
//!
//! 2. **Heuristic normalization** (no directives present):
//!    Applies built-in substitution rules (e.g. `pip install` → `uv pip install`
//!    when `uv` is available) to existing SKILL.md files without modification.
//!
//! # Directive Syntax
//!
//! ```markdown
//! <!-- @if has_command("uv") -->
//! Use uv: `uv pip install -r requirements.txt`
//! <!-- @else -->
//! Use pip: `pip install -r requirements.txt`
//! <!-- @endif -->
//!
//! <!-- @if os == darwin -->
//! macOS specific content
//! <!-- @endif -->
//! ```
//!
//! # Supported Expressions
//!
//! | Expression | Description |
//! |---|---|
//! | `os == darwin\|linux\|windows` | OS comparison |
//! | `os != darwin` | Negated OS comparison |
//! | `has_command("tool")` | Command available in PATH |
//! | `has_env("VAR")` | Environment variable is set |
//! | `expr && expr` | Logical AND |
//! | `expr \|\| expr` | Logical OR |

use crate::env::EnvironmentProfile;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compile `content` for the given `env`.
///
/// Returns the environment-adapted content. Never fails; returns original
/// content on any unexpected state.
pub fn compile(content: &str, env: &EnvironmentProfile) -> String {
    if has_conditional_blocks(content) {
        compile_conditional(content, env)
    } else {
        apply_heuristic_normalization(content, env)
    }
}

// ---------------------------------------------------------------------------
// Conditional block compiler
// ---------------------------------------------------------------------------

/// Returns `true` if `content` contains any conditional directive
/// (`<!-- @if ... -->`, `<!-- @else -->`, or `<!-- @endif -->`).
///
/// Any of the three routes the document into structural validation: a
/// document whose only directive is a stray `@else`/`@endif` with no
/// enclosing `@if` must reach the original-content anomaly fallback, not
/// fall through to heuristic normalization, which would rewrite a
/// structurally broken document instead of returning it verbatim.
fn has_conditional_blocks(content: &str) -> bool {
    content.contains("<!-- @if ")
        || content.contains("<!-- @else -->")
        || content.contains("<!-- @endif -->")
}

/// Compile content that contains `<!-- @if -->` / `<!-- @else -->` / `<!-- @endif -->` blocks.
///
/// Algorithm:
/// - Maintain a stack `emit_at_depth: Vec<bool>` starting with `[true]`.
/// - On `@if expr`: push `eval(expr)` when the parent scope is active, else push `false`.
/// - On `@else`: toggle the top entry **only** when all parent entries are `true`.
/// - On `@endif`: pop the top entry.
/// - Emit a line only when all stack entries are `true`.
///
/// If the directive structure does not balance — an `@if` left open at
/// end-of-input, or a stray `@else`/`@endif` with no enclosing `@if` — the
/// input is an unexpected state under `compile`'s contract ("Never fails;
/// returns original content on any unexpected state") and the original
/// content is returned unchanged. Suppressing to end-of-file on an unclosed
/// `@if` would silently delete the remainder; stripping a stray directive
/// would silently rewrite structure the author did not balance.
fn compile_conditional(content: &str, env: &EnvironmentProfile) -> String {
    let mut output = String::with_capacity(content.len());
    // Depth 0 = root level, always emit.
    let mut emit_at_depth: Vec<bool> = vec![true];
    // Set when the directive structure is unbalanced; checked at the end so
    // the fallback decision covers both stray directives seen mid-input and
    // a depth stack that never returned to 1.
    let mut structural_anomaly = false;

    for line in content.split_inclusive('\n') {
        let (body, terminator) = split_line_terminator(line);
        let trimmed = body.trim();

        if let Some(expr) = parse_if_directive(trimmed) {
            // Push: active iff parent scope is active AND condition true.
            let parent_active = emit_at_depth.iter().all(|&e| e);
            let condition = parent_active && evaluate_expr(expr, env);
            emit_at_depth.push(condition);
            continue;
        }

        if is_else_directive(trimmed) {
            if emit_at_depth.len() > 1 {
                // Toggle only when all parent depths are true.
                let len = emit_at_depth.len();
                let parent_active = emit_at_depth[..len - 1].iter().all(|&e| e);
                if parent_active {
                    let last = emit_at_depth.last_mut().unwrap();
                    *last = !*last;
                }
            } else {
                // @else with no enclosing @if.
                structural_anomaly = true;
            }
            continue;
        }

        if is_endif_directive(trimmed) {
            if emit_at_depth.len() > 1 {
                emit_at_depth.pop();
            } else {
                // @endif with no enclosing @if.
                structural_anomaly = true;
            }
            continue;
        }

        // Emit the line when all depth conditions are satisfied, with its own
        // terminator: directive lines are dropped, but the surviving lines
        // keep the file's line endings.
        if emit_at_depth.iter().all(|&e| e) {
            output.push_str(body);
            output.push_str(terminator);
        }
    }

    if emit_at_depth.len() != 1 {
        // An @if was never closed: emit-suppression would otherwise stay on
        // for the entire remainder of the file.
        structural_anomaly = true;
    }

    if structural_anomaly {
        return content.to_string();
    }

    output
}

/// Split one line from `str::split_inclusive('\n')` into its body and the
/// terminator it arrived with. Keeping the terminator per line is what lets
/// both compiler stages leave the file's line endings alone: a CRLF line (or
/// a final line without any terminator) is emitted exactly as written.
fn split_line_terminator(line: &str) -> (&str, &str) {
    match line.strip_suffix('\n') {
        Some(head) => match head.strip_suffix('\r') {
            Some(body) => (body, "\r\n"),
            None => (head, "\n"),
        },
        None => (line, ""),
    }
}

fn parse_if_directive(line: &str) -> Option<&str> {
    // Format: <!-- @if <expr> -->
    let inner = line.strip_prefix("<!-- @if ")?.strip_suffix(" -->")?;
    Some(inner.trim())
}

fn is_else_directive(line: &str) -> bool {
    line == "<!-- @else -->"
}

fn is_endif_directive(line: &str) -> bool {
    line == "<!-- @endif -->"
}

// ---------------------------------------------------------------------------
// Expression evaluator
// ---------------------------------------------------------------------------

/// Evaluate a boolean expression string against `env`.
///
/// Operator precedence: `||` is evaluated before `&&` (left-to-right scan).
/// Parentheses are not supported in Phase 1.
fn evaluate_expr(expr: &str, env: &EnvironmentProfile) -> bool {
    let expr = expr.trim();

    // Try || first (left-most top-level occurrence).
    if let Some(pos) = find_op(expr, "||") {
        return evaluate_expr(&expr[..pos], env) || evaluate_expr(&expr[pos + 2..], env);
    }

    // Then &&
    if let Some(pos) = find_op(expr, "&&") {
        return evaluate_expr(&expr[..pos], env) && evaluate_expr(&expr[pos + 2..], env);
    }

    evaluate_primitive(expr, env)
}

/// Find the position of `op` in `expr`, ignoring occurrences inside parentheses
/// or quoted strings.
fn find_op(expr: &str, op: &str) -> Option<usize> {
    let bytes = expr.as_bytes();
    let op_bytes = op.as_bytes();
    let mut depth: usize = 0;
    let mut in_quote = false;
    let mut quote_char = b'"';
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];
        if in_quote {
            if b == quote_char {
                in_quote = false;
            }
        } else {
            match b {
                b'"' | b'\'' => {
                    in_quote = true;
                    quote_char = b;
                }
                b'(' => depth += 1,
                b')' => depth = depth.saturating_sub(1),
                _ => {}
            }
            if depth == 0
                && i + op_bytes.len() <= bytes.len()
                && &bytes[i..i + op_bytes.len()] == op_bytes
            {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Evaluate a single primitive expression (no boolean operators).
fn evaluate_primitive(expr: &str, env: &EnvironmentProfile) -> bool {
    let expr = expr.trim();

    // has_command("tool")
    if let Some(arg) = strip_func_arg(expr, "has_command") {
        return env.has_command(unquote(arg));
    }

    // has_env("VAR")
    if let Some(arg) = strip_func_arg(expr, "has_env") {
        return env.has_env(unquote(arg));
    }

    // os == value
    if let Some(pos) = expr.find("==") {
        let lhs = expr[..pos].trim();
        let rhs = unquote(expr[pos + 2..].trim());
        if lhs == "os" {
            return env.os.as_str() == rhs;
        }
    }

    // os != value
    if let Some(pos) = expr.find("!=") {
        let lhs = expr[..pos].trim();
        let rhs = unquote(expr[pos + 2..].trim());
        if lhs == "os" {
            return env.os.as_str() != rhs;
        }
    }

    // Unknown expression: safe default is false.
    false
}

/// Extract the argument from `func_name(...)`.
fn strip_func_arg<'a>(expr: &'a str, func_name: &str) -> Option<&'a str> {
    let prefix = format!("{}(", func_name);
    let inner = expr.strip_prefix(prefix.as_str())?.strip_suffix(')')?;
    Some(inner.trim())
}

/// Strip surrounding single or double quotes from a string.
fn unquote(s: &str) -> &str {
    let s = s.trim();
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

// ---------------------------------------------------------------------------
// Heuristic normalization
// ---------------------------------------------------------------------------

/// Apply heuristic command substitution rules to `content` without modifying
/// the overall structure of the file.
///
/// Returns a clone of the original content when no rules apply (idempotent).
/// Each line keeps its own terminator (`\n`, `\r\n`, or none on the final
/// line), so only the text a rule actually rewrites changes.
fn apply_heuristic_normalization(content: &str, env: &EnvironmentProfile) -> String {
    let has_uv = env.has_command("uv");
    let node_pm = detect_best_node_pm(env);

    // Fast path: nothing to do.
    if !has_uv && node_pm.is_empty() {
        return content.to_string();
    }

    let mut output = String::with_capacity(content.len());

    for line in content.split_inclusive('\n') {
        // Each line keeps its own terminator, so a file whose lines are not
        // rewritten comes back byte-identical (CRLF sources included).
        let (body, terminator) = split_line_terminator(line);
        output.push_str(&normalize_line(body, has_uv, &node_pm));
        output.push_str(terminator);
    }

    output
}

/// Shell tokens that may precede the command being invoked without changing
/// what it is.
const TRANSPARENT_PREFIXES: &[&str] = &["sudo", "doas", "env", "nohup", "exec", "command"];

/// Options of transparent prefixes that consume a separate value token
/// (`sudo -u root cmd`), so the value is not mistaken for the command word.
/// The table is per prefix: `command -p`, for example, takes no value while
/// `env -C /tmp` consumes `/tmp`.
fn prefix_option_consumes_value(prefix: &str, option: &str) -> bool {
    matches!(
        (prefix, option),
        (
            "sudo",
            "-u" | "-g" | "-p" | "-C" | "-R" | "-T" | "-U" | "-h"
        ) | ("doas" | "env", "-u" | "-C")
            | ("exec", "-a")
    )
}

/// Options known to take no separate value (`env -i cmd`, `command -p cmd`).
fn prefix_option_is_valueless(prefix: &str, option: &str) -> bool {
    matches!(
        (prefix, option),
        (
            "sudo",
            "-b" | "-e" | "-H" | "-i" | "-k" | "-K" | "-l" | "-n" | "-P" | "-s" | "-v"
        ) | ("doas", "-n" | "-s" | "-L")
            | ("env", "-i" | "-0" | "-v")
            | ("exec", "-c" | "-l")
            | ("command", "-p")
    )
}

/// `NAME=VALUE` environment assignment. Option-like tokens (`--key=value`)
/// are not assignments.
fn is_assignment(token: &str) -> bool {
    match token.split_once('=') {
        Some((name, _)) => !name.is_empty() && !name.starts_with('-'),
        None => false,
    }
}

/// Whether the word starting at byte `pos` is the command being invoked on
/// this line. It must begin at an independent shell token, and everything
/// before it — back to the nearest unquoted command boundary (`&&`, `|`,
/// `;`, `$(`, backtick) — may only be environment assignments (`FOO=1`),
/// transparent execution prefixes (`sudo`, `env`, ...), possibly chained
/// (`sudo env FOO=1`, `env nohup`), and their options/value tokens
/// (`sudo -u root`). An argument or subcommand of another command
/// (`echo sudo virtualenv`, `pip install virtualenv`, `pyenv virtualenv`) is
/// not; a match inside a larger token (`VENV_TOOL=virtualenv`) or an option
/// value with no following token (`sudo -u virtualenv id`) is not either.
///
/// Quote- and escape-aware: separators inside quotes (`LABEL='a; b'`) are
/// not boundaries, and quoted runs stay inside their token. When the prefix
/// ends inside an unterminated quote or escape, or hits an unrecognized
/// option, the position cannot be determined and the caller keeps the
/// original text.
fn is_command_position(line: &str, pos: usize) -> bool {
    let prefix = &line[..pos];
    if let Some(last_char) = prefix.chars().next_back() {
        let boundary =
            last_char.is_whitespace() || matches!(last_char, ';' | '|' | '&' | '(' | '`');
        if !boundary {
            return false;
        }
    }
    let Some(tokens) = last_command_segment_tokens(prefix) else {
        return false; // unterminated quote/escape: position unknowable
    };
    let mut chain: Option<&str> = None; // None: waiting for the command word
    let mut tokens = tokens.into_iter();
    while let Some(token) = tokens.next() {
        match chain {
            None => {
                if is_assignment(token) {
                    continue; // another env assignment before the command
                }
                if TRANSPARENT_PREFIXES.contains(&token) {
                    chain = Some(token);
                    continue;
                }
                return false; // the command word is already present; the match is its argument
            }
            Some(current) => {
                if is_assignment(token) {
                    continue; // `env FOO=1 cmd`
                }
                if token.starts_with('-') {
                    let self_contained_long = token.starts_with("--") && token.contains('=');
                    if prefix_option_consumes_value(current, token) {
                        if tokens.next().is_none() {
                            // No value token left in the prefix: the match
                            // itself is the option's value
                            // (`sudo -u virtualenv id`).
                            return false;
                        }
                    } else if !self_contained_long && !prefix_option_is_valueless(current, token) {
                        // Unrecognized option: it may or may not consume the
                        // match as its value, so the position is unknowable.
                        return false;
                    }
                    continue;
                }
                if TRANSPARENT_PREFIXES.contains(&token) {
                    chain = Some(token); // `sudo env ...`, `env FOO=1 nohup ...`
                    continue;
                }
                return false; // a bare word ends the prefix chain: it is the command
            }
        }
    }
    true
}

/// Tokenize `prefix` into shell-ish words, honoring single quotes, double
/// quotes, backslash escapes, and command boundaries (`;`, `|`, `&`, `(`,
/// backtick). Tokens before the last unquoted boundary are dropped so the
/// walk sees only the final command segment; a quoted run stays inside its
/// token (`FOO='a; b'`). Returns `None` when the prefix ends inside an
/// unterminated quote or escape — the match position cannot be determined
/// and the caller keeps the original text.
fn last_command_segment_tokens(prefix: &str) -> Option<Vec<&str>> {
    let mut tokens: Vec<&str> = Vec::new();
    let mut token_start: Option<usize> = None;
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for (i, ch) in prefix.char_indices() {
        if escaped {
            escaped = false; // the escaped character stays inside its token
            continue;
        }
        match ch {
            '\\' if !in_single => {
                escaped = true;
                token_start.get_or_insert(i);
            }
            '\'' if !in_double => {
                in_single = !in_single;
                token_start.get_or_insert(i);
            }
            '"' if !in_single => {
                in_double = !in_double;
                token_start.get_or_insert(i);
            }
            ';' | '|' | '&' | '(' | '`' if !in_single && !in_double => {
                if let Some(ts) = token_start.take() {
                    tokens.push(&prefix[ts..i]);
                }
                tokens.clear(); // command boundary: earlier tokens are another command
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if let Some(ts) = token_start.take() {
                    tokens.push(&prefix[ts..i]);
                }
            }
            _ => {
                token_start.get_or_insert(i);
            }
        }
    }
    if in_single || in_double || escaped {
        return None;
    }
    if let Some(ts) = token_start.take() {
        tokens.push(&prefix[ts..]);
    }
    Some(tokens)
}

/// Replace every occurrence of `from` in `line` that is the command being
/// invoked, leaving matches inside larger words (`pnpm run` contains `npm
/// run`) and in argument position (`echo npm run`) untouched. Position
/// checks always run against the full (pre-substitution) line with absolute
/// offsets, so a later match on the same line — an argument of an earlier
/// rewritten command — keeps its original context.
fn rewrite_command_invocations(line: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut copied = 0; // bytes of `line` already emitted
    let mut search = 0; // search offset within the original line
    while let Some(rel) = line[search..].find(from) {
        let abs = search + rel;
        if is_command_position(line, abs) {
            out.push_str(&line[copied..abs]);
            out.push_str(to);
            copied = abs + from.len();
        }
        search = abs + from.len();
    }
    out.push_str(&line[copied..]);
    out
}

/// Run `rewrite` over the command text that follows an optional leading
/// `Run: ` documentation label, keeping the label and the original leading
/// whitespace. The label is the only transparent prose form: a bare word in
/// front of the match still means the match is that word's argument.
fn rewrite_after_doc_label(line: &str, rewrite: impl Fn(&str) -> String) -> String {
    let commands = line.trim_start().strip_prefix("Run: ").unwrap_or(line);
    let label_len = line.len() - commands.len();
    let rewritten = rewrite(commands);
    let mut output = String::with_capacity(line.len());
    output.push_str(&line[..label_len]);
    output.push_str(&rewritten);
    output
}

/// Rewrite every occurrence of one of `candidates` that is the command being
/// invoked, leaving matches inside larger words (`pnpm run` contains `npm
/// run`, `python -m venvwrapper` contains `python -m venv`) and in argument
/// position (`echo npm run`) untouched.
///
/// Each candidate is its full source spelling plus the replacement. A
/// candidate matches only when it starts at `needle`, begins at a command
/// position, and is followed by a token boundary (whitespace, a command
/// separator, or the end of the line), so a longer module or subcommand name
/// is never partially rewritten. Position checks always run against the full
/// (pre-substitution) line with absolute offsets, so a later match on the same
/// line — an argument of an earlier rewritten command — keeps its original
/// context.
fn rewrite_invocation_tokens(line: &str, needle: &str, candidates: &[(&str, &str)]) -> String {
    let mut output = String::with_capacity(line.len());
    let mut copied = 0; // bytes of `line` already emitted
    for (pos, _) in line.match_indices(needle) {
        let suffix = &line[pos..];
        let Some((matched, replacement)) = candidates
            .iter()
            .find(|(from, _)| suffix.starts_with(*from))
        else {
            continue;
        };
        let end = pos + matched.len();
        // The matched command must end where its token ends.
        let token_ends = line[end..].chars().next().is_none_or(|ch| {
            ch.is_whitespace() || matches!(ch, ';' | '|' | '&' | ')' | '`' | '<' | '>')
        });
        if token_ends && is_command_position(line, pos) {
            output.push_str(&line[copied..pos]);
            output.push_str(replacement);
            copied = end;
        }
    }
    output.push_str(&line[copied..]);
    output
}

/// Normalize bare pip invocations without changing interpreter module calls.
/// The historical `Run: ` documentation label is transparent only at the
/// beginning of a line; other prose and shell arguments stay untouched.
fn rewrite_pip_install_invocations(line: &str) -> String {
    rewrite_after_doc_label(line, |commands| {
        rewrite_invocation_tokens(
            commands,
            "pip",
            &[
                ("pip3 install", "uv pip install"),
                ("pip install", "uv pip install"),
            ],
        )
    })
}

/// Normalize `python -m venv` / `python3 -m venv` without changing prose or
/// argument positions: `echo python -m venv`, a prose mention, and the module
/// name inside another command's arguments are not invocations. `venv` must be
/// the whole module name, so `python -m venvwrapper` keeps its text. Both
/// spellings are rewritten wherever they are invoked, and the `Run: `
/// documentation label is transparent, as for the pip form.
fn rewrite_python_venv_invocations(line: &str) -> String {
    rewrite_after_doc_label(line, |commands| {
        rewrite_invocation_tokens(
            commands,
            "python",
            &[
                ("python3 -m venv", "uv venv"),
                ("python -m venv", "uv venv"),
            ],
        )
    })
}

/// Apply heuristic substitutions to a single line.
fn normalize_line(line: &str, has_uv: bool, node_pm: &str) -> String {
    let mut result = line.to_string();

    if has_uv {
        // Preserve `python -m pip`: uv availability does not imply that
        // the selected interpreter can import the uv Python module.
        result = rewrite_pip_install_invocations(&result);

        // python -m venv / python3 -m venv → uv venv — only when it is the
        // command being invoked, like the virtualenv form below.
        result = rewrite_python_venv_invocations(&result);

        // virtualenv <name> → uv venv <name> — only when `virtualenv` is the
        // command being invoked: mkvirtualenv, pyenv virtualenv, and
        // `pip install virtualenv` are different words or argument positions
        // and must pass through untouched.
        if result.contains("virtualenv ") {
            result = rewrite_command_invocations(&result, "virtualenv ", "uv venv ");
        }
    }

    // Node package manager normalization. As with `virtualenv` above, only
    // an invocation is rewritten: `pnpm run build` merely contains `npm run `
    // at its second byte, and a substring rewrite turned it into
    // `ppnpm run build` (`pnpm install` → `pyarn install` on a yarn-only
    // host). The previous line-wide `contains(pm_*)` guards are gone with the
    // position check: `npm install && pnpm install` now rewrites only the npm
    // side instead of leaving the whole line alone. The subcommand must also
    // end where the token ends: `npm installable` and `npm testing` are not
    // npm commands and keep their text.
    if !node_pm.is_empty() && node_pm != "npm" {
        let install = format!("{node_pm} install");
        let run = format!("{node_pm} run");
        let test = format!("{node_pm} test");
        result = rewrite_invocation_tokens(
            &result,
            "npm",
            &[
                ("npm install", install.as_str()),
                ("npm run", run.as_str()),
                ("npm test", test.as_str()),
            ],
        );
    }

    result
}

/// Choose the best available Node package manager (pnpm > yarn > npm).
fn detect_best_node_pm(env: &EnvironmentProfile) -> String {
    if env.has_command("pnpm") {
        "pnpm".to_string()
    } else if env.has_command("yarn") {
        "yarn".to_string()
    } else if env.has_command("npm") {
        "npm".to_string()
    } else {
        String::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{EnvironmentProfile, OsKind};
    use std::collections::{HashMap, HashSet};

    fn env_darwin_uv() -> EnvironmentProfile {
        let mut cmds = HashSet::new();
        cmds.insert("uv".to_string());
        cmds.insert("python3".to_string());
        EnvironmentProfile {
            os: OsKind::Darwin,
            available_commands: cmds,
            env_vars: HashMap::new(),
        }
    }

    fn env_linux_no_uv() -> EnvironmentProfile {
        let mut cmds = HashSet::new();
        cmds.insert("python3".to_string());
        cmds.insert("pip".to_string());
        EnvironmentProfile {
            os: OsKind::Linux,
            available_commands: cmds,
            env_vars: HashMap::new(),
        }
    }

    fn env_node_pnpm() -> EnvironmentProfile {
        let mut cmds = HashSet::new();
        cmds.insert("pnpm".to_string());
        cmds.insert("node".to_string());
        EnvironmentProfile {
            os: OsKind::Linux,
            available_commands: cmds,
            env_vars: HashMap::new(),
        }
    }

    fn env_node_yarn() -> EnvironmentProfile {
        let mut cmds = HashSet::new();
        cmds.insert("yarn".to_string());
        cmds.insert("node".to_string());
        EnvironmentProfile {
            os: OsKind::Linux,
            available_commands: cmds,
            env_vars: HashMap::new(),
        }
    }

    // -----------------------------------------------------------------------
    // @if/@else/@endif tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_if_true_emits_if_block() {
        let env = env_darwin_uv();
        let content = "A\n<!-- @if os == darwin -->\nB\n<!-- @endif -->\nC\n";
        let result = compile(content, &env);
        assert!(result.contains('A'), "A should be emitted");
        assert!(result.contains('B'), "B (darwin block) should be emitted");
        assert!(result.contains('C'), "C should be emitted");
        assert!(!result.contains("@if"), "directives should be stripped");
    }

    #[test]
    fn test_if_false_skips_if_block() {
        let env = env_linux_no_uv();
        let content = "A\n<!-- @if os == darwin -->\nB\n<!-- @endif -->\nC\n";
        let result = compile(content, &env);
        assert!(result.contains('A'));
        assert!(!result.contains('B'), "B should be skipped on linux");
        assert!(result.contains('C'));
    }

    #[test]
    fn test_if_else_endif() {
        let env = env_darwin_uv();
        let content =
            "<!-- @if os == darwin -->\ndarwin-line\n<!-- @else -->\nlinux-line\n<!-- @endif -->\n";
        let result = compile(content, &env);
        assert!(
            result.contains("darwin-line"),
            "darwin block should be emitted"
        );
        assert!(
            !result.contains("linux-line"),
            "else block should be skipped"
        );
    }

    #[test]
    fn test_if_false_else_emitted() {
        let env = env_linux_no_uv();
        let content =
            "<!-- @if os == darwin -->\ndarwin-line\n<!-- @else -->\nlinux-line\n<!-- @endif -->\n";
        let result = compile(content, &env);
        assert!(!result.contains("darwin-line"));
        assert!(result.contains("linux-line"));
    }

    #[test]
    fn test_has_command_true() {
        let env = env_darwin_uv();
        let content = "<!-- @if has_command(\"uv\") -->\nuv-line\n<!-- @else -->\npip-line\n<!-- @endif -->\n";
        let result = compile(content, &env);
        assert!(result.contains("uv-line"));
        assert!(!result.contains("pip-line"));
    }

    #[test]
    fn test_has_command_false_uses_else() {
        let env = env_linux_no_uv();
        let content = "<!-- @if has_command(\"uv\") -->\nuv-line\n<!-- @else -->\npip-line\n<!-- @endif -->\n";
        let result = compile(content, &env);
        assert!(!result.contains("uv-line"));
        assert!(result.contains("pip-line"));
    }

    #[test]
    fn test_nested_if_parent_false_skips_child() {
        let env = env_linux_no_uv();
        // Parent @if false → both if and else blocks in child should be skipped
        let content = "<!-- @if os == darwin -->\n<!-- @if has_command(\"uv\") -->\nA\n<!-- @else -->\nB\n<!-- @endif -->\n<!-- @endif -->\nC\n";
        let result = compile(content, &env);
        assert!(!result.contains('A'), "A should be skipped: parent false");
        assert!(!result.contains('B'), "B should be skipped: parent false");
        assert!(result.contains('C'));
    }

    #[test]
    fn test_nested_if_both_true() {
        let env = env_darwin_uv();
        let content = "<!-- @if os == darwin -->\n<!-- @if has_command(\"uv\") -->\nA\n<!-- @endif -->\n<!-- @endif -->\n";
        let result = compile(content, &env);
        assert!(result.contains('A'));
    }

    #[test]
    fn test_unclosed_if_returns_original_content() {
        // An unclosed <!-- @if ... --> leaves emit-suppression on for the
        // whole remainder: a false condition silently drops every line
        // after it ("A\n@if(false)\nB\nC" compiled to just "A"). The
        // documented contract is "Never fails; returns original content
        // on any unexpected state" — an unbalanced directive structure is
        // exactly that.
        let env = env_linux_no_uv();
        let content = "A\n<!-- @if os == darwin -->\nB\nC\n";
        assert_eq!(compile(content, &env), content);
    }

    #[test]
    fn test_unclosed_nested_if_returns_original_content() {
        let env = env_darwin_uv();
        let content = "<!-- @if os == darwin -->\nA\n<!-- @if has_command(\"uv\") -->\nB\n<!-- @endif -->\nC\n";
        // Inner block closed, outer one not: still unbalanced.
        assert_eq!(compile(content, &env), content);
    }

    #[test]
    fn test_stray_endif_returns_original_content() {
        // One @if, two @endif: the extra pop is currently swallowed and
        // the stray directive line silently stripped. Per the same
        // "original content on any unexpected state" contract, an
        // @endif with no enclosing @if falls back instead.
        let env = env_linux_no_uv();
        let content = "A\n<!-- @if os == darwin -->\nB\n<!-- @endif -->\n<!-- @endif -->\nC\n";
        assert_eq!(compile(content, &env), content);
    }

    #[test]
    fn test_stray_only_endif_returns_original_content() {
        // A document whose ONLY conditional directive is a stray @endif
        // (no @if anywhere) must take the original-content fallback too:
        // routing it through heuristic normalization would rewrite a
        // structurally broken document (uv present -> pip install becomes
        // uv pip install) instead of returning it verbatim.
        let env = env_darwin_uv();
        let content = "<!-- @endif -->
Run: pip install requests
";
        assert_eq!(compile(content, &env), content);
    }

    #[test]
    fn test_stray_only_else_returns_original_content() {
        // Same class of stray-only structural anomaly for @else.
        let env = env_darwin_uv();
        let content = "<!-- @else -->
Run: pip install requests
";
        assert_eq!(compile(content, &env), content);
    }

    #[test]
    fn test_stray_else_returns_original_content() {
        // An @else with no enclosing @if is the same class of structural
        // anomaly as a stray @endif.
        let env = env_linux_no_uv();
        let content = "A\n<!-- @if os == darwin -->\nB\n<!-- @endif -->\nC\n<!-- @else -->\nD\n";
        assert_eq!(compile(content, &env), content);
    }

    #[test]
    fn test_no_directives_returns_original() {
        let env = env_linux_no_uv();
        let content = "Hello world\nno directives here\n";
        // env_linux_no_uv has no uv, no pnpm/yarn → nothing to normalize
        let result = compile(content, &env);
        assert_eq!(result, content);
    }

    #[test]
    fn test_directives_stripped_from_output() {
        let env = env_darwin_uv();
        let content = "A\n<!-- @if os == darwin -->\nB\n<!-- @endif -->\n";
        let result = compile(content, &env);
        assert!(!result.contains("<!-- @if"));
        assert!(!result.contains("<!-- @endif -->"));
    }

    // -----------------------------------------------------------------------
    // Heuristic normalization tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_heuristic_pip_to_uv_pip() {
        let env = env_darwin_uv();
        let content = "Run: pip install requests\n";
        let result = compile(content, &env);
        assert!(
            result.contains("uv pip install"),
            "should use uv pip install"
        );
        // The result should NOT contain a bare "pip install" (without the "uv " prefix).
        // We check by splitting on "uv pip install" and ensuring no fragment starts with "pip install".
        assert!(
            !result
                .replace("uv pip install", "__REPLACED__")
                .contains("pip install"),
            "bare pip install should be gone after substitution"
        );
    }

    #[test]
    fn test_heuristic_pip_keeps_modules_and_arguments() {
        let env = env_darwin_uv();
        let unchanged = concat!(
            "python -m pip install requests\n",
            "python3 -m pip install requests\n",
            "/opt/venv/bin/python -m pip install requests\n",
            "sudo python3 -m pip install requests\n",
            "echo pip install requests\n",
            "echo 'pip install requests'\n",
            "echo \"pip3 install requests\"\n",
            "env LABEL='example; pip install requests' python app.py\n",
            "PIP_TOOL=pip install requests\n",
            "my-pip install requests\n",
            "pip installer requests\n",
            "pip installable requests\n",
            "Run: python3 -m pip install requests\n",
            "Run: echo pip install requests\n",
            "echo Run: pip install requests\n",
        );
        assert_eq!(compile(unchanged, &env), unchanged);
    }

    #[test]
    fn test_heuristic_pip_mixed_invocations_are_independent() {
        let env = env_darwin_uv();
        let input = concat!(
            "uv pip install a && pip install b\n",
            "pip3 install a && pip install b && uv pip install c\n",
            "python3 -m pip install a; pip3 install b\n",
            "pip install pip install requests\n",
            "  Run: pip3 install requests\n",
        );
        let expected = concat!(
            "uv pip install a && uv pip install b\n",
            "uv pip install a && uv pip install b && uv pip install c\n",
            "python3 -m pip install a; uv pip install b\n",
            "uv pip install pip install requests\n",
            "  Run: uv pip install requests\n",
        );
        let result = compile(input, &env);
        assert_eq!(result, expected);
        assert_eq!(compile(&result, &env), result);
        assert_eq!(compile(input, &env_linux_no_uv()), input);
    }

    #[test]
    fn test_heuristic_pip_respects_prefix_options_and_quoting() {
        let env = env_darwin_uv();
        let input = concat!(
            "sudo -u root pip install requests\n",
            "env FOO=1 nohup pip3 install requests\n",
            "sudo -u pip install requests\n",
            "env -C pip install requests\n",
            "sudo --unknown pip install requests\n",
            "echo a\\; pip install requests\n",
            "pip install requests; pip3 install pandas\n",
        );
        let expected = concat!(
            "sudo -u root uv pip install requests\n",
            "env FOO=1 nohup uv pip install requests\n",
            "sudo -u pip install requests\n",
            "env -C pip install requests\n",
            "sudo --unknown pip install requests\n",
            "echo a\\; pip install requests\n",
            "uv pip install requests; uv pip install pandas\n",
        );
        assert_eq!(compile(input, &env), expected);
    }

    #[test]
    fn test_heuristic_virtualenv_beside_venv() {
        let env = env_darwin_uv();
        // The venv rewrite above runs first and puts `uv venv` on the line;
        // the line-wide guard then hid every `virtualenv` call beside it.
        let input = concat!(
            "python -m venv .venv && virtualenv proj\n",
            "uv venv x && virtualenv y\n",
            "virtualenv a && virtualenv b\n",
        );
        let expected = concat!(
            "uv venv .venv && uv venv proj\n",
            "uv venv x && uv venv y\n",
            "uv venv a && uv venv b\n",
        );
        let result = compile(input, &env);
        assert_eq!(result, expected);
        assert_eq!(compile(&result, &env), result);
        assert_eq!(compile(input, &env_linux_no_uv()), input);
    }

    #[test]
    fn test_heuristic_virtualenv_arguments_untouched() {
        let env = env_darwin_uv();
        // `virtualenv` in argument or word-interior position stays; the pip
        // rewrite of a `pip install virtualenv` line keeps the argument too.
        let unchanged = concat!(
            "echo virtualenv proj\n",
            "mkvirtualenv proj\n",
            "pyenv virtualenv proj\n",
            "uv venv virtualenv x\n",
        );
        assert_eq!(compile(unchanged, &env), unchanged);
        assert_eq!(
            compile("pip install virtualenv\n", &env),
            "uv pip install virtualenv\n"
        );
    }

    #[test]
    fn test_heuristic_pip3_to_uv_pip() {
        let env = env_darwin_uv();
        let content = "pip3 install -r requirements.txt\n";
        let result = compile(content, &env);
        assert!(result.contains("uv pip install -r requirements.txt"));
    }

    #[test]
    fn test_heuristic_venv_to_uv_venv() {
        let env = env_darwin_uv();
        let content = "python -m venv .venv\n";
        let result = compile(content, &env);
        assert!(result.contains("uv venv .venv"));
    }

    #[test]
    fn test_heuristic_venv_rewrites_invocations() {
        let env = env_darwin_uv();
        let input = concat!(
            "python -m venv .venv\n",
            "python3 -m venv /opt/venv\n",
            "sudo python3 -m venv /opt/venv\n",
            "cd /tmp && python -m venv .venv\n",
            "env FOO=1 python -m venv .venv\n",
            "python3 -m venv a && python -m venv b\n",
            "  Run: python3 -m venv .venv\n",
            "python -m venv; echo done\n",
            "python3 -m venv\n",
        );
        let expected = concat!(
            "uv venv .venv\n",
            "uv venv /opt/venv\n",
            "sudo uv venv /opt/venv\n",
            "cd /tmp && uv venv .venv\n",
            "env FOO=1 uv venv .venv\n",
            "uv venv a && uv venv b\n",
            "  Run: uv venv .venv\n",
            "uv venv; echo done\n",
            "uv venv\n",
        );
        let result = compile(input, &env);
        assert_eq!(result, expected);
        assert_eq!(compile(&result, &env), result);
        assert_eq!(compile(input, &env_linux_no_uv()), input);
    }

    #[test]
    fn test_heuristic_venv_ignores_arguments_and_prose() {
        let env = env_darwin_uv();
        // The venv form is rewritten only where it is the invoked command,
        // the rule the virtualenv and npm forms already follow: an argument,
        // a prose mention, a `Run:` label that is itself the argument of
        // another command, or a longer module name that merely starts with
        // `venv` keeps the original text.
        let unchanged = concat!(
            "echo python -m venv .venv\n",
            "echo \"python3 -m venv .venv\"\n",
            "Use python -m venv to isolate dependencies.\n",
            "Run: echo python -m venv .venv\n",
            "echo Run: python3 -m venv .venv\n",
            "ENV_CMD=python -m venv .venv\n",
            "python -m venvwrapper project\n",
            "python3 -m venv_tools project\n",
        );
        assert_eq!(compile(unchanged, &env), unchanged);
    }

    #[test]
    fn test_heuristic_virtualenv_to_uv_venv() {
        let env = env_darwin_uv();
        let content = "virtualenv myenv\n";
        let result = compile(content, &env);
        assert!(result.contains("uv venv myenv"));
    }

    #[test]
    fn test_heuristic_virtualenv_command_position_boundaries() {
        let env = env_darwin_uv();
        // Boundary cases from the #3525 review: a bare word in front of the
        // prefix chain (`echo sudo`), an assignment whose value contains the
        // match, and a prefix option consuming a separate value token.
        let content = "echo sudo virtualenv myenv\nVENV_TOOL=virtualenv make\nsudo -u root virtualenv myenv\nenv FOO=1 virtualenv myenv\n";
        let compiled = compile(content, &env);
        let lines: Vec<&str> = compiled.lines().collect();
        assert_eq!(lines[0], "echo sudo virtualenv myenv");
        assert_eq!(lines[1], "VENV_TOOL=virtualenv make");
        assert_eq!(
            lines[2], "sudo -u root uv venv myenv",
            "prefix option values are walked past"
        );
        assert_eq!(
            lines[3], "env FOO=1 uv venv myenv",
            "env assignments keep the chain"
        );
    }

    #[test]
    fn test_heuristic_virtualenv_option_values_vs_command_position() {
        let env = env_darwin_uv();
        // #3525 review follow-up: an option value with no following token is
        // the match itself (`sudo -u virtualenv id` — virtualenv is the
        // username), and `command -p` consumes no value, so `python` stays
        // the command word and `virtualenv` its argument.
        let content = "sudo -u virtualenv id\ncommand -p python virtualenv myenv\nsudo env FOO=1 virtualenv .venv\nenv FOO=1 nohup virtualenv .venv\n";
        let compiled = compile(content, &env);
        let lines: Vec<&str> = compiled.lines().collect();
        assert_eq!(lines[0], "sudo -u virtualenv id");
        assert_eq!(lines[1], "command -p python virtualenv myenv");
        assert_eq!(
            lines[2], "sudo env FOO=1 uv venv .venv",
            "chained prefixes rewrite"
        );
        assert_eq!(
            lines[3], "env FOO=1 nohup uv venv .venv",
            "assignment then chained prefix rewrite"
        );
    }

    #[test]
    fn test_heuristic_virtualenv_multiple_matches_keep_argument() {
        let env = env_darwin_uv();
        // #3525 review follow-up: the second `virtualenv` is an argument of
        // the first; position checks must run against the full line.
        let content = "virtualenv virtualenv --python=python3\n";
        assert_eq!(
            compile(content, &env),
            "uv venv virtualenv --python=python3\n"
        );
    }

    #[test]
    fn test_heuristic_virtualenv_quotes_escapes_and_unknown_options() {
        let env = env_darwin_uv();
        // #3525 review follow-up 2: quoted separators and escaped separators
        // are not command boundaries (the match sits inside a quoted value or
        // after an escaped one — keep the original); unrecognized options
        // fail closed (position unknowable — keep the original); known value
        // options consume their value and the real command after them is
        // rewritten.
        let content = concat!(
            "env LABEL='example; virtualenv myenv' python app.py\n",
            "echo a\\; virtualenv x\n",
            "cd /tmp; virtualenv .venv\n",
            "env -C virtualenv python app.py\n",
            "exec -a virtualenv python app.py\n",
            "env -C /tmp virtualenv .venv\n",
            "sudo --preserve-env virtualenv x\n",
        );
        let compiled = compile(content, &env);
        let lines: Vec<&str> = compiled.lines().collect();
        assert_eq!(
            lines[0], "env LABEL='example; virtualenv myenv' python app.py",
            "a semicolon inside quotes is not a command boundary"
        );
        assert_eq!(
            lines[1], "echo a\\; virtualenv x",
            "an escaped separator is not a command boundary"
        );
        assert_eq!(
            lines[2], "cd /tmp; uv venv .venv",
            "a real command separator is honored"
        );
        assert_eq!(
            lines[3], "env -C virtualenv python app.py",
            "-C consumes the match as its value"
        );
        assert_eq!(
            lines[4], "exec -a virtualenv python app.py",
            "-a consumes the match as its argv[0]"
        );
        assert_eq!(
            lines[5], "env -C /tmp uv venv .venv",
            "after a known value option the real command is rewritten"
        );
        assert_eq!(
            lines[6], "sudo --preserve-env virtualenv x",
            "an unrecognized option fails closed"
        );
    }

    #[test]
    fn test_heuristic_virtualenv_only_as_invoked_command() {
        let env = env_darwin_uv();
        // mkvirtualenv / pyenv subcommand / package argument must not be
        // rewritten — substring replacement corrupted all three.
        let content = "mkvirtualenv myenv\npyenv virtualenv myenv\npip install virtualenv\nsudo virtualenv myenv\nvirtualenv myenv\n";
        let compiled = compile(content, &env);
        let lines: Vec<&str> = compiled.lines().collect();
        assert_eq!(lines[0], "mkvirtualenv myenv");
        assert_eq!(lines[1], "pyenv virtualenv myenv");
        assert_eq!(lines[2], "uv pip install virtualenv");
        assert_eq!(
            lines[3], "sudo uv venv myenv",
            "sudo-prefixed command is rewritten in place"
        );
        assert_eq!(lines[4], "uv venv myenv");
    }

    #[test]
    fn test_heuristic_no_double_replace() {
        let env = env_darwin_uv();
        let content = "uv pip install requests\n";
        let result = compile(content, &env);
        // Should not become "uv uv pip install"
        assert_eq!(result, content);
    }

    #[test]
    fn test_heuristic_npm_to_pnpm() {
        let env = env_node_pnpm();
        let content = "npm install\nnpm run build\nnpm test\n";
        let result = compile(content, &env);
        assert!(result.contains("pnpm install"));
        assert!(result.contains("pnpm run build"));
        assert!(result.contains("pnpm test"));
    }

    /// A package-manager name that merely contains `npm` is not the npm
    /// command being invoked. `pnpm run build` starts with `npm run ` at its
    /// second byte; rewriting that substring produced `ppnpm run build`, and
    /// on a yarn-only host `pnpm install` became `pyarn install`. Matches in
    /// argument position (`echo npm run build`) are not invocations either.
    #[test]
    fn test_heuristic_node_pm_rewrites_only_invocations() {
        let pnpm = env_node_pnpm();
        let already_pnpm = "pnpm install\npnpm run build\npnpm test\n";
        assert_eq!(
            compile(already_pnpm, &pnpm),
            already_pnpm,
            "pnpm invocations must pass through untouched"
        );

        let mixed = "npm install && pnpm install\n";
        assert_eq!(
            compile(mixed, &pnpm),
            "pnpm install && pnpm install\n",
            "only the npm invocation on the line is rewritten"
        );

        let argument = "echo npm run build\n";
        assert_eq!(
            compile(argument, &pnpm),
            argument,
            "an argument of another command is not an invocation"
        );

        // `install` / `test` must be whole subcommands: a longer word that
        // merely starts with one is not an npm command.
        let longer_token = "npm installable packages\nnpm testing\n";
        assert_eq!(
            compile(longer_token, &pnpm),
            longer_token,
            "a longer word is not a rewritten subcommand"
        );

        let yarn = env_node_yarn();
        let already_pnpm_on_yarn = "pnpm install\npnpm test\npnpm run build\n";
        assert_eq!(
            compile(already_pnpm_on_yarn, &yarn),
            already_pnpm_on_yarn,
            "a yarn-only host must not mangle pnpm invocations"
        );

        let yarn_rewritten = compile("npm install\nnpm run build\nnpm test\n", &yarn);
        assert!(yarn_rewritten.contains("yarn install"));
        assert!(yarn_rewritten.contains("yarn run build"));
        assert!(yarn_rewritten.contains("yarn test"));
    }

    #[test]
    fn test_heuristic_no_uv_unchanged() {
        let env = env_linux_no_uv();
        let content = "pip install requests\n";
        // No uv available → no substitution
        let result = compile(content, &env);
        assert_eq!(result, content);
    }

    // -----------------------------------------------------------------------
    // Expression evaluator tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_expr_and_short_circuit() {
        let env = env_linux_no_uv();
        // "os == linux && has_command(uv)" → false (uv not present)
        assert!(!evaluate_expr("os == linux && has_command(\"uv\")", &env));
        // "os == linux && has_command(python3)" → true
        assert!(evaluate_expr(
            "os == linux && has_command(\"python3\")",
            &env
        ));
    }

    #[test]
    fn test_expr_or() {
        let env = env_linux_no_uv();
        assert!(evaluate_expr("os == darwin || os == linux", &env));
        assert!(!evaluate_expr("os == darwin || os == windows", &env));
    }

    #[test]
    fn test_expr_os_neq() {
        let env = env_darwin_uv();
        assert!(evaluate_expr("os != linux", &env));
        assert!(!evaluate_expr("os != darwin", &env));
    }

    #[test]
    fn test_unquote_single_char_no_panic() {
        assert_eq!(unquote("\""), "\"");
        assert_eq!(unquote("'"), "'");
        assert_eq!(unquote(""), "");
    }

    #[test]
    fn test_unquote_matched_pair() {
        assert_eq!(unquote("\"x\""), "x");
        assert_eq!(unquote("'x'"), "x");
        assert_eq!(unquote("\"\""), "");
        assert_eq!(unquote("''"), "");
    }

    #[test]
    fn test_unquote_mismatched_quotes() {
        assert_eq!(unquote("\"x'"), "\"x'");
        assert_eq!(unquote("'x\""), "'x\"");
    }

    #[test]
    fn test_compile_malformed_directives_never_panic() {
        let env = env_linux_no_uv();
        let cases = [
            "<!-- @if has_command(\") -->\nA\n<!-- @endif -->",
            "<!-- @if os == \" -->\nA\n<!-- @endif -->",
            "<!-- @if os == ' -->\nA\n<!-- @endif -->",
            "<!-- @if has_command(\")\") -->\nA\n<!-- @endif -->",
            "<!-- @if has_command(\"\") -->\nA\n<!-- @endif -->",
            "<!-- @if has_command('') -->\nA\n<!-- @endif -->",
            "<!-- @if has_command(') -->\nA\n<!-- @endif -->",
            "<!-- @if os == -->\nA\n<!-- @endif -->",
        ];
        for input in cases {
            let _ = compile(input, &env);
        }
    }

    #[test]
    fn test_trailing_newline_preserved() {
        let env = env_darwin_uv();
        let with_newline = "line\n";
        let without_newline = "line";
        let r1 = compile(with_newline, &env);
        let r2 = compile(without_newline, &env);
        assert!(r1.ends_with('\n'));
        assert!(!r2.ends_with('\n'));
    }

    #[test]
    fn test_heuristic_keeps_line_endings_when_no_rule_matches() {
        let env = env_darwin_uv();
        // The heuristic stage substitutes commands; it must not re-flow the
        // file. A CRLF SKILL.md (Windows-authored sources exist) with no
        // applicable rule has to come back byte-identical.
        let content = "---\r\nname: crlf-skill\r\n---\r\n\r\nBody text.\r\n";
        assert_eq!(compile(content, &env), content);
    }

    #[test]
    fn test_heuristic_keeps_line_endings_of_rewritten_lines() {
        let env = env_darwin_uv();
        let content = "Install with:\r\n\r\nRun: pip install requests\r\n\r\nDone.\r\n";
        let expected = "Install with:\r\n\r\nRun: uv pip install requests\r\n\r\nDone.\r\n";
        assert_eq!(compile(content, &env), expected);
    }

    #[test]
    fn test_conditional_keeps_line_endings() {
        let env = env_darwin_uv();
        let content = "---\r\nname: x\r\n---\r\n<!-- @if os == darwin -->\r\nDarwin body.\r\n<!-- @endif -->\r\n";
        let expected = "---\r\nname: x\r\n---\r\nDarwin body.\r\n";
        assert_eq!(compile(content, &env), expected);
    }
}

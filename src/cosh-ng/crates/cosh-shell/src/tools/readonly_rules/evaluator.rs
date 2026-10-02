use super::runtime_config::RuntimeValidator;
use super::runtime_config::{RuntimeGenericSpec, RuntimeReadonlyConfig, RuntimeSubcommandSpec};
use super::specs::{GenericSpec, PathMode, SubcommandSpec, Validator};

pub(super) fn evaluate(validator: &Validator, tokens: &[String]) -> bool {
    match validator {
        Validator::Bare => tokens.len() == 1,
        Validator::Generic(spec) => evaluate_generic(&tokens[1..], spec),
        Validator::Subcommand(spec) => evaluate_subcommand(tokens, spec),
        Validator::VersionCheck(flags) => evaluate_version_check(tokens, flags),
        Validator::Custom(f) => f(tokens),
    }
}

pub(super) fn evaluate_runtime(validator: &RuntimeValidator, tokens: &[String]) -> bool {
    match validator {
        RuntimeValidator::Bare => tokens.len() == 1,
        RuntimeValidator::Generic(spec) => evaluate_runtime_generic(&tokens[1..], spec),
        RuntimeValidator::Subcommand(spec) => evaluate_runtime_subcommand(tokens, spec),
        RuntimeValidator::VersionCheck(flags) => {
            tokens.len() == 2 && flags.iter().any(|flag| flag == &tokens[1])
        }
    }
}

pub(super) fn config_disables_command(
    config: &RuntimeReadonlyConfig,
    command: &str,
    subcommand: Option<&str>,
) -> bool {
    config
        .disabled
        .iter()
        .any(|key| key.matches(command, subcommand))
}

fn evaluate_runtime_generic(args: &[String], spec: &RuntimeGenericSpec) -> bool {
    let mut idx = 0;
    let mut saw_path = false;
    let mut positionals_only = false;

    while idx < args.len() {
        let token = args[idx].as_str();

        if positionals_only {
            if !check_positional_after_separator(token, spec.path_mode) {
                return false;
            }
            saw_path = true;
            idx += 1;
            continue;
        }

        if token == "--" {
            positionals_only = true;
            idx += 1;
            continue;
        }

        if token.starts_with("--") {
            if spec.deny_flags.iter().any(|d| token.starts_with(d)) {
                return false;
            }

            if let Some((flag, bound)) = spec
                .value_flags
                .iter()
                .find(|(f, _)| f == token || token.starts_with(&format!("{f}=")))
            {
                if token.contains('=') {
                    let val = token.split_once('=').unwrap().1;
                    if let Some(max) = bound {
                        if !is_bounded_positive_count(val, *max) {
                            return false;
                        }
                    }
                } else {
                    idx += 1;
                    let Some(val) = args.get(idx) else {
                        return false;
                    };
                    if let Some(max) = bound {
                        if !is_bounded_positive_count(val, *max) {
                            return false;
                        }
                    }
                }
                let _ = flag;
                idx += 1;
                continue;
            }

            if spec.long_flags.iter().any(|flag| flag == token) {
                idx += 1;
                continue;
            }

            return false;
        }

        if token.starts_with('-') && token.len() > 1 {
            if spec.deny_flags.iter().any(|d| token.starts_with(d)) {
                return false;
            }

            if let Some((flag, bound)) = spec
                .value_flags
                .iter()
                .find(|(f, _)| f.len() == 2 && token.starts_with(f))
            {
                if token.len() > flag.len() {
                    let val = &token[flag.len()..];
                    if let Some(max) = bound {
                        if !is_bounded_positive_count(val, *max) {
                            return false;
                        }
                    }
                } else {
                    idx += 1;
                    let Some(val) = args.get(idx) else {
                        return false;
                    };
                    if let Some(max) = bound {
                        if !is_bounded_positive_count(val, *max) {
                            return false;
                        }
                    }
                }
                idx += 1;
                continue;
            }

            if spec.bare_number_max > 0 && token[1..].chars().all(|ch| ch.is_ascii_digit()) {
                if !is_bounded_positive_count(&token[1..], spec.bare_number_max) {
                    return false;
                }
                idx += 1;
                continue;
            }

            let chars = &token[1..];
            if !chars.chars().all(|ch| spec.short_flags.contains(ch)) {
                return false;
            }
            idx += 1;
            continue;
        }

        if !check_positional(token, spec.path_mode) {
            return false;
        }
        saw_path = true;
        idx += 1;
    }

    match spec.path_mode {
        PathMode::Required => saw_path,
        _ => true,
    }
}

fn evaluate_runtime_subcommand(tokens: &[String], spec: &RuntimeSubcommandSpec) -> bool {
    if tokens.len() < 2 {
        return false;
    }

    if tokens
        .iter()
        .skip(1)
        .any(|arg| spec.deny_args.iter().any(|deny| arg.starts_with(deny)))
    {
        return false;
    }

    let subcmd = tokens[1].as_str();
    spec.subcommands
        .iter()
        .find(|(name, _)| name == subcmd)
        .is_some_and(|(_, validator)| {
            let sub_tokens = &tokens[1..];
            match validator {
                RuntimeValidator::Bare => sub_tokens.len() == 1,
                RuntimeValidator::Generic(g) => evaluate_runtime_generic(&sub_tokens[1..], g),
                RuntimeValidator::VersionCheck(flags) => {
                    sub_tokens.len() == 2 && flags.iter().any(|flag| flag == &sub_tokens[1])
                }
                RuntimeValidator::Subcommand(_) => false,
            }
        })
}

fn evaluate_generic(args: &[String], spec: &GenericSpec) -> bool {
    let mut idx = 0;
    let mut saw_path = false;
    let mut positionals_only = false;

    while idx < args.len() {
        let token = args[idx].as_str();

        if positionals_only {
            if !check_positional_after_separator(token, spec.path_mode) {
                return false;
            }
            saw_path = true;
            idx += 1;
            continue;
        }

        if token == "--" {
            positionals_only = true;
            idx += 1;
            continue;
        }

        if token.starts_with("--") {
            if spec.deny_flags.iter().any(|d| token.starts_with(d)) {
                return false;
            }

            if let Some(&(_, bound)) = spec
                .value_flags
                .iter()
                .find(|(f, _)| *f == token || token.starts_with(&format!("{f}=")))
            {
                if token.contains('=') {
                    let val = token.split_once('=').unwrap().1;
                    if let Some(max) = bound {
                        if !is_bounded_positive_count(val, max) {
                            return false;
                        }
                    }
                } else {
                    idx += 1;
                    let Some(val) = args.get(idx) else {
                        return false;
                    };
                    if let Some(max) = bound {
                        if !is_bounded_positive_count(val, max) {
                            return false;
                        }
                    }
                }
                idx += 1;
                continue;
            }

            if spec.long_flags.contains(&token) {
                idx += 1;
                continue;
            }

            return false;
        }

        if token.starts_with('-') && token.len() > 1 {
            if spec.deny_flags.iter().any(|d| token.starts_with(d)) {
                return false;
            }

            if let Some(&(flag, bound)) = spec
                .value_flags
                .iter()
                .find(|(f, _)| f.len() == 2 && token.starts_with(f))
            {
                if token.len() > flag.len() {
                    let val = &token[flag.len()..];
                    if let Some(max) = bound {
                        if !is_bounded_positive_count(val, max) {
                            return false;
                        }
                    }
                } else {
                    idx += 1;
                    let Some(val) = args.get(idx) else {
                        return false;
                    };
                    if let Some(max) = bound {
                        if !is_bounded_positive_count(val, max) {
                            return false;
                        }
                    }
                }
                idx += 1;
                continue;
            }

            if spec.bare_number_max > 0
                && token.len() > 1
                && token[1..].chars().all(|ch| ch.is_ascii_digit())
            {
                if !is_bounded_positive_count(&token[1..], spec.bare_number_max) {
                    return false;
                }
                idx += 1;
                continue;
            }

            let chars = &token[1..];
            if !chars.chars().all(|ch| spec.short_flags.contains(ch)) {
                return false;
            }
            idx += 1;
            continue;
        }

        if !check_positional(token, spec.path_mode) {
            return false;
        }
        saw_path = true;
        idx += 1;
    }

    match spec.path_mode {
        PathMode::Required => saw_path,
        _ => true,
    }
}

fn evaluate_subcommand(tokens: &[String], spec: &SubcommandSpec) -> bool {
    if tokens.len() < 2 {
        return false;
    }

    if tokens
        .iter()
        .skip(1)
        .any(|a| spec.deny_args.iter().any(|d| a.starts_with(d)))
    {
        return false;
    }

    let subcmd = tokens[1].as_str();
    spec.subcommands
        .iter()
        .find(|(name, _)| *name == subcmd)
        .map(|(_, validator)| {
            let sub_tokens = &tokens[1..];
            match validator {
                Validator::Bare => sub_tokens.len() == 1,
                Validator::Generic(g) => evaluate_generic(&sub_tokens[1..], g),
                Validator::Custom(f) => f(tokens),
                Validator::VersionCheck(flags) => evaluate_version_check(sub_tokens, flags),
                Validator::Subcommand(_) => false,
            }
        })
        .unwrap_or(false)
}

fn evaluate_version_check(tokens: &[String], allowed: &[&str]) -> bool {
    tokens.len() == 2 && allowed.contains(&tokens[1].as_str())
}

fn check_positional(token: &str, mode: PathMode) -> bool {
    match mode {
        PathMode::None => false,
        PathMode::Unchecked => true,
        PathMode::Optional | PathMode::Required => is_safe_readonly_path(token),
    }
}

fn check_positional_after_separator(token: &str, mode: PathMode) -> bool {
    match mode {
        PathMode::None => false,
        PathMode::Unchecked => true,
        PathMode::Optional | PathMode::Required => {
            !token.is_empty() && !is_blocked_special_path(token)
        }
    }
}

pub fn is_safe_readonly_path(path: &str) -> bool {
    !path.is_empty() && path != "-" && !path.starts_with('-') && !is_blocked_special_path(path)
}

pub fn is_bounded_positive_count(value: &str, max: u32) -> bool {
    let Ok(count) = value.parse::<u32>() else {
        return false;
    };
    count > 0 && count <= max
}

/// Returns true when the path targets the special kernel filesystems
/// (`/dev`, `/proc`, `/sys`) under any lexical spelling: the raw prefix
/// match runs first so a blocked spelling stays blocked even when its
/// lexical resolution escapes the blocklist (`/proc/../etc` fails
/// closed), then traversal spellings (`/../proc`, `/./proc`, `//proc`)
/// are caught by re-matching the lexically normalized form (issue #2184).
/// Relative spellings (`../proc/version`, `proc/version`) are blocked
/// cwd-independently — see `first_component_targets_special_dir` — and
/// a leading `~`/`~user` that a `..` would pop is unresolvable at this
/// layer (the shell expands it only at execution time), so those
/// spellings fail closed as well.
///
/// `$`-quoting and variable-expansion spellings (`2>$DEV_NULL`,
/// `$'/proc/self/cmdline'`) are intentionally out of scope: this layer
/// passes them through, and both automatic-execution chains upstream
/// intercept them — the broker's `is_shell_meta` hard-denies `$` and
/// quote characters, and the readonly compound executor's rule 5 rejects
/// tokens containing `$` or backticks (issue #2184 investigation).
/// Evaluate-pass / execute-intercept is the division of labor; new
/// callers must not treat this predicate as a complete sandbox check.
pub fn is_blocked_special_path(path: &str) -> bool {
    if has_blocked_special_prefix(path) {
        return true;
    }
    // Unresolvable normalization (a `..` popping a leading `~`) fails
    // closed.
    let Some(normalized) = normalize_path_lexically(path) else {
        return true;
    };
    has_blocked_special_prefix(&normalized) || first_component_targets_special_dir(&normalized)
}

/// Blocks relative spellings whose first surviving component — after
/// skipping any leading `..` chain — names a special directory
/// (`../proc/version`, `proc/version`, `./proc/version`). The readonly
/// executors run with the shell's working directory, so a relative
/// spelling resolves against whatever the cwd happens to be; this layer
/// has no filesystem access and no cwd, so it fails closed and refuses
/// the spelling for every cwd — one of them (`/`, or a child of it when
/// a `..` chain leads) resolves the spelling into the blocklist.
/// Accepted cost: an ordinary directory literally named
/// `proc`/`dev`/`sys` is refused too — the user can still approve the
/// command interactively.
fn first_component_targets_special_dir(normalized: &str) -> bool {
    // After lexical normalization surviving `..` segments can only lead
    // the path (inner ones are popped by the normalizer).
    let mut rest = normalized;
    while let Some(suffix) = rest.strip_prefix("../") {
        rest = suffix;
    }
    if rest == ".." {
        // Pure traversal with no target component.
        return false;
    }
    matches!(rest.split('/').next(), Some("dev" | "proc" | "sys"))
}

fn has_blocked_special_prefix(path: &str) -> bool {
    path == "/dev"
        || path.starts_with("/dev/")
        || path == "/proc"
        || path.starts_with("/proc/")
        || path == "/sys"
        || path.starts_with("/sys/")
}

/// Lexically normalizes `.` / `..` segments and duplicate slashes. Pure
/// string operation — never touches the filesystem.
///
/// Contract:
/// - Absolute inputs keep the leading `/`; `..` above the root is dropped
///   (POSIX: `/..` resolves to `/`).
/// - Relative inputs keep leading `..` segments verbatim: they cannot be
///   resolved without the working directory.
/// - A leading `~`/`~user` segment of a relative path expands to a
///   home directory only when the shell executes the command, so a `..`
///   that would pop it is unresolvable here: the function returns
///   `None` and the caller fails closed. A `~` appearing after another
///   segment is a literal directory name (no shell expansion) and pops
///   normally.
/// - Symlink aliases are intentionally not covered: resolving them would
///   require filesystem access, which this purely lexical pass never
///   performs by contract.
fn normalize_path_lexically(path: &str) -> Option<String> {
    let absolute = path.starts_with('/');
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            // Empty segments come from leading/duplicate slashes.
            "" | "." => {}
            ".." => {
                let last = segments.last().copied();
                match last {
                    Some(seg) if !absolute && segments.len() == 1 && seg.starts_with('~') => {
                        return None;
                    }
                    Some(seg) if seg != ".." => {
                        segments.pop();
                    }
                    _ => {
                        if !absolute {
                            // Relative `..` with nothing to pop stays in the path.
                            segments.push("..");
                        }
                    }
                }
            }
            other => segments.push(other),
        }
    }

    let joined = if absolute {
        format!("/{}", segments.join("/"))
    } else if segments.is_empty() {
        ".".to_string()
    } else {
        segments.join("/")
    };
    Some(joined)
}

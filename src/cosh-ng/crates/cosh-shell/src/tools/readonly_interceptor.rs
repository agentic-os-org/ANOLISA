//! Interceptor wrappers (#3415): programs that must stay in the
//! execution path because they interpose on the payload's output (rtk
//! compresses it). Unlike launchers (`walk_launcher_chain`), which are
//! peeled classification-only and discarded, an interceptor is peeled
//! for assessment but kept for execution — the step spawns the trusted
//! wrapper binary with the declared env injected, so no `env` process
//! ever runs and the wrapper's function is preserved.

use std::path::{Path, PathBuf};

use super::command_risk::{stage_assessment, CommandShape, InteractionRequirement, RiskImpact};
use super::command_risk_build::basename;
use super::command_risk_parser::{is_env_assignment, ParsedCommand, SegmentConnector};
use super::readonly_compound::{
    eligible_readonly_argv, resolve_trusted_executable, ReadonlyCompoundStep,
    TRUSTED_EXECUTABLE_DIRS,
};

/// One reviewed interceptor wrapper. Each entry is an individual
/// security decision: a new wrapper joins the table only after review
/// shows it executes its payload without adding side effects.
struct InterceptorSpec {
    /// Basename the wrapper path token must carry.
    name: &'static str,
    /// Exact assignment keys accepted in front of the wrapper. The
    /// collected pairs are injected into the auto-executed step, so
    /// the set is explicit rather than a prefix: a future
    /// `TOKENLESS_*` variable with behavioral effects must not enter
    /// the approval-free path by accident, and a forged key gains
    /// nothing either. Keys with path semantics (currently only
    /// `TOKENLESS_DATA_DIR`) additionally get their *values* compared
    /// against the session's trusted state before injection.
    /// Session-level config the command does not carry is supplied
    /// separately from the trusted process environment
    /// (`merge_process_session_env`).
    env_keys: &'static [&'static str],
}

const INTERCEPTOR_WRAPPERS: &[InterceptorSpec] = &[
    // tokenless rtk: `anchor_rtk_prefix` emits
    // `env TOKENLESS_* <abs rtk> <payload>` when rtk rewrites with a
    // bare name; when rtk already emits the absolute wrapper path the
    // anchor adds nothing and the wrapped form is just
    // `<abs rtk> <payload>`. Both shapes occur in the wild.
    InterceptorSpec {
        name: "rtk",
        env_keys: &[
            "TOKENLESS_AGENT_ID",
            "TOKENLESS_SESSION_ID",
            "TOKENLESS_TOOL_USE_ID",
            "TOKENLESS_DATA_DIR",
        ],
    },
];

/// Pure peel result: no filesystem probes, so tests can pin the
/// template contract on any machine.
struct InterceptorTokens<'a> {
    spec: &'static InterceptorSpec,
    env: Vec<(String, String)>,
    wrapper: &'a str,
    payload: &'a [String],
}

/// Peels the interceptor wrapper template (`[env KEY=…] <wrapper path>
/// <payload>`, env prefix optional) from command tokens. Pure parsing:
/// every deviation — foreign env keys, an unrecognized wrapper
/// basename — returns `None` before any filesystem probe, so these
/// rejections hold on machines without the wrapper installed.
fn parse_interceptor_tokens(tokens: &[String]) -> Option<InterceptorTokens<'_>> {
    let mut rest = tokens;
    // The env prefix is optional: when rtk emits the absolute wrapper
    // path itself there are no assignments to carry. An `env` program
    // with options lands here with zero assignments and a non-wrapper
    // next token, failing closed at the spec lookup.
    if rest.first().is_some_and(|token| token == "env") {
        rest = &rest[1..];
    }
    let assignment_end = rest
        .iter()
        .position(|token| !is_env_assignment(token))
        .unwrap_or(rest.len());
    let (assignments, rest) = rest.split_at(assignment_end);
    let (wrapper, payload) = rest.split_first()?;
    let spec = INTERCEPTOR_WRAPPERS
        .iter()
        .find(|spec| Path::new(wrapper).file_name().and_then(|n| n.to_str()) == Some(spec.name))?;
    let mut env = Vec::with_capacity(assignments.len());
    for token in assignments {
        let (key, value) = token.split_once('=')?;
        if !spec.env_keys.contains(&key) {
            return None;
        }
        env.push((key.to_string(), value.to_string()));
    }
    Some(InterceptorTokens {
        spec,
        env,
        wrapper,
        payload,
    })
}

/// Peels an interceptor wrapper from a simple command, assessing the
/// payload under the shared readonly rules while keeping the wrapper
/// as the executed program. Every deviation from the emitted
/// templates — foreign env keys, a wrapper outside the trusted
/// directories, a non-readonly payload, a payload whose own stage
/// assessment would demand approval — falls back to the regular
/// (AskUser) paths, so a forged wrapper shape gains nothing.
pub(super) fn build_interceptor_step(parsed: &ParsedCommand) -> Option<ReadonlyCompoundStep> {
    if parsed.shape != CommandShape::Simple
        || parsed.requires_shell_expansion
        || !parsed.null_redirections.is_stderr_only()
        || parsed.stages.len() != 1
    {
        return None;
    }
    let tokens = parse_interceptor_tokens(&parsed.stages[0])?;
    // The outer classifier gates auto-approval on the *wrapper's*
    // stage assessment, whose program is rtk/env — payload-sensitive
    // rules (e.g. the grep-family secret search, which keys on the
    // program identity) cannot fire there. Re-run the stage rules on
    // the payload and mirror the outer gate, so wrapping never
    // weakens the verdict the bare command would get (#3436 review).
    // Pure check, kept before the filesystem probes so the rejection
    // stays machine-independent.
    let Some(payload_program) = tokens.payload.first() else {
        // A bare wrapper with no payload fails closed rather than
        // panicking the classification path (#3436 review).
        return None;
    };
    let payload_stage = stage_assessment(basename(payload_program), tokens.payload);
    if payload_stage.impact == RiskImpact::High
        || payload_stage.interaction != InteractionRequirement::None
    {
        return None;
    }
    if !data_dir_values_are_trusted(&tokens.env) {
        return None;
    }
    let program = resolve_trusted_wrapper(tokens.wrapper)?;
    if !eligible_readonly_argv(tokens.payload, true) {
        return None;
    }
    // The payload runs under the step's trusted-only `PATH`, but
    // resolving it here as well keeps the eligibility verdict equal to
    // the executability verdict (no 127 at run time).
    resolve_trusted_executable(payload_program)?;
    let mut argv = Vec::with_capacity(tokens.payload.len() + 1);
    argv.push(tokens.spec.name.to_string());
    argv.extend(tokens.payload.iter().cloned());
    let mut env = tokens.env;
    merge_process_session_env(&mut env, tokenless_process_env());
    Some(ReadonlyCompoundStep {
        connector: SegmentConnector::Seq,
        program,
        argv,
        suppress_stderr: true,
        env,
    })
}

/// The command text is attacker-controlled input, so its assignments
/// are gated on the exact emitted key set above. The cosh process
/// environment is a different trust domain — the session state the
/// tokenless hook chain itself inherits — so `TOKENLESS_*` entries
/// the command did not carry pass through by prefix: the wrapper sees
/// the same tokenless configuration (stats switches, the selected
/// data directory) it would see after manual approval, without cosh
/// enumerating tokenless's knobs (#3436 review). The validated
/// command text wins on conflicts.
fn merge_process_session_env<I>(env: &mut Vec<(String, String)>, process_env: I)
where
    I: IntoIterator<Item = (String, String)>,
{
    for (key, value) in process_env {
        if key.starts_with("TOKENLESS_") && !env.iter().any(|(existing, _)| *existing == key) {
            env.push((key, value));
        }
    }
}

/// UTF-8 `TOKENLESS_*` entries of the cosh process environment.
fn tokenless_process_env() -> Vec<(String, String)> {
    std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(key, _)| key.starts_with("TOKENLESS_"))
        .collect()
}

/// `TOKENLESS_DATA_DIR` has path semantics: rtk creates the directory
/// and writes its state databases there, so a caller-chosen value
/// would turn a readonly grant into writes at arbitrary paths (#3436
/// review). Every occurrence must match this session's trusted state
/// directory; anything else falls back to AskUser. Compared before
/// any filesystem probe so the rejection is machine-independent.
fn data_dir_values_are_trusted(env: &[(String, String)]) -> bool {
    let mut values = env
        .iter()
        .filter(|(key, _)| key == "TOKENLESS_DATA_DIR")
        .map(|(_, value)| value.as_str());
    let Some(first) = values.next() else {
        return true;
    };
    let Some(trusted) = trusted_data_dir() else {
        return false;
    };
    std::iter::once(first)
        .chain(values)
        .all(|value| is_trusted_data_dir_value(value, &trusted))
}

/// The session's trusted tokenless state directory from the cosh
/// process environment — the same inputs the tokenless hook chain
/// inherits, so a value tokenless emitted always matches here.
fn trusted_data_dir() -> Option<PathBuf> {
    let env_override = std::env::var("TOKENLESS_DATA_DIR").ok();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    resolve_trusted_data_dir(env_override.as_deref(), home.as_deref())
}

/// Mirrors tokenless's data-dir resolution order
/// (`tokenless-stats/path_policy.rs`): an explicit
/// `TOKENLESS_DATA_DIR` environment override wins, otherwise the
/// default is `$HOME/.tokenless`. A path set only in the tokenless
/// config file is invisible to cosh and fails closed.
fn resolve_trusted_data_dir(env_override: Option<&str>, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = env_override.filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(path));
    }
    home.filter(|path| !path.as_os_str().is_empty())
        .map(|home| home.join(".tokenless"))
}

/// Exact-match against the trusted directory, tolerating trailing
/// slashes only — canonicalizing here would probe the filesystem for
/// every candidate command.
fn is_trusted_data_dir_value(value: &str, trusted: &Path) -> bool {
    trim_slashes(value) == trim_slashes(&trusted.to_string_lossy())
}

fn trim_slashes(text: &str) -> &str {
    let trimmed = text.trim_end_matches('/');
    if trimmed.is_empty() {
        "/"
    } else {
        trimmed
    }
}

/// Resolves an interceptor wrapper token to a trusted binary: the path
/// must live directly in a trusted directory, so a user-writable
/// location can never supply the wrapper the eligibility verdict
/// binds to. The parent-directory check runs before the filesystem
/// probe so rejections do not depend on machine state.
fn resolve_trusted_wrapper(token: &str) -> Option<PathBuf> {
    let path = Path::new(token);
    let parent = path.parent()?;
    if !TRUSTED_EXECUTABLE_DIRS
        .iter()
        .any(|dir| parent == Path::new(dir))
    {
        return None;
    }
    path.is_file().then_some(path.to_path_buf())
}

/// Test-only surface for the lib-only regression module
/// (`tools/readonly_interceptor_tests.rs`, declared from `lib.rs`):
/// the implementation stays private to this module while the
/// crate-root tests pin the template contract through these wrappers.
/// Absent from production builds.
#[cfg(test)]
#[allow(dead_code)] // compiled but unused in the bin test target
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};

    /// Owned view of [`super::parse_interceptor_tokens`]: peel result
    /// as `(env assignments, wrapper token, payload tokens)` so the
    /// borrow-based token struct stays private.
    pub(crate) fn peel(words: &[String]) -> Option<(Vec<(String, String)>, String, Vec<String>)> {
        let tokens = super::parse_interceptor_tokens(words)?;
        Some((
            tokens.env,
            tokens.wrapper.to_string(),
            tokens.payload.to_vec(),
        ))
    }

    pub(crate) fn resolve_trusted_data_dir(
        env_override: Option<&str>,
        home: Option<&Path>,
    ) -> Option<PathBuf> {
        super::resolve_trusted_data_dir(env_override, home)
    }

    pub(crate) fn is_trusted_data_dir_value(value: &str, trusted: &Path) -> bool {
        super::is_trusted_data_dir_value(value, trusted)
    }

    pub(crate) fn merge_session_env(
        env: Vec<(String, String)>,
        process_env: Vec<(String, String)>,
    ) -> Vec<(String, String)> {
        let mut env = env;
        super::merge_process_session_env(&mut env, process_env);
        env
    }

    pub(crate) fn process_session_env() -> Vec<(String, String)> {
        super::tokenless_process_env()
    }
}

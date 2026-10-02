//! Interceptor-wrapper regression coverage for issue #3415 / PR #3436.
//!
//! Declared only from `lib.rs` (the `wrap_tests` pattern) so the cases
//! stay out of the lib/bin test overlap ratchet
//! (`scripts/check-test-inventory.sh`): the module is compiled for the
//! `--lib` target only, while `main.rs` does not declare it.
//!
//! Implementation internals are reached through the `test_support`
//! adapters, so no production item widens its visibility for tests.

use std::path::Path;

use crate::tools::readonly_compound::test_support::{seq_step, trusted_executable_dirs};
use crate::tools::readonly_compound::{
    build_readonly_compound_plan, run_readonly_compound, ReadonlyCompoundPlan,
};
use crate::tools::readonly_interceptor::test_support::{
    is_trusted_data_dir_value, merge_session_env, peel, process_session_env,
    resolve_trusted_data_dir,
};
use crate::tools::readonly_pipeline::{ReadonlyPipelineConfig, ReadonlyPipelineOutput};
use crate::tools::{assess_shell_command, AssessmentPolicy, AssessmentSource, ExecutionDecision};

fn tokens(words: &[&str]) -> Vec<String> {
    words.iter().map(ToString::to_string).collect()
}

fn plan(command: &str) -> ReadonlyCompoundPlan {
    build_readonly_compound_plan(command).expect("eligible compound")
}

fn run_plan(plan: &ReadonlyCompoundPlan) -> ReadonlyPipelineOutput {
    run_readonly_compound(plan, &ReadonlyPipelineConfig::default(), Path::new("/"))
        .expect("compound run")
}

// tokenless rtk wraps commands as `env TOKENLESS_* <abs rtk> <payload>`
// or, when rtk emits the absolute path itself, `<abs rtk> <payload>`.
// The peel-for-assessment / keep-for-execution contract lives in
// `readonly_interceptor`.

#[test]
fn interceptor_parse_is_machine_independent() {
    let anchored_words = tokens(&["env", "TOKENLESS_AGENT_ID=a", "/usr/bin/rtk", "ps", "aux"]);
    let (env, wrapper, payload) = peel(&anchored_words).expect("anchored template");
    assert_eq!(
        env,
        vec![("TOKENLESS_AGENT_ID".to_string(), "a".to_string())]
    );
    assert_eq!(wrapper, "/usr/bin/rtk");
    assert_eq!(payload, ["ps", "aux"]);

    let bare_words = tokens(&["/usr/bin/rtk", "ps", "aux"]);
    let (env, _, payload) = peel(&bare_words).expect("bare template");
    assert!(env.is_empty());
    assert_eq!(payload, ["ps", "aux"]);

    // Foreign or unknown keys reject during pure parsing, before any
    // filesystem probe.
    for words in [
        &[
            "env",
            "TOKENLESS_FUTURE_KNOB=1",
            "/usr/bin/rtk",
            "ps",
            "aux",
        ][..],
        &["env", "LD_PRELOAD=/tmp/x.so", "/usr/bin/rtk", "ps", "aux"][..],
    ] {
        assert!(peel(&tokens(words)).is_none());
    }
}

#[test]
fn mid_token_equals_is_not_shell_expansion() {
    // Review regression (#3436): assignments carry unquoted `=`, which
    // the parser once flagged as expansion, silently rejecting the
    // anchored template before the (host-dependent) plan probe ever
    // ran. zsh equals-expansion is word-start-only; `git log
    // --format=%h` exercises the same flag through the stderr-sink
    // path without needing rtk installed.
    assert!(build_readonly_compound_plan("git log --format=%h 2>/dev/null").is_some());
    // Word-start `=` (zsh equals-expansion) keeps the manual path.
    assert!(build_readonly_compound_plan("cat =bash 2>/dev/null").is_none());
}

#[test]
fn interceptor_fails_closed_off_template() {
    for command in [
        // caller-chosen state directory: rtk would create it and write
        // its state databases there, so a readonly grant must never
        // carry a data dir the session would not use itself (#3436)
        "env TOKENLESS_DATA_DIR=/tmp/chosen /usr/bin/rtk ps aux 2>/dev/null",
        // foreign env key: an assignment channel into the
        // approval-free path must stay on an exact allowlist
        "env LD_PRELOAD=/tmp/evil.so TOKENLESS_AGENT_ID=a /usr/bin/rtk ps aux 2>/dev/null",
        // unknown TOKENLESS_* key: the accepted set is exact, so
        // future wrapper variables cannot ride in unreviewed
        "env TOKENLESS_FUTURE_KNOB=1 /usr/bin/rtk ps aux 2>/dev/null",
        // wrapper outside the trusted directories
        "env TOKENLESS_AGENT_ID=a /tmp/rtk ps aux 2>/dev/null",
        // non-readonly payload (on hosts without rtk this fails even
        // earlier, at wrapper resolution)
        "env TOKENLESS_AGENT_ID=a /usr/bin/rtk rm -rf /tmp 2>/dev/null",
        // compound shape around the wrapper stays manual
        "env TOKENLESS_AGENT_ID=a /usr/bin/rtk ps aux 2>/dev/null; echo done",
        // payload-sensitive search: the wrapper's stage assessment
        // cannot see the grep payload, so the payload itself must
        // keep the sensitive-search judgment — otherwise wrapping
        // would downgrade the bare command's High/AskUser to
        // AutoAllow (#3436 review). Rejected before any filesystem
        // probe, so these hold without rtk installed.
        "env TOKENLESS_AGENT_ID=a /usr/bin/rtk grep password settings.txt 2>/dev/null",
        "/usr/bin/rtk grep password settings.txt 2>/dev/null",
        // unquoted glob needs shell expansion the executor never does
        "env TOKENLESS_AGENT_ID=a /usr/bin/rtk find /tmp -name *cosh* 2>/dev/null",
        // stdout suppression is out of the stderr-only contract
        "env TOKENLESS_AGENT_ID=a /usr/bin/rtk ps aux >/dev/null",
    ] {
        assert!(build_readonly_compound_plan(command).is_none(), "{command}");
    }
}

#[test]
fn trusted_data_dir_resolution_mirrors_tokenless() {
    let home = Path::new("/home/user");
    // Environment override wins, mirroring tokenless's resolution order.
    assert_eq!(
        resolve_trusted_data_dir(Some("/var/lib/tokenless"), Some(home)).as_deref(),
        Some(Path::new("/var/lib/tokenless"))
    );
    // Empty override falls through to the per-user default.
    assert_eq!(
        resolve_trusted_data_dir(Some(""), Some(home)).as_deref(),
        Some(Path::new("/home/user/.tokenless"))
    );
    assert_eq!(
        resolve_trusted_data_dir(None, Some(home)).as_deref(),
        Some(Path::new("/home/user/.tokenless"))
    );
    // No inputs at all: nothing trustworthy, callers fail closed.
    assert_eq!(resolve_trusted_data_dir(None, None), None);

    let trusted = Path::new("/root/.tokenless");
    assert!(is_trusted_data_dir_value("/root/.tokenless", trusted));
    assert!(is_trusted_data_dir_value("/root/.tokenless/", trusted));
    assert!(!is_trusted_data_dir_value("/tmp/chosen", trusted));
    assert!(!is_trusted_data_dir_value("/root/.tokenless2", trusted));
}

#[test]
fn interceptor_grants_readonly_payload_when_rtk_installed() {
    // rtk ships with the tokenless package; machines without it cannot
    // exercise the positive path, and the negative tests above pin the
    // fail-closed behavior they do have.
    let Some(wrapper) = trusted_executable_dirs()
        .iter()
        .map(|dir| Path::new(dir).join("rtk"))
        .find(|path| path.is_file())
    else {
        return;
    };
    // The injected data dir must equal the session's trusted tokenless
    // state directory, resolved from the same environment tokenless
    // itself would use.
    let data_dir = std::env::var("TOKENLESS_DATA_DIR")
        .ok()
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".tokenless")));
    if let Some(data_dir) = data_dir {
        let command = format!(
            "env TOKENLESS_AGENT_ID='agent 1' TOKENLESS_DATA_DIR={} {} \
             find /tmp -name '*cosh*' 2>/dev/null",
            data_dir.display(),
            wrapper.display()
        );
        let anchored = plan(&command);
        assert_eq!(anchored.steps.len(), 1);
        let step = &anchored.steps[0];
        assert_eq!(step.program, wrapper);
        assert_eq!(step.argv, vec!["rtk", "find", "/tmp", "-name", "*cosh*"]);
        assert!(step.suppress_stderr);
        // The validated command text comes first; session-level
        // TOKENLESS_* the command did not carry follows from the
        // trusted process environment.
        let mut expected = vec![
            ("TOKENLESS_AGENT_ID".to_string(), "agent 1".to_string()),
            (
                "TOKENLESS_DATA_DIR".to_string(),
                data_dir.to_string_lossy().to_string(),
            ),
        ];
        let session_extra: Vec<(String, String)> = process_session_env()
            .into_iter()
            .filter(|(key, _)| !expected.iter().any(|(existing, _)| existing == key))
            .collect();
        expected.extend(session_extra);
        assert_eq!(step.env, expected);
    }

    // rtk may emit the absolute wrapper path itself; the anchor then
    // adds nothing and no assignments are carried.
    let bare = plan(&format!("{} ps aux 2>/dev/null", wrapper.display()));
    assert_eq!(bare.steps.len(), 1);
    assert_eq!(bare.steps[0].program, wrapper);
    assert_eq!(bare.steps[0].argv, vec!["rtk", "ps", "aux"]);
    // The bare form carries no command-text assignments; the plan env
    // is exactly the trusted session config (#3436 review).
    assert_eq!(bare.steps[0].env, process_session_env());
}

/// Session-config passthrough (#3436 review): command text is the
/// untrusted channel and wins on conflicts; trusted process-level
/// `TOKENLESS_*` the command did not carry (stats switches, the
/// selected data directory) is injected so the wrapper behaves as it
/// would after manual approval. Non-tokenless process variables
/// never pass.
#[test]
fn session_env_passthrough_covers_stats_and_bare_data_dir() {
    let command_env = vec![("TOKENLESS_AGENT_ID".to_string(), "from-command".to_string())];
    let merged = merge_session_env(
        command_env,
        vec![
            ("TOKENLESS_AGENT_ID".to_string(), "from-process".to_string()),
            ("TOKENLESS_STATS_ENABLED".to_string(), "0".to_string()),
            ("TOKENLESS_DATA_DIR".to_string(), "/selected".to_string()),
            ("HOME".to_string(), "/not-tokenless".to_string()),
        ],
    );
    assert_eq!(
        merged,
        vec![
            ("TOKENLESS_AGENT_ID".to_string(), "from-command".to_string()),
            ("TOKENLESS_STATS_ENABLED".to_string(), "0".to_string()),
            ("TOKENLESS_DATA_DIR".to_string(), "/selected".to_string()),
        ]
    );
    // Bare form: nothing carried, the whole session config passes.
    let bare = merge_session_env(
        Vec::new(),
        vec![("TOKENLESS_DATA_DIR".to_string(), "/selected".to_string())],
    );
    assert_eq!(
        bare,
        vec![("TOKENLESS_DATA_DIR".to_string(), "/selected".to_string())]
    );
}

/// Wrapping must never weaken the approval verdict the bare command
/// would get (#3436 review): a payload the unwrapped classifier sends
/// to AskUser keeps AskUser under every emitted wrapper shape. Pure
/// assessment — holds without rtk installed.
#[test]
fn wrapped_payload_keeps_unwrapped_approval_verdict() {
    let policy = AssessmentPolicy::auto_with_readonly_pipeline(AssessmentSource::ProviderShellTool);
    let bare = "grep password settings.txt 2>/dev/null";
    for wrapped in [
        "env TOKENLESS_AGENT_ID=a /usr/bin/rtk grep password settings.txt 2>/dev/null",
        "/usr/bin/rtk grep password settings.txt 2>/dev/null",
    ] {
        assert_eq!(
            assess_shell_command(bare, policy).execution,
            ExecutionDecision::AskUser,
            "bare: {bare}"
        );
        assert_eq!(
            assess_shell_command(wrapped, policy).execution,
            ExecutionDecision::AskUser,
            "wrapped: {wrapped}"
        );
    }
}

/// A wrapper with no payload at all must fail closed, not panic the
/// classification path (#3436 review): both emitted shapes stay on
/// AskUser, and no rtk installation is needed for the rejection.
#[test]
fn empty_payload_fails_closed_without_panicking() {
    let policy = AssessmentPolicy::auto_with_readonly_pipeline(AssessmentSource::ProviderShellTool);
    for command in [
        "/usr/bin/rtk 2>/dev/null",
        "env TOKENLESS_AGENT_ID=a /usr/bin/rtk 2>/dev/null",
    ] {
        assert!(build_readonly_compound_plan(command).is_none(), "{command}");
        assert_eq!(
            assess_shell_command(command, policy).execution,
            ExecutionDecision::AskUser,
            "{command}"
        );
    }
}

#[test]
fn executor_injects_wrapper_env_assignments() {
    // The step env lands after `env_clear`, so a wrapper sees exactly
    // its declared assignments plus the passthrough allowlist.
    let sh = trusted_executable_dirs()
        .iter()
        .map(|dir| Path::new(dir).join("sh"))
        .find(|path| path.is_file())
        .expect("trusted sh");
    let plan = ReadonlyCompoundPlan {
        steps: vec![seq_step(
            sh,
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf '%s' \"$TOKENLESS_AGENT_ID\"".to_string(),
            ],
            vec![("TOKENLESS_AGENT_ID".to_string(), "agent-1".to_string())],
        )],
    };
    let output = run_plan(&plan);
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.stdout, "agent-1");
}

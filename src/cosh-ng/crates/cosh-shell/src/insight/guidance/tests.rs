use super::*;
use crate::config::guide::GuideStore;

#[test]
fn scope_represents_an_observed_fragment_without_claiming_the_original_input() {
    let scope: ExecutionScope = serde_json::from_str("\"observed_command_fragment\"").unwrap();
    assert_eq!(
        serde_json::to_string(&scope).unwrap(),
        "\"observed_command_fragment\""
    );
    assert!(serde_json::from_str::<ExecutionScope>("\"top_level_simple\"").is_err());
}

fn event(expression: &str, status: u16) -> BashEvent {
    BashEvent {
        expression: expression.to_string(),
        status,
        cwd: "/project".to_string(),
        observed_at_ms: 1000,
        scope: ExecutionScope::ObservedCommandFragment,
        identity: CommandIdentity::ExternalUnshadowed,
    }
}

fn state(enabled: bool) -> (tempfile::TempDir, GuideSnapshot) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let store = GuideStore::open(root.join("guide")).unwrap();
    if !enabled {
        store.set_guide(None, false).unwrap();
    }
    let state = store.snapshot(None).unwrap();
    (dir, state)
}

#[test]
fn only_exact_failure_shapes_produce_suggest_only() {
    let (_dir, state) = state(true);
    for (command, status, kind) in [
        ("cargo build", 101, FailureKind::CargoBuild),
        ("cargo test", 101, FailureKind::CargoTest),
        ("pytest", 1, FailureKind::Pytest),
    ] {
        assert_eq!(
            decide(&event(command, status), &state),
            Decision::SuggestOnly(kind)
        );
    }
}

#[test]
fn generic_failures_and_shell_syntax_remain_silent() {
    let (_dir, state) = state(true);
    for (command, status) in [
        ("ls missing", 2),
        ("gti", 127),
        ("grep absent file", 1),
        ("cargo build", 1),
        ("cargo test", 137),
        ("pytest", 2),
        ("cargo test --no-run", 101),
        ("pytest --collect-only", 1),
        ("make -q", 1),
        ("command cargo build", 101),
        ("env cargo build", 101),
        ("/usr/bin/cargo build", 101),
        ("cargo build;false", 101),
        ("cargo build|cat", 101),
        ("cargo build&&false", 101),
        ("cargo\nbuild", 101),
        ("cargo\tbuild", 101),
        (" cargo build", 101),
        ("cargo build ", 101),
        ("请帮我排查内存压力", 127),
        ("build release", 127),
    ] {
        assert!(
            matches!(decide(&event(command, status), &state), Decision::Skip(_)),
            "{command:?}"
        );
    }
}

#[test]
fn unknown_scope_aliases_and_functions_never_invite() {
    let (_dir, state) = state(true);
    let mut observation = event("cargo build", 101);
    observation.scope = ExecutionScope::Unknown;
    assert_eq!(
        decide(&observation, &state),
        Decision::Skip(SkipReason::UnprovenScope)
    );
    observation.scope = ExecutionScope::ObservedCommandFragment;
    for identity in [
        CommandIdentity::Alias,
        CommandIdentity::Function,
        CommandIdentity::Unknown,
    ] {
        observation.identity = identity;
        assert_eq!(
            decide(&observation, &state),
            Decision::Skip(SkipReason::UnprovenIdentity)
        );
    }
}

#[test]
fn disabled_or_invalid_evidence_never_invites() {
    let (_dir, disabled) = state(false);
    assert_eq!(
        decide(&event("cargo build", 101), &disabled),
        Decision::Skip(SkipReason::Disabled)
    );
    let (_dir, state) = state(true);
    for cwd in ["", "relative", "/project\0oops"] {
        let mut observation = event("cargo build", 101);
        observation.cwd = cwd.to_string();
        assert_eq!(
            decide(&observation, &state),
            Decision::Skip(SkipReason::InvalidEvidence)
        );
    }
}

#[test]
fn literal_arguments_are_preserved_but_expansion_and_sensitive_arguments_are_rejected() {
    let (_dir, state) = state(true);
    for command in [
        "cargo build --release",
        "cargo test --test 'my suite'",
        "pytest 'tests/test a.py' -k \"example\"",
    ] {
        assert!(
            matches!(
                decide(
                    &event(
                        command,
                        if command.starts_with("pytest") {
                            1
                        } else {
                            101
                        }
                    ),
                    &state
                ),
                Decision::SuggestOnly(_)
            ),
            "{command}"
        );
    }
    for command in [
        "cargo test $TARGET",
        "cargo build $(id)",
        "cargo test *.rs",
        "cargo test ~",
        "cargo test --token secret",
        "pytest '--token' secret",
        "pytest --to\"ken\" secret",
        "cargo test --test 'unterminated",
        "cargo test foo;id",
        "cargo test foo\\ bar",
    ] {
        assert!(
            matches!(
                decide(
                    &event(
                        command,
                        if command.starts_with("pytest") {
                            1
                        } else {
                            101
                        }
                    ),
                    &state
                ),
                Decision::Skip(_)
            ),
            "{command}"
        );
    }
}

use super::*;
use std::ffi::{OsStr, OsString};

const ALL_TTY: (bool, bool, bool) = (true, true, true);

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

fn classify(argv0: &str, args: &[&str], tty: (bool, bool, bool)) -> Invocation {
    classify_invocation(OsStr::new(argv0), &os(args), tty.0, tty.1, tty.2)
}

fn exec_args(invocation: Invocation) -> Vec<OsString> {
    match invocation {
        Invocation::ExecShell(plan) => plan.args,
        Invocation::Tui(entry) => panic!("expected ExecShell, got Tui({entry:?})"),
    }
}

#[test]
fn agent_namespace_builds_gateway_plan_without_the_namespace_token() {
    assert_eq!(
        gateway_plan(&os(&["agent", "task", "get", "task-1"])),
        Some(GatewayPlan {
            args: os(&["task", "get", "task-1"]),
        })
    );
    assert_eq!(gateway_plan(&os(&["agentic"])), None);
    assert_eq!(gateway_plan(&[]), None);
}

#[test]
fn cosh_entry_matches_only_the_cosh_basename() {
    for argv0 in ["cosh", "-cosh", "/usr/bin/cosh", "./cosh"] {
        assert!(is_cosh_entry(OsStr::new(argv0)), "argv0 {argv0}");
    }
    for argv0 in ["cosh-shell", "-cosh-shell", "/usr/libexec/cosh-shell", ""] {
        assert!(!is_cosh_entry(OsStr::new(argv0)), "argv0 {argv0}");
    }
}

#[test]
fn raw_subcommand_enters_tui_and_is_marked_for_launch_normalization() {
    for args in [
        vec!["raw"],
        vec!["raw", "cosh-core"],
        vec!["raw", "--shell", "zsh", "cosh-core", "--resume"],
    ] {
        assert_eq!(
            classify("cosh", &args, ALL_TTY),
            Invocation::Tui(TuiEntry {
                login: false,
                launch_args: os(&args[1..]),
            }),
            "args {args:?}"
        );
    }

    assert!(matches!(
        classify("cosh", &["raw", "cosh-core"], (false, true, true)),
        Invocation::Tui(TuiEntry { launch_args, .. }) if launch_args == os(&["cosh-core"])
    ));
}

#[test]
fn raw_non_interactive_escape_hatches_keep_their_legacy_passthrough() {
    for (args, expected) in [
        (
            vec!["raw", "cosh-core", "-c", "echo ok"],
            vec!["-c", "echo ok"],
        ),
        (vec!["raw", "--", "echo", "ok"], vec!["--", "echo", "ok"]),
    ] {
        assert_eq!(
            exec_args(classify("cosh", &args, ALL_TTY)),
            os(&expected),
            "args {args:?}"
        );
    }
}

#[test]
fn owned_flags_gate_on_terminals_and_translate_isolation() {
    for args in [
        vec!["--resume"],
        vec!["--resume", "00000000-0000-4000-8000-000000000000"],
        vec!["--isolated"],
        vec!["--shell", "zsh", "--isolated"],
        vec!["--shell=bash"],
    ] {
        assert!(
            matches!(classify("cosh", &args, ALL_TTY), Invocation::Tui(_)),
            "args {args:?}"
        );
    }

    for args in [
        vec!["--resume"],
        vec!["--resume", "00000000-0000-4000-8000-000000000000"],
    ] {
        let invocation = classify("cosh", &args, (false, true, true));
        assert_eq!(
            exec_args(invocation),
            os(&args),
            "args {args:?} must reach the inner shell verbatim"
        );
    }

    match classify("cosh", &["--isolated"], (false, true, true)) {
        Invocation::ExecShell(plan) => {
            assert!(plan.isolated);
            assert!(plan.args.is_empty());
        }
        other => panic!("expected ExecShell, got {other:?}"),
    }

    match classify(
        "cosh",
        &["--shell", "zsh", "--isolated"],
        (false, true, true),
    ) {
        Invocation::ExecShell(plan) => {
            assert_eq!(plan.shell_override, Some(OsString::from("zsh")));
            assert!(plan.isolated);
            assert!(plan.args.is_empty());
        }
        other => panic!("expected ExecShell, got {other:?}"),
    }
}

#[test]
fn isolated_metadata_is_consumed_only_before_shell_owned_argv() {
    match classify(
        "cosh",
        &["--isolated", "-c", "printf ok", "--isolated"],
        ALL_TTY,
    ) {
        Invocation::ExecShell(plan) => {
            assert!(plan.isolated);
            assert_eq!(plan.args, os(&["-c", "printf ok", "--isolated"]));
        }
        other => panic!("expected ExecShell, got {other:?}"),
    }
}

#[test]
fn normalize_raw_diverts_only_command_string_and_double_dash() {
    assert_eq!(
        normalize_raw_invocation(&os(&["raw", "cosh-core", "-c", "echo ok"])),
        Some(os(&["-c", "echo ok"]))
    );
    assert_eq!(
        normalize_raw_invocation(&os(&["raw", "--shell", "bash", "cosh-core", "-c", "x"])),
        Some(os(&["--shell", "bash", "-c", "x"]))
    );
    // A non-dash token after `-c` stays verbatim (it is `-c` payload,
    // not an adapter).
    assert_eq!(
        normalize_raw_invocation(&os(&["raw", "fake", "-c", "cosh-core"])),
        Some(os(&["-c", "cosh-core"]))
    );
    assert_eq!(
        normalize_raw_invocation(&os(&["raw", "--", "echo", "ok"])),
        Some(os(&["--", "echo", "ok"]))
    );
    // Explicit TUI request: raw-surface flags and bare launches stay on
    // the TUI path.
    assert_eq!(normalize_raw_invocation(&os(&["raw", "--run"])), None);
    assert_eq!(
        normalize_raw_invocation(&os(&["raw", "fake", "--run"])),
        None
    );
    assert_eq!(normalize_raw_invocation(&os(&["raw", "cosh-core"])), None);
    assert_eq!(
        normalize_raw_invocation(&os(&["raw", "fake", "--resume"])),
        None
    );
    assert_eq!(normalize_raw_invocation(&os(&["-c", "echo ok"])), None);
}

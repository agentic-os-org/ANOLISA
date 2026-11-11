use super::*;
use std::ffi::{OsStr, OsString};

const ALL_TTY: (bool, bool, bool) = (true, true, true);

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

fn classify(argv0: &str, args: &[&str], tty: (bool, bool, bool)) -> Invocation {
    classify_invocation(OsStr::new(argv0), &os(args), tty.0, tty.1, tty.2)
}

#[test]
fn private_compatibility_worker_is_an_exact_early_dispatch_leaf() {
    assert!(matches!(
        classify(
            "cosh",
            &["shell-integration", "--__compat-worker-v2"],
            (false, false, false)
        ),
        Invocation::ShellIntegration("--__compat-worker-v2")
    ));
    assert!(matches!(
        classify(
            "cosh",
            &["shell-integration", "--__compat-worker-v2", "extra"],
            ALL_TTY
        ),
        Invocation::UsageError(_)
    ));
}

fn exec_args(invocation: Invocation) -> Vec<OsString> {
    match invocation {
        Invocation::ExecShell(plan) => plan.args,
        Invocation::Tui(entry) => panic!("expected ExecShell, got Tui({entry:?})"),
        Invocation::UsageError(error) => panic!("unexpected usage error: {error}"),
        Invocation::Guide(args) => panic!("unexpected guide command: {args:?}"),
        Invocation::HealthWorker => panic!("unexpected health worker"),
        Invocation::LoginShellPlanWorker => panic!("unexpected login shell plan worker"),
        Invocation::LoginInvitationWorker => panic!("unexpected login invitation worker"),
        Invocation::LoginEditor => panic!("unexpected login editor"),
        Invocation::LoginLifecycle(args) => panic!("unexpected lifecycle: {args:?}"),
        Invocation::Health(args) => panic!("unexpected health command: {args:?}"),
        Invocation::Login(args) => panic!("unexpected login command: {args:?}"),
        Invocation::ShellIntegration(operation) => {
            panic!("unexpected shell integration: {operation}")
        }
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
fn raw_subcommand_carries_structured_tui_options() {
    let Invocation::Tui(entry) = classify(
        "cosh",
        &["raw", "--shell", "zsh", "cosh-core", "--resume"],
        ALL_TTY,
    ) else {
        panic!("expected Tui")
    };
    assert_eq!(entry.adapter_name.as_deref(), Some("cosh-core"));
    assert_eq!(entry.shell_kind, Some(RawShellKind::Zsh));
    assert_eq!(entry.launch_options.resume, Some(ResumeLaunch::Picker));
    assert!(!entry.launch_options.login);
    let Invocation::Tui(entry) = classify("cosh", &["raw", "cosh-core"], (false, true, true))
    else {
        panic!("expected Tui")
    };
    assert_eq!(entry.adapter_name.as_deref(), Some("cosh-core"));
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
fn raw_legacy_escape_hatches_preserve_metadata_and_shell_ownership() {
    for (args, shell_override, forwarded, isolated) in [
        (
            vec!["raw", "--shell", "bash", "cosh-core", "-c", "x"],
            Some("bash"),
            vec!["-c", "x"],
            false,
        ),
        (
            vec!["raw", "--shell", "bash", "--shell", "-l", "fake", "-c", "x"],
            Some("bash"),
            vec!["--shell", "-l", "-c", "x"],
            false,
        ),
        (
            vec!["raw", "--shell", "--isolated", "fake", "-c", "x"],
            None,
            vec!["--shell", "--isolated", "-c", "x"],
            false,
        ),
        (
            vec![
                "raw",
                "--isolated",
                "--shell",
                "bash",
                "fake",
                "-c",
                "x",
                "--isolated",
            ],
            Some("bash"),
            vec!["-c", "x", "--isolated"],
            true,
        ),
    ] {
        let Invocation::ExecShell(plan) = classify("-cosh", &args, ALL_TTY) else {
            panic!("expected ExecShell for {args:?}")
        };
        assert_eq!(
            plan.shell_override.as_deref(),
            shell_override.map(OsStr::new)
        );
        assert_eq!(plan.args, os(&forwarded));
        assert_eq!(plan.arg0, OsString::from("-cosh"));
        assert_eq!(plan.isolated, isolated);
    }
    for args in [
        vec!["raw", "--run", "-c", "x"],
        vec!["raw", "fake", "--resume", "-c", "x"],
        vec!["raw", "fake", "--isolated", "--", "echo"],
    ] {
        assert!(
            matches!(classify("cosh", &args, ALL_TTY), Invocation::Tui(_)),
            "{args:?}"
        );
    }
}

#[test]
fn raw_classification_does_not_reinterpret_consumed_shell_values() {
    for argv0 in ["cosh", "cosh-shell"] {
        for value in ["-l", "--login", "--isolated"] {
            let invocation = classify(
                argv0,
                &["raw", "fake", "--shell", "bash", "--shell", value],
                ALL_TTY,
            );
            match invocation {
                Invocation::Tui(entry) => {
                    assert!(!entry.launch_options.login, "argv0 {argv0}, value {value}");
                    assert!(!entry.launch_options.isolated);
                    assert_eq!(entry.shell_kind, Some(RawShellKind::Bash));
                    assert_eq!(entry.adapter_name.as_deref(), Some("fake"));
                }
                other => panic!("expected Tui, got {other:?}"),
            }
        }
    }
}

#[test]
fn raw_command_tail_preserves_non_utf8_bytes_and_launch_like_tokens() {
    use std::os::unix::ffi::OsStringExt;
    let tail = vec![
        OsString::from("-c"),
        OsString::from_vec(b"printf '\xff'".to_vec()),
        OsString::from("--login"),
        OsString::from("--isolated"),
    ];
    let mut args = os(&["raw", "fake"]);
    args.extend(tail.clone());
    for argv0 in ["cosh", "cosh-shell"] {
        let Invocation::ExecShell(plan) =
            classify_invocation(OsStr::new(argv0), &args, true, true, true)
        else {
            panic!("expected ExecShell")
        };
        assert_eq!(plan.args, tail);
        assert!(!plan.isolated);
    }
    let mut args = os(&["raw"]);
    args.push(OsString::from_vec(b"f\xffake".to_vec()));
    args.extend(tail);
    assert!(matches!(
        classify_invocation(OsStr::new("cosh"), &args, true, true, true),
        Invocation::Tui(_)
    ));
}

#[test]
fn ask_consumes_one_value_before_interpreting_launch_flags() {
    for question in ["--login", "--isolated", "--"] {
        assert!(
            matches!(
                classify("cosh", &["--ask", question], ALL_TTY),
                Invocation::Tui(_)
            ),
            "question={question}"
        );
    }
}

#[test]
fn ask_rejects_cosh_owned_conflicts_and_non_interactive_launches() {
    for args in [
        vec!["--ask"],
        vec!["--ask", "  "],
        vec!["--ask", "question", "--resume"],
        vec!["--resume", "session", "--ask", "question"],
        vec!["--ask", "question", "-c", "echo test"],
        vec!["--ask", "question", "script.sh"],
        vec!["--ask", "question", "--", "script.sh"],
        vec!["--ask", "question", "--ask", "another"],
        vec!["raw", "fake", "script.sh", "--ask", "question"],
    ] {
        assert!(
            matches!(classify("cosh", &args, ALL_TTY), Invocation::UsageError(_)),
            "{args:?}"
        );
    }
    for args in [
        vec!["--ask", "question"],
        vec!["raw", "fake", "--ask", "question"],
    ] {
        for tty in [
            (false, true, true),
            (true, false, true),
            (true, true, false),
        ] {
            assert!(
                matches!(classify("cosh", &args, tty), Invocation::UsageError(_)),
                "{args:?}, {tty:?}"
            );
        }
    }
}

#[test]
fn ask_does_not_recapture_shell_owned_tail_and_preserves_question_text() {
    for args in [
        vec!["-c", "printf ok", "--ask", "--login"],
        vec!["script.sh", "--ask", "--isolated"],
        vec!["--", "script.sh", "--ask", "question"],
    ] {
        assert_eq!(exec_args(classify("cosh", &args, ALL_TTY)), os(&args));
    }
    for args in [
        vec!["--ask", "  original question  "],
        vec!["raw", "fake", "--ask", "  original question  "],
    ] {
        let Invocation::Tui(entry) = classify("-cosh", &args, ALL_TTY) else {
            panic!("expected Tui")
        };
        assert_eq!(
            entry.launch_options.initial_question.as_deref(),
            Some("  original question  ")
        );
        assert!(entry.launch_options.login);
        assert!(!entry.launch_options.isolated);
    }
    for question in ["--login", "--isolated", "--"] {
        let Invocation::Tui(entry) = classify("cosh", &["--ask", question], ALL_TTY) else {
            panic!("expected Tui")
        };
        assert_eq!(
            entry.launch_options.initial_question.as_deref(),
            Some(question)
        );
        assert!(!entry.launch_options.login);
        assert!(!entry.launch_options.isolated);
    }
}

#[test]
fn guide_namespace_is_dispatched_before_non_interactive_shell_startup() {
    for argv0 in ["cosh", "cosh-shell"] {
        assert_eq!(
            classify(argv0, &["guide", "off"], (false, false, false)),
            Invocation::Guide(os(&["off"]))
        );
    }
}

#[test]
fn guide_is_reserved_only_at_the_owned_first_command_position() {
    for args in [
        vec!["-c", "printf ok", "guide", "off"],
        vec!["script.sh", "guide", "status"],
        vec!["--", "guide", "on"],
    ] {
        assert_eq!(exec_args(classify("cosh", &args, ALL_TTY)), os(&args));
    }
    let Invocation::Tui(entry) = classify("cosh", &["raw", "guide"], (false, false, false)) else {
        panic!("raw adapter must remain Tui")
    };
    assert_eq!(entry.adapter_name.as_deref(), Some("guide"));
    assert_eq!(
        classify("cosh", &["guide", "invalid"], ALL_TTY),
        Invocation::Guide(os(&["invalid"]))
    );
}

#[test]
fn private_health_worker_rejects_extra_arguments_before_shell_dispatch() {
    let flag = crate::diagnostics::health::runtime::PRIVATE_HEALTH_WORKER_FLAG;
    for argv0 in ["cosh", "cosh-shell"] {
        assert!(matches!(
            classify(argv0, &[flag, "unexpected"], (false, false, false)),
            Invocation::UsageError(_)
        ));
    }
}

#[test]
fn private_health_worker_is_reserved_only_as_the_exact_first_token() {
    let flag = crate::diagnostics::health::runtime::PRIVATE_HEALTH_WORKER_FLAG;
    for argv0 in ["cosh", "cosh-shell"] {
        assert_eq!(
            classify(argv0, &[flag], (false, false, false)),
            Invocation::HealthWorker
        );
        for args in [
            vec!["-c", "printf ok", flag],
            vec!["script.sh", flag],
            vec!["--", flag],
            vec!["--isolated", flag],
        ] {
            assert!(
                matches!(classify(argv0, &args, ALL_TTY), Invocation::ExecShell(_)),
                "{args:?}"
            );
        }
        assert!(!matches!(
            classify(argv0, &["raw", flag], ALL_TTY),
            Invocation::HealthWorker
        ));
        let Invocation::Tui(entry) = classify(argv0, &["--ask", flag], ALL_TTY) else {
            panic!("expected initial question");
        };
        assert_eq!(entry.launch_options.initial_question.as_deref(), Some(flag));
    }
}

#[test]
fn login_namespace_is_owned_before_non_interactive_shell_startup() {
    for argv0 in ["cosh", "cosh-shell"] {
        assert!(!matches!(
            classify(argv0, &["login", "status"], (false, false, false)),
            Invocation::ExecShell(_)
        ));
    }
}

#[test]
fn login_namespace_never_recaptures_raw_or_shell_owned_arguments() {
    for argv0 in ["cosh", "cosh-shell"] {
        assert_eq!(
            classify(
                argv0,
                &["login", "set", "--user", "alice", "--yes"],
                ALL_TTY
            ),
            Invocation::Login(os(&["set", "--user", "alice", "--yes"]))
        );
        for args in [
            vec!["-c", "echo login", "login", "set"],
            vec!["script.sh", "login", "restore"],
            vec!["--", "login", "status"],
            vec!["raw", "fake", "-c", "echo login", "login", "set"],
        ] {
            assert!(matches!(
                classify(argv0, &args, ALL_TTY),
                Invocation::ExecShell(_)
            ));
        }
        let Invocation::Tui(entry) = classify(argv0, &["raw", "login"], ALL_TTY) else {
            panic!("raw adapter boundary changed");
        };
        assert_eq!(entry.adapter_name.as_deref(), Some("login"));
        let Invocation::Tui(entry) = classify(argv0, &["--ask", "login"], ALL_TTY) else {
            panic!("initial question boundary changed");
        };
        assert_eq!(
            entry.launch_options.initial_question.as_deref(),
            Some("login")
        );
    }
}

#[test]
fn shell_integration_rejects_unknown_or_extra_leaf_before_shell_dispatch() {
    for args in [
        vec!["shell-integration"],
        vec!["shell-integration", "unknown"],
        vec!["shell-integration", "compat", "extra"],
        vec!["shell-integration", "advise", "--login"],
    ] {
        for argv0 in ["cosh", "cosh-shell"] {
            assert!(
                matches!(
                    classify(argv0, &args, (false, false, false)),
                    Invocation::UsageError(_)
                ),
                "{args:?}"
            );
        }
    }
}

#[test]
fn shell_integration_is_owned_only_at_the_first_token_and_keeps_shell_tails() {
    for argv0 in ["cosh", "cosh-shell"] {
        for operation in ["compat", "advise"] {
            assert_eq!(
                classify(
                    argv0,
                    &["shell-integration", operation],
                    (false, false, false)
                ),
                Invocation::ShellIntegration(operation)
            );
        }
        for args in [
            vec!["-c", "echo test", "shell-integration", "compat"],
            vec!["script.sh", "shell-integration", "advise"],
            vec!["--", "shell-integration", "compat"],
            vec![
                "raw",
                "fake",
                "-c",
                "echo test",
                "shell-integration",
                "advise",
            ],
        ] {
            assert!(
                matches!(classify(argv0, &args, ALL_TTY), Invocation::ExecShell(_)),
                "{args:?}"
            );
        }
        let Invocation::Tui(entry) = classify(argv0, &["raw", "shell-integration"], ALL_TTY) else {
            panic!("raw adapter boundary changed");
        };
        assert_eq!(entry.adapter_name.as_deref(), Some("shell-integration"));
        let Invocation::Tui(entry) = classify(argv0, &["--ask", "shell-integration"], ALL_TTY)
        else {
            panic!("initial question changed");
        };
        assert_eq!(
            entry.launch_options.initial_question.as_deref(),
            Some("shell-integration")
        );
    }
}

#[test]
fn health_namespace_is_early_owned_and_never_steals_shell_or_question_tails() {
    for argv0 in ["cosh", "cosh-shell"] {
        assert_eq!(
            classify(
                argv0,
                &["health", "banner", "status"],
                (false, false, false)
            ),
            Invocation::Health(vec!["banner".into(), "status".into()])
        );
        for args in [
            vec!["-c", "echo test", "health", "banner"],
            vec!["script.sh", "health", "banner"],
            vec!["--", "health", "banner"],
        ] {
            assert!(matches!(
                classify(argv0, &args, ALL_TTY),
                Invocation::ExecShell(_)
            ));
        }
        let Invocation::Tui(entry) = classify(argv0, &["raw", "health"], ALL_TTY) else {
            panic!("raw adapter boundary changed");
        };
        assert_eq!(entry.adapter_name.as_deref(), Some("health"));
        let Invocation::Tui(entry) = classify(argv0, &["--ask", "health"], ALL_TTY) else {
            panic!("ask value changed");
        };
        assert_eq!(
            entry.launch_options.initial_question.as_deref(),
            Some("health")
        );
    }
}

#[path = "invocation/private_tests.rs"]
mod private_tests;

#[test]
fn failure_context_option_requires_explicit_interactive_question() {
    let id = "1234567890abcdef1234567890abcdef";
    for args in [
        vec!["raw", "fake", "--ask", "same question", "--context", id],
        vec!["--ask", "分析这次失败", "--context", id],
        vec!["--context", id, "--ask", "same question"],
    ] {
        let Invocation::Tui(entry) = classify("cosh", &args, ALL_TTY) else {
            panic!("context should enter Tui")
        };
        assert_eq!(entry.launch_options.failure_context.as_deref(), Some(id));
    }
    for args in [
        vec!["--context", id],
        vec!["--context", id, "-c", "touch should-not-execute"],
        vec!["--ask", "q", "--context", "../file"],
        vec!["--ask", "q", "--context", id, "--context", id],
    ] {
        assert!(matches!(
            classify("cosh", &args, ALL_TTY),
            Invocation::UsageError(_)
        ));
    }
    assert!(matches!(
        classify(
            "cosh",
            &["--ask", "q", "--context", id],
            (false, false, false)
        ),
        Invocation::UsageError(_)
    ));
}

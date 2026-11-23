use super::*;

#[test]
fn private_login_invitation_worker_is_an_exact_owned_first_token() {
    let flag = crate::runtime::login::invitation::WORKER_FLAG;
    for argv0 in ["cosh", "cosh-shell"] {
        assert_eq!(
            classify(argv0, &[flag], (false, false, false)),
            Invocation::LoginInvitationWorker
        );
        assert!(matches!(
            classify(argv0, &[flag, "extra"], ALL_TTY),
            Invocation::UsageError(_)
        ));
        for args in [
            vec!["-c", "echo ok", flag],
            vec!["--", flag],
            vec!["script.sh", flag],
        ] {
            assert!(matches!(
                classify(argv0, &args, ALL_TTY),
                Invocation::ExecShell(_)
            ));
        }
        assert_eq!(
            classify(argv0, &["raw", flag], ALL_TTY),
            classify(argv0, &["raw"], ALL_TTY)
        );
        let Invocation::Tui(entry) = classify(argv0, &["--ask", flag], ALL_TTY) else {
            panic!("ask value stolen");
        };
        assert_eq!(entry.launch_options.initial_question.as_deref(), Some(flag));
    }
}

#[test]
fn native_login_editor_is_reserved_only_with_the_exact_owned_tool_arguments() {
    let flag = crate::runtime::login::editor::EDITOR_FLAG;
    for argv0 in ["cosh", "cosh-shell"] {
        assert_eq!(
            classify(
                argv0,
                &[flag, "3", "/etc/passwd.edit"],
                (false, false, false)
            ),
            Invocation::LoginEditor
        );
        for args in [
            vec![flag],
            vec![flag, "3", "/etc/passwd"],
            vec![flag, "4", "/etc/passwd.edit"],
            vec![flag, "3", "/etc/passwd.edit", "extra"],
        ] {
            assert!(matches!(
                classify(argv0, &args, ALL_TTY),
                Invocation::UsageError(_)
            ));
        }
        assert!(matches!(
            classify(argv0, &["-c", "echo ok", flag], ALL_TTY),
            Invocation::ExecShell(_)
        ));
        assert!(!matches!(
            classify(argv0, &["raw", flag], ALL_TTY),
            Invocation::LoginEditor
        ));
        let Invocation::Tui(entry) = classify(argv0, &["--ask", flag], ALL_TTY) else {
            panic!("ask value stolen");
        };
        assert_eq!(entry.launch_options.initial_question.as_deref(), Some(flag));
    }
}

#[test]
fn login_lifecycle_namespace_is_owned_only_at_the_first_token() {
    let flag = "--__cosh-login-lifecycle-v1";
    for argv0 in ["cosh", "cosh-shell"] {
        assert_eq!(
            classify(
                argv0,
                &[
                    flag,
                    "check-protocol",
                    "1",
                    "--record-version",
                    "1",
                    "--entry",
                    "/usr/bin/cosh"
                ],
                (false, false, false)
            ),
            Invocation::LoginLifecycle(os(&[
                "check-protocol",
                "1",
                "--record-version",
                "1",
                "--entry",
                "/usr/bin/cosh"
            ]))
        );
        for args in [
            vec!["-c", "printf ok", flag],
            vec!["--", flag],
            vec!["script.sh", flag],
        ] {
            assert!(matches!(
                classify(argv0, &args, ALL_TTY),
                Invocation::ExecShell(_)
            ));
        }
        assert!(!matches!(
            classify(argv0, &["raw", flag], ALL_TTY),
            Invocation::LoginLifecycle(_)
        ));
        let Invocation::Tui(entry) = classify(argv0, &["--ask", flag], ALL_TTY) else {
            panic!("question stolen");
        };
        assert_eq!(entry.launch_options.initial_question.as_deref(), Some(flag));
    }
}

#[test]
fn managed_exec_probe_namespace_is_private_and_never_captures_shell_tails() {
    let flag = crate::runtime::login::probe::WORKER_FLAG;
    for argv0 in ["cosh", "cosh-shell"] {
        assert_eq!(
            classify(argv0, &[flag], (false, false, false)),
            Invocation::LoginShellPlanWorker
        );
        assert!(matches!(
            classify(argv0, &[flag, "extra"], ALL_TTY),
            Invocation::UsageError(_)
        ));
        for args in [
            vec!["-c", "printf ok", flag],
            vec!["--", flag],
            vec!["script.sh", flag],
        ] {
            assert!(matches!(
                classify(argv0, &args, ALL_TTY),
                Invocation::ExecShell(_)
            ));
        }
        assert!(!matches!(
            classify(argv0, &["raw", flag], ALL_TTY),
            Invocation::LoginShellPlanWorker
        ));
        let Invocation::Tui(entry) = classify(argv0, &["--ask", flag], ALL_TTY) else {
            panic!("question stolen")
        };
        assert_eq!(entry.launch_options.initial_question.as_deref(), Some(flag));
    }
}

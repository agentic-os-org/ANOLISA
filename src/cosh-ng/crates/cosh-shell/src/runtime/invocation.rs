//! Invocation-transparency dispatch for the `/usr/bin/cosh` entry.
//!
//! Single source of truth for the installed entry: the reserved `agent`
//! namespace enters the Gateway, the TUI is an allowlist, and other shapes
//! preserve shell-owned argv unless a leading cosh-owned `--isolated` requests
//! target-specific startup suppression or fail-closed handling.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;

use super::cli_args::{parse_raw_shell, LaunchOptions, RawShellKind, ResumeLaunch};

mod exec;
mod raw;
pub(crate) use exec::{exec_gateway, exec_shell};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Invocation {
    /// Allowlist hit: start the interactive TUI.
    Tui(TuiEntry),
    /// Everything else: replace this process with the inner shell.
    ExecShell(ExecPlan),
    UsageError(&'static str),
    Guide(Vec<OsString>),
    Login(Vec<OsString>),
    ShellIntegration(&'static str),
    HealthWorker,
    LoginInvitationWorker,
    LoginShellPlanWorker,
    LoginEditor,
    LoginLifecycle(Vec<OsString>),
    Health(Vec<OsString>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GatewayPlan {
    /// Arguments after the reserved `agent` namespace token.
    pub(crate) args: Vec<OsString>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TuiEntry {
    pub(crate) login_argv0: bool,
    pub(crate) adapter_name: Option<String>,
    pub(crate) shell_kind: Option<RawShellKind>,
    pub(crate) launch_options: LaunchOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecOrigin {
    Primary,
    Raw,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecPlan {
    pub(crate) origin: ExecOrigin,
    /// `--shell` override consumed from the cosh-owned leading options.
    pub(crate) shell_override: Option<OsString>,
    /// argv[0] passed through verbatim (login `-` prefix included) so the
    /// inner shell observes the same `$0` contract as a direct bash call.
    pub(crate) arg0: OsString,
    /// Remaining inner-shell argv. Cosh-owned target metadata is consumed
    /// before this boundary; bytes after the first shell token stay verbatim.
    pub(crate) args: Vec<OsString>,
    /// Translate the cosh-owned isolation contract for the selected shell.
    pub(crate) isolated: bool,
}

/// True when argv[0] names the `/usr/bin/cosh` entry (optionally with the
/// login `-` prefix set by login(1)/su).
pub(crate) fn is_cosh_entry(argv0: &OsStr) -> bool {
    entry_basename(argv0) == b"cosh"
}

/// Remove the reserved `agent` namespace before entering the Gateway.
pub(crate) fn gateway_plan(args: &[OsString]) -> Option<GatewayPlan> {
    (args.first().and_then(|arg| arg.to_str()) == Some("agent")).then(|| GatewayPlan {
        args: args[1..].to_vec(),
    })
}

fn entry_basename(argv0: &OsStr) -> &[u8] {
    let bytes = argv0.as_bytes();
    let bytes = bytes.strip_prefix(b"-").unwrap_or(bytes);
    match bytes.rsplit(|byte| *byte == b'/').next() {
        Some(name) => name,
        None => bytes,
    }
}

fn login_argv0(argv0: &OsStr) -> bool {
    argv0.as_bytes().first() == Some(&b'-')
}

/// Standalone allowlist flags recognized by the classifier. Single vocabulary
/// source used by the classifier and the launch-parser vocabulary-contract
/// test, so adding a flag also checks that launch parsing consumes it.
/// Exec handling remains
/// flag-specific; value-carrying flags (`--shell`, `--resume`) stay explicit.
pub(crate) const TUI_STANDALONE_FLAGS: &[&str] = &["-l", "--login", "--isolated"];

/// Classify one invocation of the transparency contract entry.
///
/// Scan rules (single pass, fail-safe):
/// 1. `--shell <v>` / `--shell=v` is consumed as exec metadata (it selects
///    the inner shell). A missing, empty, or dash-leading value is not
///    consumed: the token is handed to the inner shell verbatim, which
///    reports it as an invalid option.
/// 2. Login flags (`-l`/`--login`) and TUI-only resume flags stay eligible
///    for the TUI and are kept verbatim for the inner shell on the exec side.
///    `--isolated` is cosh-owned target metadata and is translated only after
///    the inner shell is selected.
/// 3. Any other token (options, operands, unknown or future flags, non-UTF-8
///    bytes) short-circuits to `ExecShell` with the remainder untouched, so
///    the worst case for an unclassified shape is native bash.
/// 4. Owned/login flags only: the TUI additionally requires stdin, stdout,
///    and stderr to all be terminals. This is deliberately stricter than
///    bash's own interactivity rule (stdin + stderr, INVOCATION in bash(1)):
///    whenever any fd is not a terminal the invocation degrades to the
///    inner shell verbatim, and interactivity is decided by bash's native
///    rule on the real fd topology — so equivalence holds on every
///    topology, and the TUI only claims fully terminal-backed sessions.
pub(crate) fn classify_invocation(
    argv0: &OsStr,
    args: &[OsString],
    stdin_tty: bool,
    stdout_tty: bool,
    stderr_tty: bool,
) -> Invocation {
    if args.first().and_then(|arg| arg.to_str())
        == Some(crate::diagnostics::health::runtime::PRIVATE_HEALTH_WORKER_FLAG)
    {
        return if args.len() == 1 {
            Invocation::HealthWorker
        } else {
            Invocation::UsageError("private health worker does not accept arguments")
        };
    }
    if args.first().and_then(|arg| arg.to_str())
        == Some(crate::runtime::login::invitation::WORKER_FLAG)
    {
        return if args.len() == 1 {
            Invocation::LoginInvitationWorker
        } else {
            Invocation::UsageError("private login invitation worker does not accept arguments")
        };
    }
    if args.first().and_then(|arg| arg.to_str()) == Some(crate::runtime::login::editor::EDITOR_FLAG)
    {
        return if args.len() == 3
            && args[1].to_str() == Some("3")
            && args[2] == OsStr::new(crate::runtime::login::editor::TEMP_PATH)
        {
            Invocation::LoginEditor
        } else {
            Invocation::UsageError("invalid private login editor invocation")
        };
    }
    if args.first().and_then(|arg| arg.to_str()) == Some(crate::runtime::login::probe::WORKER_FLAG)
    {
        return if args.len() == 1 {
            Invocation::LoginShellPlanWorker
        } else {
            Invocation::UsageError("private login shell query does not accept arguments")
        };
    }
    if args.first().and_then(|arg| arg.to_str()) == Some("--__cosh-login-lifecycle-v1") {
        return Invocation::LoginLifecycle(args[1..].to_vec());
    }
    if args.first().and_then(|arg| arg.to_str()) == Some("raw") {
        return raw::classify_raw_invocation(
            argv0,
            &args[1..],
            stdin_tty && stdout_tty && stderr_tty,
        );
    }

    if args.first().and_then(|arg| arg.to_str()) == Some("shell-integration") {
        return match (args.get(1).and_then(|arg| arg.to_str()), args.len()) {
            (Some("compat"), 2) => Invocation::ShellIntegration("compat"),
            (Some("advise"), 2) => Invocation::ShellIntegration("advise"),
            (Some("--__compat-worker-v2"), 2) => {
                Invocation::ShellIntegration("--__compat-worker-v2")
            }
            _ => Invocation::UsageError("usage: cosh shell-integration compat|advise"),
        };
    }
    if args.first().and_then(|arg| arg.to_str()) == Some("login") {
        return Invocation::Login(args[1..].to_vec());
    }
    if args.first().and_then(|arg| arg.to_str()) == Some("health") {
        return Invocation::Health(args[1..].to_vec());
    }
    if args.first().and_then(|arg| arg.to_str()) == Some("guide") {
        return Invocation::Guide(args[1..].to_vec());
    }

    let mut entry = TuiEntry {
        login_argv0: login_argv0(argv0),
        ..Default::default()
    };
    entry.launch_options.login = entry.login_argv0;
    let mut kept: Vec<OsString> = Vec::new();
    let mut shell_override: Option<OsString> = None;
    let mut isolated = false;
    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        match arg.to_str() {
            Some("--isolated") => {
                isolated = true;
                entry.launch_options.isolated = true;
                idx += 1;
            }
            Some(text) if TUI_STANDALONE_FLAGS.contains(&text) => {
                entry.launch_options.login = true;
                kept.push(arg.clone());
                idx += 1;
            }
            Some("--shell")
                if args
                    .get(idx + 1)
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| !value.is_empty() && !value.starts_with('-')) =>
            {
                entry
                    .shell_kind
                    .get_or_insert_with(|| parse_raw_shell(&args[idx + 1].to_string_lossy()));
                shell_override = Some(args[idx + 1].clone());
                idx += 2;
            }
            Some(text)
                if text.starts_with("--shell=")
                    && !text["--shell=".len()..].is_empty()
                    && !text["--shell=".len()..].starts_with('-') =>
            {
                entry
                    .shell_kind
                    .get_or_insert_with(|| parse_raw_shell(&text["--shell=".len()..]));
                shell_override = Some(OsString::from(&text["--shell=".len()..]));
                idx += 1;
            }
            Some("--context") => {
                if let Err(error) =
                    parse_failure_context(&mut entry.launch_options, args.get(idx + 1))
                {
                    return Invocation::UsageError(error);
                }
                idx += 2;
            }
            Some("--ask") => {
                if let Err(error) =
                    parse_initial_question(&mut entry.launch_options, args.get(idx + 1))
                {
                    return Invocation::UsageError(error);
                }
                idx += 2;
            }
            Some("--resume") => {
                if entry.launch_options.initial_question.is_some() {
                    return Invocation::UsageError("--ask cannot be combined with --resume");
                }
                entry.launch_options.resume = Some(ResumeLaunch::Picker);
                kept.push(arg.clone());
                idx += 1;
                if args
                    .get(idx)
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| !value.starts_with('-'))
                {
                    entry.launch_options.resume = Some(ResumeLaunch::Session(
                        args[idx].to_string_lossy().into_owned(),
                    ));
                    kept.push(args[idx].clone());
                    idx += 1;
                }
            }
            Some(text) if text.starts_with("--resume=") => {
                if entry.launch_options.initial_question.is_some() {
                    return Invocation::UsageError("--ask cannot be combined with --resume");
                }
                let value = &text["--resume=".len()..];
                entry.launch_options.resume = Some(if value.is_empty() {
                    ResumeLaunch::Picker
                } else {
                    ResumeLaunch::Session(value.to_string())
                });
                kept.push(arg.clone());
                idx += 1;
            }
            _ => {
                if entry.launch_options.initial_question.is_some()
                    || entry.launch_options.failure_context.is_some()
                {
                    return Invocation::UsageError("--ask cannot be combined with shell arguments");
                }
                // First non-cosh token (including a `--shell` without a
                // usable value): the rest of argv belongs to the inner shell
                // byte-for-byte (no reordering, no interpretation).
                let mut rest = kept;
                rest.extend(args[idx..].iter().cloned());
                return Invocation::ExecShell(ExecPlan {
                    origin: ExecOrigin::Primary,
                    shell_override,
                    arg0: argv0.to_os_string(),
                    args: rest,
                    isolated,
                });
            }
        }
    }

    if entry.launch_options.failure_context.is_some()
        && entry.launch_options.initial_question.is_none()
    {
        return Invocation::UsageError("--context requires --ask");
    }
    if entry.launch_options.initial_question.is_some() && !(stdin_tty && stdout_tty && stderr_tty) {
        return Invocation::UsageError("--ask requires an interactive terminal");
    }
    if stdin_tty && stdout_tty && stderr_tty {
        Invocation::Tui(entry)
    } else {
        Invocation::ExecShell(ExecPlan {
            origin: ExecOrigin::Primary,
            shell_override,
            arg0: argv0.to_os_string(),
            args: kept,
            isolated,
        })
    }
}

fn parse_failure_context(
    options: &mut LaunchOptions,
    value: Option<&OsString>,
) -> Result<(), &'static str> {
    if options.failure_context.is_some() || options.resume.is_some() {
        return Err("--context requires one --ask invocation");
    }
    let id = value
        .and_then(|value| value.to_str())
        .filter(|id| crate::config::guide::valid_context_id(id))
        .ok_or("--context requires a valid event ID")?;
    options.failure_context = Some(id.to_string());
    Ok(())
}

fn parse_initial_question(
    options: &mut LaunchOptions,
    value: Option<&OsString>,
) -> Result<(), &'static str> {
    if options.resume.is_some() {
        return Err("--ask cannot be combined with --resume");
    }
    if options.initial_question.is_some() {
        return Err("--ask may only be specified once");
    }
    let question = value
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .ok_or("--ask requires a non-empty question")?;
    options.initial_question = Some(question.to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    fn classify(argv0: &str, args: &[&str], tty: (bool, bool, bool)) -> Invocation {
        classify_invocation(OsStr::new(argv0), &os(args), tty.0, tty.1, tty.2)
    }

    const ALL_TTY: (bool, bool, bool) = (true, true, true);

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
    fn classify_row_1_bare_terminal_enters_tui() {
        assert_eq!(
            classify("cosh", &[], ALL_TTY),
            Invocation::Tui(TuiEntry {
                ..TuiEntry::default()
            })
        );
    }

    #[test]
    fn classify_row_2_bare_without_all_three_terminals_execs_shell() {
        for stdin_tty in [true, false] {
            for stdout_tty in [true, false] {
                for stderr_tty in [true, false] {
                    if stdin_tty && stdout_tty && stderr_tty {
                        continue;
                    }
                    let invocation = classify("cosh", &[], (stdin_tty, stdout_tty, stderr_tty));
                    assert_eq!(
                        exec_args(invocation),
                        Vec::<OsString>::new(),
                        "fd topology ({stdin_tty},{stdout_tty},{stderr_tty})"
                    );
                }
            }
        }
    }

    #[test]
    fn classify_row_3_login_argv0_sets_login_and_preserves_arg0() {
        assert_eq!(
            classify("-cosh", &[], ALL_TTY),
            Invocation::Tui(TuiEntry {
                login_argv0: true,
                launch_options: LaunchOptions {
                    login: true,
                    ..LaunchOptions::default()
                },
                ..TuiEntry::default()
            })
        );
        match classify("-cosh", &["-c", "id"], ALL_TTY) {
            Invocation::ExecShell(plan) => assert_eq!(plan.arg0, OsString::from("-cosh")),
            other => panic!("expected ExecShell, got {other:?}"),
        }
    }

    #[test]
    fn classify_row_4_login_flags_enter_tui_and_pass_through_on_exec() {
        for flag in ["-l", "--login"] {
            assert_eq!(
                classify("cosh", &[flag], ALL_TTY),
                Invocation::Tui(TuiEntry {
                    launch_options: LaunchOptions {
                        login: true,
                        ..LaunchOptions::default()
                    },
                    ..TuiEntry::default()
                })
            );
            assert_eq!(
                exec_args(classify("cosh", &[flag], (false, false, false))),
                os(&[flag])
            );
        }
    }

    #[test]
    fn classify_rows_5_6_command_string_and_combined_flags_exec_verbatim() {
        for args in [
            vec!["-c", "printf ok"],
            vec!["-lc", "printf ok"],
            vec!["-cl", "printf ok"],
            vec!["-ic", "printf ok"],
            vec!["-i", "-c", "printf ok"],
        ] {
            assert_eq!(
                exec_args(classify("cosh", &args, ALL_TTY)),
                os(&args),
                "args {args:?}"
            );
        }
    }

    #[test]
    fn classify_rows_7_8_bash_options_exec_verbatim() {
        for args in [
            vec!["--posix", "-c", "set -o"],
            vec!["--norc", "-i"],
            vec!["--noprofile", "--norc", "-i"],
            vec!["--restricted", "-c", "cd /"],
            vec!["--dump-strings", "-c", "true"],
            vec!["--debugger", "-c", "true"],
            vec!["--noediting", "-i"],
            vec!["--rcfile", "/tmp/rc", "-i"],
            vec!["-O", "extglob", "-c", "shopt -q extglob"],
            vec!["+O", "extglob", "-c", "shopt -q extglob"],
        ] {
            assert_eq!(
                exec_args(classify("cosh", &args, ALL_TTY)),
                os(&args),
                "args {args:?}"
            );
        }
    }

    #[test]
    fn classify_rows_9_to_13_operands_and_edge_options_exec_verbatim() {
        for args in [
            vec!["-s", "name", "a", "b"],
            vec!["/definitely/not/present"],
            vec!["script.sh", "arg1"],
            vec!["--definitely-invalid"],
            vec!["-c"],
            vec!["--", "operand"],
        ] {
            assert_eq!(
                exec_args(classify("cosh", &args, ALL_TTY)),
                os(&args),
                "args {args:?}"
            );
        }
    }

    #[test]
    fn classify_row_14_force_interactive_flags_exec_even_on_terminals() {
        for args in [
            vec!["-i"],
            vec!["--norc", "-i"],
            vec!["--noprofile", "--norc", "-i"],
            vec!["--login", "-i"],
        ] {
            let invocation = classify("cosh", &args, ALL_TTY);
            assert!(
                matches!(invocation, Invocation::ExecShell(_)),
                "args {args:?} classified {invocation:?}"
            );
        }
    }

    #[test]
    fn classify_row_15_shell_override_is_consumed_before_the_trigger_only() {
        match classify("cosh", &["--shell", "zsh", "-c", "true"], ALL_TTY) {
            Invocation::ExecShell(plan) => {
                assert_eq!(plan.shell_override, Some(OsString::from("zsh")));
                assert_eq!(plan.args, os(&["-c", "true"]));
            }
            other => panic!("expected ExecShell, got {other:?}"),
        }
        // Past the trigger the remainder is inner-shell argv ($0/$@ under
        // `-c`), so a later `--shell` is data, not a cosh flag.
        assert_eq!(
            exec_args(classify("cosh", &["-c", "true", "--shell", "zsh"], ALL_TTY)),
            os(&["-c", "true", "--shell", "zsh"])
        );
    }

    #[test]
    fn classify_shell_guard_never_consumes_options_or_empty_values() {
        // A `--shell` without a usable value is not a cosh flag anymore: it
        // goes to the inner shell verbatim (invalid option, exit 2) instead
        // of eating the next token or silently vanishing.
        for args in [
            vec!["--shell"],
            vec!["--shell", "-c", "echo hi"],
            vec!["--shell", "--isolated"],
            vec!["--shell="],
        ] {
            match classify("cosh", &args, ALL_TTY) {
                Invocation::ExecShell(plan) => {
                    assert_eq!(plan.shell_override, None, "args {args:?}");
                    assert_eq!(plan.args, os(&args), "args {args:?}");
                }
                other => panic!("expected ExecShell for {args:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn classify_rows_18_20_version_help_and_future_flags_exec_verbatim() {
        for args in [vec!["--version"], vec!["--help"], vec!["--future-flag"]] {
            assert_eq!(
                exec_args(classify("cosh", &args, ALL_TTY)),
                os(&args),
                "args {args:?}"
            );
        }
    }

    #[test]
    fn classify_row_19_non_utf8_argv_execs_verbatim() {
        let raw = OsString::from_vec(b"\xff\xfe".to_vec());
        let args = vec![raw.clone()];
        match classify_invocation(OsStr::new("cosh"), &args, true, true, true) {
            Invocation::ExecShell(plan) => assert_eq!(plan.args, vec![raw]),
            other => panic!("expected ExecShell, got {other:?}"),
        }
    }

    #[test]
    fn invariant_i1_tui_requires_all_three_terminals_for_every_allowlist_shape() {
        for args in [
            vec![],
            vec!["-l"],
            vec!["--login"],
            vec!["--resume"],
            vec!["--isolated"],
            vec!["--shell", "zsh"],
        ] {
            for stdin_tty in [true, false] {
                for stdout_tty in [true, false] {
                    for stderr_tty in [true, false] {
                        let invocation =
                            classify("cosh", &args, (stdin_tty, stdout_tty, stderr_tty));
                        assert_eq!(
                            matches!(invocation, Invocation::Tui(_)),
                            stdin_tty && stdout_tty && stderr_tty,
                            "args {args:?} fd topology ({stdin_tty},{stdout_tty},{stderr_tty})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn invariant_i2_i4_arguments_survive_byte_for_byte_in_order() {
        let args = os(&["-l", "-lc", "", "printf %s", "--", "-c", "später"]);
        match classify_invocation(OsStr::new("cosh"), &args, true, true, true) {
            Invocation::ExecShell(plan) => assert_eq!(plan.args, args),
            other => panic!("expected ExecShell, got {other:?}"),
        }
    }

    #[test]
    fn invariant_i3_arg0_is_preserved_verbatim() {
        for argv0 in ["cosh", "-cosh", "/usr/bin/cosh"] {
            match classify(argv0, &["-c", "true"], ALL_TTY) {
                Invocation::ExecShell(plan) => assert_eq!(plan.arg0, OsString::from(argv0)),
                other => panic!("expected ExecShell, got {other:?}"),
            }
        }
        let raw0 = OsString::from_vec(b"-c\xffosh".to_vec());
        match classify_invocation(&raw0, &os(&["-c", "true"]), true, true, true) {
            Invocation::ExecShell(plan) => assert_eq!(plan.arg0, raw0),
            other => panic!("expected ExecShell, got {other:?}"),
        }
    }

    #[test]
    fn login_invocation_is_carried_by_the_classification_result() {
        for (argv0, args, login) in [
            ("-cosh", vec![], true),
            ("cosh", vec!["-l"], true),
            ("cosh", vec!["--isolated", "--login"], true),
            ("/usr/bin/cosh", vec![], false),
        ] {
            let Invocation::Tui(entry) = classify(argv0, &args, ALL_TTY) else {
                panic!("expected Tui")
            };
            assert_eq!(entry.launch_options.login, login);
        }
    }
}

#[cfg(test)]
#[path = "invocation_tests.rs"]
mod invocation_tests;

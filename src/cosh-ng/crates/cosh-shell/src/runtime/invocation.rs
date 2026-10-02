//! Invocation-transparency dispatch for the `/usr/bin/cosh` entry.
//!
//! Single source of truth for the installed entry: the reserved `agent`
//! namespace enters the Gateway, the TUI is an allowlist, and other shapes
//! preserve shell-owned argv unless a leading cosh-owned `--isolated` requests
//! target-specific startup suppression or fail-closed handling.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Invocation {
    /// Allowlist hit: start the interactive TUI.
    Tui(TuiEntry),
    /// Everything else: replace this process with the inner shell.
    ExecShell(ExecPlan),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GatewayPlan {
    /// Arguments after the reserved `agent` namespace token.
    pub(crate) args: Vec<OsString>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TuiEntry {
    pub(crate) login: bool,
    /// Arguments consumed by the TUI after any entry subcommand is removed.
    pub(crate) launch_args: Vec<OsString>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecPlan {
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
/// source shared by the classifier scan, the launch-side adapter scan
/// (`cli_args::adapter_name_from_args`), and the vocabulary-contract test, so
/// adding a flag here mechanically updates all three. Exec handling remains
/// flag-specific; value-carrying flags (`--shell`, `--resume`) stay explicit.
pub(crate) const TUI_STANDALONE_FLAGS: &[&str] = &["-l", "--login", "--isolated"];

/// Single source of truth for the login-shell bit: argv[0] carries the
/// login `-` prefix (login(1)/su convention) or an explicit `-l`/`--login`
/// flag appears anywhere in argv. Shared by the classifier and the TUI
/// bootstrap so the two sites cannot drift.
pub(crate) fn is_login_invocation<A: AsRef<OsStr>>(argv0: &OsStr, args: &[A]) -> bool {
    login_argv0(argv0)
        || args
            .iter()
            .any(|arg| matches!(arg.as_ref().to_str(), Some("-l") | Some("--login")))
}

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
    if args.first().and_then(|arg| arg.to_str()) == Some("raw") {
        // Keep the legacy non-interactive escape hatches (`raw -c` and
        // `raw --`) transparent, but treat every other raw shape as the
        // explicit TUI request that the direct cosh-shell entry accepts.
        if let Some(args) = normalize_raw_invocation(args) {
            return classify_invocation(argv0, &args, stdin_tty, stdout_tty, stderr_tty);
        }

        return Invocation::Tui(TuiEntry {
            login: is_login_invocation(argv0, args),
            launch_args: args[1..].to_vec(),
        });
    }

    let mut kept: Vec<OsString> = Vec::new();
    let mut shell_override: Option<OsString> = None;
    let mut isolated = false;
    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        match arg.to_str() {
            Some("--isolated") => {
                isolated = true;
                idx += 1;
            }
            Some(text) if TUI_STANDALONE_FLAGS.contains(&text) => {
                kept.push(arg.clone());
                idx += 1;
            }
            Some("--shell")
                if args
                    .get(idx + 1)
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| !value.is_empty() && !value.starts_with('-')) =>
            {
                shell_override = Some(args[idx + 1].clone());
                idx += 2;
            }
            Some(text)
                if text.starts_with("--shell=")
                    && !text["--shell=".len()..].is_empty()
                    && !text["--shell=".len()..].starts_with('-') =>
            {
                shell_override = Some(OsString::from(&text["--shell=".len()..]));
                idx += 1;
            }
            Some("--resume") => {
                kept.push(arg.clone());
                idx += 1;
                if args
                    .get(idx)
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| !value.starts_with('-'))
                {
                    kept.push(args[idx].clone());
                    idx += 1;
                }
            }
            Some(text) if text.starts_with("--resume=") => {
                kept.push(arg.clone());
                idx += 1;
            }
            _ => {
                // First non-cosh token (including a `--shell` without a
                // usable value): the rest of argv belongs to the inner shell
                // byte-for-byte (no reordering, no interpretation).
                let mut rest = kept;
                rest.extend(args[idx..].iter().cloned());
                return Invocation::ExecShell(ExecPlan {
                    shell_override,
                    arg0: argv0.to_os_string(),
                    args: rest,
                    isolated,
                });
            }
        }
    }

    if stdin_tty && stdout_tty && stderr_tty {
        Invocation::Tui(TuiEntry {
            login: is_login_invocation(argv0, args),
            launch_args: args.to_vec(),
        })
    } else {
        Invocation::ExecShell(ExecPlan {
            shell_override,
            arg0: argv0.to_os_string(),
            args: kept,
            isolated,
        })
    }
}

/// Extract the passthrough candidate from a `cosh-shell raw [adapter] …`
/// invocation. `raw` is an explicit TUI request, so only an explicit `-c`
/// command string or a leading `--` diverts to passthrough (legacy
/// contract); every other shape — including unknown raw-surface flags such
/// as `--run` — returns `None` and stays on the TUI launch path.
pub(crate) fn normalize_raw_invocation(args: &[OsString]) -> Option<Vec<OsString>> {
    if args.first().and_then(|arg| arg.to_str()) != Some("raw") {
        return None;
    }

    let mut out: Vec<OsString> = Vec::new();
    let mut skipped_adapter = false;
    let mut idx = 1;
    while idx < args.len() {
        let arg = &args[idx];
        match arg.to_str() {
            // Legacy gate: `--` only diverts when it is the first normalized
            // token; after cosh-owned flags it stays a TUI launch error
            // surface, matching the pre-classifier scan.
            Some("--") => {
                if !out.is_empty() {
                    return None;
                }
                out.extend(args[idx..].iter().cloned());
                return Some(out);
            }
            Some("-c") => {
                out.extend(args[idx..].iter().cloned());
                return Some(out);
            }
            Some("--shell") => {
                out.push(arg.clone());
                if let Some(value) = args.get(idx + 1) {
                    out.push(value.clone());
                    idx += 2;
                } else {
                    idx += 1;
                }
            }
            Some("--isolated") | Some("--login") | Some("-l") => {
                out.push(arg.clone());
                idx += 1;
            }
            Some(text) if text.starts_with("--shell=") => {
                out.push(arg.clone());
                idx += 1;
            }
            Some(text) if !text.starts_with('-') && !skipped_adapter => {
                skipped_adapter = true;
                idx += 1;
            }
            _ => return None,
        }
    }

    None
}

/// Execute the inner-shell plan, preserving shell-owned argv and SIGPIPE state
/// outside the cosh-owned isolation contract. Isolated Bash execs suppress its
/// startup inputs and reject login shells (Bash cannot suppress logout files);
/// isolated Zsh execs use `-f` only with no shell arguments and
/// reject arguments before exec; unsupported targets retain `--isolated` and
/// fail loudly. Returns 2 for isolation rejection, or 127/126 when exec fails.
pub(crate) fn exec_shell(plan: ExecPlan) -> i32 {
    use std::os::unix::process::CommandExt;

    let shell = plan
        .shell_override
        .or_else(|| std::env::var_os("COSH_SHELL_DEFAULT_SHELL").filter(|value| !value.is_empty()))
        .unwrap_or_else(|| OsString::from("bash"));
    let mut command = std::process::Command::new(&shell);
    if plan.isolated {
        match entry_basename(&shell) {
            b"bash" | b"cosh-shell-bash" => {
                if bash_login_requested(&plan.arg0, &plan.args) {
                    let program = String::from_utf8_lossy(entry_basename(&plan.arg0));
                    eprintln!("{program}: isolated Bash login shells are not supported");
                    return 2;
                }
                // Bash long options must precede single-character options.
                // `--norc` also covers bash's remote-daemon startup path.
                command.args(["--noprofile", "--norc"]);
                command.env_remove("BASH_ENV");
                command.env_remove("ENV");
            }
            b"zsh" | b"cosh-shell-zsh" => {
                if !plan.args.is_empty() {
                    let program = String::from_utf8_lossy(entry_basename(&plan.arg0));
                    eprintln!("{program}: isolated Zsh shell arguments are not supported");
                    return 2;
                }
                command.arg("-f");
            }
            _ => {
                // Unsupported targets retain the fail-loud contract instead
                // of silently claiming isolation that was not established.
                command.arg("--isolated");
            }
        }
    }
    command.args(&plan.args).arg0(&plan.arg0);
    unsafe {
        command.pre_exec(crate::shell_host::sigpipe::restore_in_child);
    }
    let error = command.exec();

    let program = String::from_utf8_lossy(entry_basename(&plan.arg0)).into_owned();
    let shell = shell.to_string_lossy();
    // Match the shell wording for exec failures instead of Rust's
    // "(os error N)" suffix, keeping stderr bytes shell-shaped.
    let reason = match error.kind() {
        std::io::ErrorKind::NotFound => "No such file or directory".to_string(),
        std::io::ErrorKind::PermissionDenied => "Permission denied".to_string(),
        _ => error.to_string(),
    };
    eprintln!("{program}: {shell}: {reason}");
    if error.kind() == std::io::ErrorKind::NotFound {
        127
    } else {
        126
    }
}

fn bash_login_requested(argv0: &OsStr, args: &[OsString]) -> bool {
    if login_argv0(argv0) {
        return true;
    }
    let mut args = args.iter();
    let mut long_options = true;
    while let Some(arg) = args.next() {
        let bytes = arg.as_bytes();
        match bytes {
            b"-" | b"--" => break,
            b"--login" | b"-login" => return true,
            b"--rcfile" | b"-rcfile" | b"--init-file" | b"-init-file" if long_options => {
                args.next();
            }
            // Bash accepts long names with one dash before its short options.
            b"-debug" | b"-debugger" | b"-dump-po-strings" | b"-dump-strings" | b"-help"
            | b"-noediting" | b"-noprofile" | b"-norc" | b"-posix" | b"-pretty-print"
            | b"-protected" | b"-restricted" | b"-verbose" | b"-version" | b"-wordexp"
                if long_options => {}
            _ if bytes.starts_with(b"--") => {}
            _ if matches!(bytes.first(), Some(b'-' | b'+')) => {
                long_options = false;
                for option in &bytes[1..] {
                    match option {
                        // Bash treats both -l and +l as a login request.
                        b'l' => return true,
                        b'o' | b'O' => {
                            args.next();
                        }
                        _ => {}
                    }
                }
            }
            // Bash finishes option parsing before the command/script operand.
            _ => break,
        }
    }
    false
}

/// Replace the installed `cosh` entry with its sibling Gateway binary.
pub(crate) fn exec_gateway(plan: GatewayPlan) -> i32 {
    use std::os::unix::process::CommandExt;

    let gateway = match std::env::current_exe().ok().and_then(|executable| {
        executable
            .parent()
            .map(|parent| parent.join("cosh-gateway"))
    }) {
        Some(gateway) => gateway,
        None => {
            eprintln!("cosh: cosh-gateway: cannot resolve sibling executable");
            return 126;
        }
    };
    let mut command = std::process::Command::new(&gateway);
    command.args(&plan.args);
    unsafe {
        command.pre_exec(crate::shell_host::sigpipe::restore_in_child);
    }
    let error = command.exec();
    let reason = match error.kind() {
        std::io::ErrorKind::NotFound => "No such file or directory".to_string(),
        std::io::ErrorKind::PermissionDenied => "Permission denied".to_string(),
        _ => error.to_string(),
    };
    eprintln!("cosh: {}: {reason}", gateway.display());
    if error.kind() == std::io::ErrorKind::NotFound {
        127
    } else {
        126
    }
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
        }
    }

    #[test]
    fn classify_row_1_bare_terminal_enters_tui() {
        assert_eq!(
            classify("cosh", &[], ALL_TTY),
            Invocation::Tui(TuiEntry {
                login: false,
                launch_args: os(&[]),
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
                login: true,
                launch_args: os(&[]),
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
                    login: true,
                    launch_args: os(&[flag]),
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
    fn login_invocation_shares_one_truth_for_argv0_and_flags() {
        assert!(is_login_invocation(OsStr::new("-cosh"), &os(&[])));
        assert!(is_login_invocation(OsStr::new("cosh"), &os(&["-l"])));
        assert!(is_login_invocation(
            OsStr::new("cosh"),
            &os(&["--isolated", "--login"])
        ));
        assert!(!is_login_invocation(OsStr::new("cosh"), &os(&["-lc"])));
        assert!(!is_login_invocation(OsStr::new("/usr/bin/cosh"), &os(&[])));
    }
}

#[cfg(test)]
#[path = "invocation_tests.rs"]
mod invocation_tests;

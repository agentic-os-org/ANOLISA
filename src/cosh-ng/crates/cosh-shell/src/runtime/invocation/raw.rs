use super::{login_argv0, ExecOrigin, ExecPlan, Invocation, TuiEntry};
use crate::runtime::cli_args::{parse_raw_shell, RawShellKind, ResumeLaunch};
use std::ffi::{OsStr, OsString};

pub(super) fn classify_raw_invocation(
    argv0: &OsStr,
    args: &[OsString],
    all_tty: bool,
) -> Invocation {
    let mut entry = TuiEntry {
        login_argv0: login_argv0(argv0),
        ..Default::default()
    };
    entry.launch_options.login = entry.login_argv0;
    let mut plan = ExecPlan {
        origin: ExecOrigin::Raw,
        shell_override: None,
        arg0: argv0.to_os_string(),
        args: Vec::new(),
        isolated: false,
    };
    // Raw keeps accepting TUI options after its legacy passthrough gate closes.
    let mut legacy = true;
    let mut prefix_empty = true;
    let mut shell_owned = false;
    let mut adapter_skipped = false;
    let mut shell_arguments = false;
    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        let text = arg.to_string_lossy();
        if arg.to_str().is_none() {
            legacy = false;
        }
        match text.as_ref() {
            "-c" if legacy => {
                plan.args.extend(args[idx..].iter().cloned());
                return Invocation::ExecShell(plan);
            }
            "--" if legacy && prefix_empty => {
                plan.args.extend(args[idx..].iter().cloned());
                return Invocation::ExecShell(plan);
            }
            "--shell" => {
                let value = args.get(idx + 1);
                if entry.shell_kind.is_none() {
                    entry.shell_kind = Some(match value {
                        Some(value) if !value.to_string_lossy().starts_with("--") => {
                            parse_raw_shell(&value.to_string_lossy())
                        }
                        _ => RawShellKind::MissingShellValue,
                    });
                }
                if legacy {
                    prefix_empty = false;
                    if !shell_owned
                        && value
                            .and_then(|value| value.to_str())
                            .is_some_and(|value| !value.is_empty() && !value.starts_with('-'))
                    {
                        plan.shell_override = value.cloned();
                    } else {
                        shell_owned = true;
                        plan.args.push(arg.clone());
                        plan.args.extend(value.cloned());
                    }
                }
                idx += if value.is_some() { 2 } else { 1 };
            }
            text if text.starts_with("--shell=") => {
                let value = &text["--shell=".len()..];
                entry
                    .shell_kind
                    .get_or_insert_with(|| parse_raw_shell(value));
                if legacy {
                    prefix_empty = false;
                    if !shell_owned && !value.is_empty() && !value.starts_with('-') {
                        plan.shell_override = Some(OsString::from(value));
                    } else {
                        shell_owned = true;
                        plan.args.push(arg.clone());
                    }
                }
                idx += 1;
            }
            "--login" | "-l" | "--isolated" => {
                if text == "--isolated" {
                    entry.launch_options.isolated = true;
                } else {
                    entry.launch_options.login = true;
                }
                if legacy {
                    prefix_empty = false;
                    if text == "--isolated" && !shell_owned {
                        plan.isolated = true;
                    } else {
                        plan.args.push(arg.clone());
                    }
                }
                idx += 1;
            }
            "--context" => {
                if shell_arguments {
                    return Invocation::UsageError(
                        "--context cannot be combined with shell arguments",
                    );
                }
                if let Err(error) =
                    super::parse_failure_context(&mut entry.launch_options, args.get(idx + 1))
                {
                    return Invocation::UsageError(error);
                }
                legacy = false;
                idx += 2;
            }
            "--ask" => {
                if shell_arguments {
                    return Invocation::UsageError("--ask cannot be combined with shell arguments");
                }
                if let Err(error) =
                    super::parse_initial_question(&mut entry.launch_options, args.get(idx + 1))
                {
                    return Invocation::UsageError(error);
                }
                legacy = false;
                idx += 2;
            }
            "--resume" => {
                if entry.launch_options.initial_question.is_some() {
                    return Invocation::UsageError("--ask cannot be combined with --resume");
                }
                legacy = false;
                idx += 1;
                entry.launch_options.resume = Some(match args.get(idx) {
                    Some(value) if !value.to_string_lossy().starts_with('-') => {
                        idx += 1;
                        ResumeLaunch::Session(value.to_string_lossy().into_owned())
                    }
                    _ => ResumeLaunch::Picker,
                });
            }
            text if text.starts_with("--resume=") => {
                if entry.launch_options.initial_question.is_some() {
                    return Invocation::UsageError("--ask cannot be combined with --resume");
                }
                legacy = false;
                let value = &text["--resume=".len()..];
                entry.launch_options.resume = Some(if value.is_empty() {
                    ResumeLaunch::Picker
                } else {
                    ResumeLaunch::Session(value.to_string())
                });
                idx += 1;
            }
            text => {
                if entry.launch_options.initial_question.is_some() {
                    return Invocation::UsageError("--ask cannot be combined with shell arguments");
                }
                shell_arguments |= text == "--"
                    || (text.starts_with('-') && !text.starts_with("--"))
                    || (!text.starts_with("--") && entry.adapter_name.is_some());
                if !text.starts_with("--") {
                    entry.adapter_name.get_or_insert_with(|| text.to_string());
                }
                if legacy {
                    if !text.starts_with('-') && !adapter_skipped {
                        adapter_skipped = true;
                    } else {
                        legacy = false;
                    }
                }
                idx += 1;
            }
        }
    }
    if entry.launch_options.failure_context.is_some()
        && entry.launch_options.initial_question.is_none()
    {
        return Invocation::UsageError("--context requires --ask");
    }
    if entry.launch_options.initial_question.is_some() && !all_tty {
        return Invocation::UsageError("--ask requires an interactive terminal");
    }
    Invocation::Tui(entry)
}

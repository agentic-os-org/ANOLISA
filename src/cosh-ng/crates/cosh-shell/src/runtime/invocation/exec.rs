use super::{entry_basename, login_argv0, ExecOrigin, ExecPlan, GatewayPlan};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;

/// Execute the inner-shell plan, preserving shell-owned argv and SIGPIPE state
/// outside the cosh-owned isolation contract. Isolated Bash execs suppress its
/// startup inputs and reject login shells (Bash cannot suppress logout files);
/// isolated Zsh execs use `-f` only with no shell arguments and
/// reject arguments before exec; unsupported targets retain `--isolated` and
/// fail loudly. Returns 2 for isolation rejection, or 127/126 when exec fails.
pub(crate) fn exec_shell(plan: ExecPlan) -> i32 {
    use std::os::unix::process::CommandExt;

    let shell = match choose_shell(
        &plan,
        || {
            std::env::var_os("COSH_SHELL_DEFAULT_SHELL")
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| OsString::from("bash"))
        },
        query_login_shell,
    ) {
        Ok(shell) => shell,
        Err(error) => {
            eprintln!("cosh: {error}");
            return 126;
        }
    };
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

fn choose_shell(
    plan: &ExecPlan,
    baseline: impl FnOnce() -> OsString,
    query: impl FnOnce() -> Result<Option<String>, String>,
) -> Result<OsString, String> {
    if let Some(shell) = &plan.shell_override {
        return Ok(shell.clone());
    }
    if plan.origin == ExecOrigin::Primary {
        if let Some(shell) = query()? {
            return Ok(shell.into());
        }
    }
    Ok(baseline())
}

fn query_login_shell() -> Result<Option<String>, String> {
    #[cfg(target_os = "linux")]
    {
        use nix::libc;

        let mut inherited: libc::sigaction = unsafe { std::mem::zeroed() };
        if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut inherited) } < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if inherited.sa_sigaction != libc::SIG_IGN && inherited.sa_flags & libc::SA_NOCLDWAIT == 0 {
            return crate::runtime::login::probe::query();
        }
        // The bounded account query owns a child; the exec target keeps the caller's disposition.
        let mut waitable = inherited;
        waitable.sa_sigaction = libc::SIG_DFL;
        waitable.sa_flags &= !libc::SA_NOCLDWAIT;
        if unsafe { libc::sigaction(libc::SIGCHLD, &waitable, std::ptr::null_mut()) } < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let result = crate::runtime::login::probe::query();
        if unsafe { libc::sigaction(libc::SIGCHLD, &inherited, std::ptr::null_mut()) } < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        result
    }
    #[cfg(not(target_os = "linux"))]
    crate::runtime::login::probe::query()
}

#[cfg(test)]
#[path = "exec/tests.rs"]
mod tests;

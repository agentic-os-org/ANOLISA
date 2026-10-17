//! Explicit login-shell management, separate from shell invocation and Agent sessions.

use clap::{Args, Subcommand};
use cosh_platform::{
    detect::Distro,
    login_shell::{self, Operation, Request},
};
use cosh_types::error::{CoshError, ErrorCode};
use std::{path::PathBuf, time::Instant};

#[derive(Args)]
pub struct AccessProbeArgs {
    #[arg(long)]
    user: String,
    #[arg(long)]
    uid: u32,
    #[arg(long)]
    gid: u32,
    #[arg(long)]
    shell: PathBuf,
}

pub fn run_access_probe(args: &AccessProbeArgs) -> i32 {
    // This early child-only path must stay ahead of tracing or worker startup.
    let meta = cosh_types::output::ResponseMeta {
        subsystem: "login-shell".into(),
        duration_ms: 0,
        distro: None,
        dry_run: true,
        warning: None,
    };
    match login_shell::probe_access(&args.user, args.uid, args.gid, &args.shell) {
        Ok(accessible) => crate::print_success(accessible, meta),
        Err(error) => crate::print_failure(cli_error(error), meta),
    }
}

#[derive(Args)]
pub struct LoginShellArgs {
    /// Absolute public cosh entry (defaults to cosh beside this cosh-cli).
    #[arg(long, global = true)]
    shell: Option<PathBuf>,
    #[command(subcommand)]
    action: LoginShellCommands,
}

#[derive(Subcommand)]
enum LoginShellCommands {
    /// Report executable, registration, and optional account state without writes.
    Status {
        /// Inspect this account's configured shell.
        #[arg(long)]
        user: Option<String>,
    },
    /// Register the executable as available; never changes account shells.
    Register(ChangeArgs),
    /// Remove registration unless an account still uses the entry.
    Unregister {
        #[command(flatten)]
        change: ChangeArgs,
        /// Keep registration while an executable provider remains at the entry.
        #[arg(long)]
        if_missing: bool,
    },
    /// Select the registered cosh entry for one explicit local account.
    Set(AccountArgs),
    /// Select an explicit registered replacement for one local account.
    Restore {
        #[command(flatten)]
        account: AccountArgs,
        /// Absolute replacement shell; no previous value is guessed.
        #[arg(long)]
        to: PathBuf,
    },
}

#[derive(Args)]
struct ChangeArgs {
    /// Validate and report the proposed change without writing anything.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct AccountArgs {
    /// Explicit local account; never inferred from sudo, HOME, or login state.
    #[arg(long)]
    user: String,
    /// Expected current shell; refuses pre-existing administrator changes.
    #[arg(long)]
    expect_shell: PathBuf,
    #[command(flatten)]
    change: ChangeArgs,
}

pub fn run(args: LoginShellArgs, distro: &Distro, start: Instant) -> i32 {
    let mut request = Request {
        operation: Operation::Status,
        shell: PathBuf::new(),
        user: None,
        expect_shell: None,
        restore_shell: None,
        dry_run: false,
        if_missing: false,
    };
    let account = match args.action {
        LoginShellCommands::Status { user } => {
            request.user = user;
            None
        }
        LoginShellCommands::Register(change) => {
            request.operation = Operation::Register;
            request.dry_run = change.dry_run;
            None
        }
        LoginShellCommands::Unregister { change, if_missing } => {
            request.operation = Operation::Unregister;
            request.dry_run = change.dry_run;
            request.if_missing = if_missing;
            None
        }
        LoginShellCommands::Set(account) => {
            request.operation = Operation::Set;
            Some(account)
        }
        LoginShellCommands::Restore { account, to } => {
            request.operation = Operation::Restore;
            request.restore_shell = Some(to);
            Some(account)
        }
    };
    if let Some(account) = account {
        request.user = Some(account.user);
        request.expect_shell = Some(account.expect_shell);
        request.dry_run = account.change.dry_run;
    }
    let meta = crate::build_meta("login-shell", distro, start, request.dry_run);
    let result = std::env::current_exe()
        .map_err(login_shell::Error::from)
        .and_then(|cli| {
            request.shell = login_shell::entry_path(&cli, args.shell.as_deref())?;
            login_shell::execute(&request, &cli)
        });
    match result {
        Ok(report) => crate::print_success(report, meta),
        Err(error) => crate::print_failure(cli_error(error), meta),
    }
}

fn cli_error(error: login_shell::Error) -> CoshError {
    let code = match &error {
        login_shell::Error::Permission => ErrorCode::PermissionDenied,
        login_shell::Error::AccessDenied(_) => ErrorCode::LoginShellAccessDenied,
        login_shell::Error::Invalid(_) => ErrorCode::InvalidInput,
        login_shell::Error::Unsupported => ErrorCode::UnsupportedPlatform,
        login_shell::Error::Conflict(_) => ErrorCode::LoginShellConflict,
        login_shell::Error::Backend(_) => ErrorCode::LoginShellBackendError,
        login_shell::Error::Io(error) => match error.kind() {
            std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
            std::io::ErrorKind::NotFound => ErrorCode::NotFound,
            std::io::ErrorKind::TimedOut => ErrorCode::Timeout,
            _ => ErrorCode::LoginShellIoError,
        },
    };
    CoshError::new(code, error.to_string(), "login-shell")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_shell_errors_preserve_machine_readable_categories() {
        use login_shell::Error;
        use std::io::ErrorKind;
        for (error, code) in [
            (Error::Unsupported, "UnsupportedPlatform"),
            (Error::Permission, "PermissionDenied"),
            (Error::Invalid("relative path".into()), "InvalidInput"),
            (
                Error::AccessDenied("alice".into()),
                "LoginShellAccessDenied",
            ),
            (Error::Conflict("in use".into()), "LoginShellConflict"),
            (
                Error::Backend("usermod failed".into()),
                "LoginShellBackendError",
            ),
            (
                Error::Io(ErrorKind::PermissionDenied.into()),
                "PermissionDenied",
            ),
            (Error::Io(ErrorKind::NotFound.into()), "NotFound"),
            (Error::Io(ErrorKind::TimedOut.into()), "Timeout"),
            (Error::Io(ErrorKind::Other.into()), "LoginShellIoError"),
        ] {
            let message = error.to_string();
            let json = serde_json::to_value(cli_error(error)).unwrap();
            assert_eq!(json["code"], code);
            assert_eq!(json["message"], message);
            assert_eq!(json["recoverable"], false);
        }
    }
}

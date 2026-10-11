//! Bounded account-tool adapter and orchestration, never an account database writer.

use super::{storage, validate_path, Error, Operation, Request};
use cosh_types::login_shell::LoginShellReport;
use cosh_types::output::CoshResponse;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Account {
    pub(super) name: String,
    pub(super) uid: u32,
    pub(super) gid: u32,
    pub(super) shell: PathBuf,
}

pub(super) trait Accounts {
    fn all(&self) -> Result<Vec<Account>, Error>;
    fn local(&self, user: &str) -> Result<Account, Error>;
    fn set_shell(&self, user: &str, shell: &Path) -> Result<(), Error>;
    fn check_access(&self, account: &Account, shell: &Path) -> Result<(), Error>;
}

struct SystemAccounts<'a> {
    cli: &'a Path,
}

fn output(command: &mut Command) -> Result<Vec<u8>, Error> {
    let result = crate::run_command(command, Duration::from_secs(15), "login-shell")
        .map_err(|error| Error::Backend(error.to_string()))?;
    if !result.status.success() {
        return Err(Error::Backend(format!(
            "command exited {}: {}",
            result.status,
            String::from_utf8_lossy(&result.stderr).trim()
        )));
    }
    Ok(result.stdout)
}

fn parse_accounts(bytes: &[u8]) -> Result<Vec<Account>, Error> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| Error::Backend("getent returned non-UTF-8 account data".into()))?;
    text.lines()
        .map(|line| {
            let fields: Vec<_> = line.split(':').collect();
            if fields.len() != 7 || fields[0].is_empty() {
                return Err(Error::Backend(
                    "getent returned malformed passwd data".into(),
                ));
            }
            Ok(Account {
                name: fields[0].into(),
                uid: fields[2]
                    .parse()
                    .map_err(|_| Error::Backend("getent returned invalid UID".into()))?,
                gid: fields[3]
                    .parse()
                    .map_err(|_| Error::Backend("getent returned invalid GID".into()))?,
                shell: PathBuf::from(fields[6]),
            })
        })
        .collect()
}

impl Accounts for SystemAccounts<'_> {
    fn check_access(&self, account: &Account, shell: &Path) -> Result<(), Error> {
        let result = crate::run_command(
            Command::new(self.cli)
                .env_clear()
                .arg("__login-shell-access")
                .args(["--user", &account.name, "--uid", &account.uid.to_string()])
                .args(["--gid", &account.gid.to_string()])
                .arg("--shell")
                .arg(shell),
            Duration::from_secs(15),
            "login-shell",
        )
        .map_err(|error| Error::Backend(error.to_string()))?;
        let response: CoshResponse<bool> = serde_json::from_slice(&result.stdout)
            .map_err(|error| Error::Backend(format!("invalid account access probe: {error}")))?;
        match (result.status.success(), response.ok, response.data) {
            (true, true, Some(true)) => Ok(()),
            (true, true, Some(false)) => Err(Error::AccessDenied(format!(
                "{} cannot execute {}",
                account.name,
                shell.display()
            ))),
            _ => Err(Error::Backend(format!(
                "account access probe failed: {}",
                response
                    .error
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| format!("exit {} without a result", result.status))
            ))),
        }
    }

    fn all(&self) -> Result<Vec<Account>, Error> {
        let accounts = parse_accounts(&output(Command::new("/usr/bin/getent").arg("passwd"))?)?;
        if accounts.is_empty() {
            return Err(Error::Backend(
                "getent returned no accounts; cannot establish in-use state".into(),
            ));
        }
        Ok(accounts)
    }

    fn local(&self, user: &str) -> Result<Account, Error> {
        let accounts = parse_accounts(&output(
            Command::new("/usr/bin/getent").args(["-s", "files", "passwd", user]),
        )?)?;
        match accounts.as_slice() {
            [account] if account.name == user => Ok(account.clone()),
            _ => Err(Error::Backend(format!(
                "expected exactly one local account for {user}"
            ))),
        }
    }

    fn set_shell(&self, user: &str, shell: &Path) -> Result<(), Error> {
        output(
            Command::new("/usr/sbin/usermod")
                .arg("--shell")
                .arg(shell)
                .arg("--")
                .arg(user),
        )?;
        Ok(())
    }
}

pub(super) fn execute(request: &Request, cli: &Path) -> Result<LoginShellReport, Error> {
    run(
        request,
        Path::new("/etc/shells"),
        Path::new("/etc/.cosh-login-shell.lock"),
        nix::unistd::geteuid().is_root(),
        &SystemAccounts { cli },
    )
}

fn validate(request: &Request) -> Result<(), Error> {
    validate_path(&request.shell)?;
    // passwd permits an empty shell field. Preserve its exact value for the
    // expectation guard; destinations still require absolute executable paths.
    if let Some(path) = &request.expect_shell {
        if !path.as_os_str().is_empty() {
            validate_path(path)?;
        }
    }
    if let Some(path) = &request.restore_shell {
        validate_path(path)?;
    }
    if let Some(user) = &request.user {
        if user.is_empty()
            || user.starts_with('-')
            || !user
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
        {
            return Err(Error::Invalid(
                "user must be an explicit local account name".into(),
            ));
        }
    }
    let account_change = matches!(request.operation, Operation::Set | Operation::Restore);
    if account_change && (request.user.is_none() || request.expect_shell.is_none()) {
        return Err(Error::Invalid(
            "set and restore require --user and --expect-shell".into(),
        ));
    }
    if (request.operation == Operation::Restore) != request.restore_shell.is_some() {
        return Err(Error::Invalid(
            "only restore requires a replacement shell".into(),
        ));
    }
    if !account_change && request.expect_shell.is_some() {
        return Err(Error::Invalid(
            "--expect-shell is only valid for set and restore".into(),
        ));
    }
    if request.if_missing && request.operation != Operation::Unregister {
        return Err(Error::Invalid(
            "--if-missing is only valid for unregister".into(),
        ));
    }
    Ok(())
}

fn executable(path: &Path) -> Result<bool, Error> {
    match fs::metadata(path) {
        Ok(meta) => Ok(meta.is_file() && meta.permissions().mode() & 0o111 != 0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn run(
    request: &Request,
    shells: &Path,
    lock: &Path,
    root: bool,
    accounts: &impl Accounts,
) -> Result<LoginShellReport, Error> {
    validate(request)?;
    let mutation = request.operation != Operation::Status && !request.dry_run;
    if mutation && !root {
        return Err(Error::Permission);
    }
    let _lock = if mutation {
        Some(storage::lock(lock)?)
    } else {
        None
    };
    let table = storage::Table::read(shells)?;
    let all = accounts.all()?;
    let account = request
        .user
        .as_ref()
        .map(|user| {
            let matching: Vec<_> = all.iter().filter(|account| &account.name == user).collect();
            match matching.as_slice() {
                [account] => Ok((*account).clone()),
                [] => Err(Error::Invalid(format!("account not found: {user}"))),
                _ => Err(Error::Conflict(format!(
                    "NSS returned multiple accounts named {user}"
                ))),
            }
        })
        .transpose()?;
    let installed = executable(&request.shell)?;
    let mut report = LoginShellReport {
        operation: request.operation.name().into(),
        shell: request.shell.clone(),
        resolved_executable: if installed {
            Some(fs::canonicalize(&request.shell)?)
        } else {
            None
        },
        executable: installed,
        registered: table.contains(&request.shell),
        user: request.user.clone(),
        account_shell: account.as_ref().map(|a| a.shell.clone()),
        using_accounts: all
            .iter()
            .filter(|a| a.shell == request.shell)
            .map(|a| a.name.clone())
            .collect(),
        changed: false,
        previous_shell: None,
        retained_provider: false,
        account_access_checked: false,
    };
    match request.operation {
        Operation::Status => {}
        Operation::Register | Operation::Unregister => {
            if request.operation == Operation::Register && !installed {
                return Err(Error::Invalid(
                    "registration requires an existing executable shell entry".into(),
                ));
            }
            if request.operation == Operation::Unregister {
                if request.if_missing && installed {
                    report.retained_provider = true;
                    return Ok(report);
                }
                if !report.using_accounts.is_empty() {
                    return Err(Error::Conflict(format!(
                        "shell is still configured for accounts: {}",
                        report.using_accounts.join(", ")
                    )));
                }
            }
            let bytes = table.changed(&request.shell, request.operation == Operation::Register);
            report.changed = bytes != table.bytes;
            if mutation && report.changed {
                table.replace(&bytes)?;
                report.registered = request.operation == Operation::Register;
            }
        }
        Operation::Set | Operation::Restore => {
            let user = request
                .user
                .as_deref()
                .ok_or_else(|| Error::Invalid("missing user".into()))?;
            let expected = request
                .expect_shell
                .as_ref()
                .ok_or_else(|| Error::Invalid("missing expected shell".into()))?;
            let target = request.restore_shell.as_ref().unwrap_or(&request.shell);
            if !executable(target)? || !table.contains(target) {
                return Err(Error::Invalid(
                    "account target must be an executable registered in /etc/shells".into(),
                ));
            }
            let local = accounts.local(user)?;
            if account.as_ref().is_none_or(|a| a != &local) {
                return Err(Error::Conflict(
                    "NSS and local account state disagree".into(),
                ));
            }
            if root {
                accounts.check_access(&local, target)?;
                report.account_access_checked = true;
            }
            report.previous_shell = Some(local.shell.clone());
            if &local.shell == target {
                return Ok(report);
            }
            if &local.shell != expected {
                return Err(Error::Conflict(format!(
                    "{user} has shell {}, expected {}; account unchanged",
                    local.shell.display(),
                    expected.display()
                )));
            }
            report.changed = true;
            if mutation {
                // Recheck immediately before usermod. Its own account lock must
                // remain authoritative; holding it here would deadlock the tool.
                if accounts.local(user)? != local {
                    return Err(Error::Conflict(
                        "account changed before usermod; account unchanged".into(),
                    ));
                }
                accounts.set_shell(user, target)?;
                let final_account = accounts.local(user)?;
                if final_account.shell != *target
                    || final_account.uid != local.uid
                    || final_account.gid != local.gid
                {
                    return Err(Error::Conflict("account differs after usermod; inspect administrator changes before retrying".into()));
                }
                report.account_shell = Some(target.clone());
                report.using_accounts = accounts
                    .all()?
                    .into_iter()
                    .filter(|a| a.shell == request.shell)
                    .map(|a| a.name)
                    .collect();
            }
        }
    }
    Ok(report)
}

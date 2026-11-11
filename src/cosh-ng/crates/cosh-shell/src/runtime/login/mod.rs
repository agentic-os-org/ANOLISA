mod account;
mod capability;
mod cli;
pub(crate) mod editor;
pub(crate) mod invitation;
mod launch;
pub(crate) mod lifecycle;
mod native_lease;
mod record;

use std::fmt;

use account::{supported_bash, Account};
use record::{Phase, Record, Store};

pub(crate) use cli::run_cli;
pub(crate) use launch::{apply_login_shell_plan, login_shell_plan, probe};

pub(super) const COSH_PATH: &str = "/usr/bin/cosh";
type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Error {
    message: String,
    usage: bool,
}
impl Error {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            usage: false,
        }
    }
    fn usage() -> Self {
        Self {
            message: "usage: cosh login status|set|restore [--user USER] [--yes]".into(),
            usage: true,
        }
    }
    fn io(error: std::io::Error) -> Self {
        Self::new(format!("login management I/O failed: {error}"))
    }
}
impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Status,
    Set,
    Restore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Status {
    account: Account,
    original_shell: Option<String>,
    phase: Option<Phase>,
    conflict: bool,
}

fn status(account: Account, record: Option<&Record>) -> Result<Status> {
    if let Some(record) = record {
        record.validate(&account)?;
    }
    let conflict = match record {
        Some(record) => {
            record.phase == Phase::Conflict
                || (account.shell != record.original_shell && account.shell != record.cosh_shell)
                || (account.shell == record.cosh_shell && record.phase == Phase::Restored)
        }
        None => !supported_bash(&account.shell) && account.shell != COSH_PATH,
    };
    Ok(Status {
        original_shell: record.map(|r| r.original_shell.clone()),
        phase: record.map(|r| r.phase),
        account,
        conflict,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoginStatus {
    pub(crate) username: String,
    pub(crate) uid: u32,
    pub(crate) current_shell: String,
    pub(crate) original_bash: Option<String>,
    pub(crate) transaction: String,
    pub(crate) managed: bool,
    pub(crate) conflict: bool,
}

impl From<Status> for LoginStatus {
    fn from(status: Status) -> Self {
        Self {
            managed: !status.conflict
                && status.account.shell == COSH_PATH
                && status.original_shell.is_some(),
            username: status.account.name,
            uid: status.account.uid,
            current_shell: status.account.shell,
            original_bash: status.original_shell,
            transaction: if status.conflict {
                "conflict".into()
            } else {
                status
                    .phase
                    .map(|phase| format!("{phase:?}").to_lowercase())
                    .unwrap_or_else(|| "unmanaged".into())
            },
            conflict: status.conflict,
        }
    }
}

pub(crate) fn query_status(user: Option<&str>) -> std::result::Result<LoginStatus, String> {
    (|| {
        let euid = unsafe { nix::libc::geteuid() };
        let account = account::lookup(user, euid)?;
        if euid != 0 && account.uid != euid {
            return Err(Error::new(
                "administrator privileges are required to inspect another account",
            ));
        }
        let record = Store::system().read(account.uid)?;
        let snapshot = status(account, record.as_ref())?;
        if let Some(original) = &snapshot.original_shell {
            account::verify_bash(original)?;
        }
        Ok(LoginStatus::from(snapshot))
    })()
    .map_err(|error: Error| error.to_string())
}

fn transact(
    action: Action,
    expected: &Account,
    original_sha256: &str,
    store: &Store,
    mut validate: impl FnMut() -> Result<()>,
    mut reload: impl FnMut() -> Result<Account>,
    mut change: impl FnMut(&str, &std::fs::File) -> Result<()>,
) -> Result<Status> {
    if action == Action::Status {
        let current = reload()?;
        ensure_identity(expected, &current)?;
        return status(current, store.read(expected.uid)?.as_ref());
    }
    let lock = store.lock()?;
    let account = reload()?;
    ensure_identity(expected, &account)?;
    let existing = store.read(account.uid)?;
    let snapshot = status(account.clone(), existing.as_ref())?;
    if snapshot.conflict {
        if let Some(mut record) = existing {
            record.phase = Phase::Conflict;
            store.write(&record)?;
        }
        return Err(Error::new(
            "login shell or transaction conflicts with administrator state; no change made",
        ));
    }
    let mut record = match existing {
        Some(record) => record,
        None if action == Action::Set && supported_bash(&account.shell) => Record {
            schema: 1,
            username: account.name.clone(),
            uid: account.uid,
            original_shell: account.shell.clone(),
            cosh_shell: COSH_PATH.into(),
            original_sha256: original_sha256.into(),
            phase: Phase::Prepared,
        },
        None => {
            return Err(Error::new(
                "no trusted login recovery record; administrator review is required",
            ))
        }
    };
    record.validate(&account)?;
    let previous_phase = record.phase;
    let target = if action == Action::Set {
        record.cosh_shell.clone()
    } else {
        record.original_shell.clone()
    };
    let final_phase = if action == Action::Set {
        Phase::Applied
    } else {
        Phase::Restored
    };
    if account.shell == target {
        record.phase = final_phase;
        store.write(&record)?;
        return status(account, Some(&record));
    }
    let expected_shell = if action == Action::Set {
        &record.original_shell
    } else {
        &record.cosh_shell
    };
    if &account.shell != expected_shell {
        return Err(Error::new(
            "current account shell no longer matches the managed transaction",
        ));
    }
    record.phase = Phase::Prepared;
    store.write(&record)?;
    validate()?;
    let before = reload()?;
    if ensure_identity(expected, &before).is_err() || before.shell != account.shell {
        record.phase = Phase::Conflict;
        store.write(&record)?;
        return Err(Error::new(
            "account changed before login transaction; no change made",
        ));
    }
    let changed = change(&target, &lock);
    let after = match reload() {
        Ok(after) => after,
        Err(_) => {
            record.phase = Phase::Conflict;
            store.write(&record)?;
            return Err(Error::new(
                "account update could not be verified; inspect login status before retrying",
            ));
        }
    };
    if after == before {
        if let Err(error) = &changed {
            record.phase = previous_phase;
            store.write(&record)?;
            return Err(error.clone());
        }
    }
    if ensure_identity(expected, &after).is_err() || after.shell != target {
        record.phase = Phase::Conflict;
        store.write(&record)?;
        return Err(Error::new(
            "account update could not be verified; inspect login status before retrying",
        ));
    }
    record.phase = final_phase;
    store.write(&record).map_err(|_| {
        Error::new(
            "account shell changed but transaction finalization failed; inspect login status",
        )
    })?;
    changed?;
    status(after, Some(&record))
}

fn ensure_identity(expected: &Account, current: &Account) -> Result<()> {
    if expected.name != current.name || expected.uid != current.uid {
        Err(Error::new(
            "account name or UID changed; no account change authorized",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;

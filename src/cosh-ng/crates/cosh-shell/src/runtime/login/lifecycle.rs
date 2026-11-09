use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

use super::{
    account::{self, Account},
    record::{Phase, Record},
    status, Error, Result, Store, COSH_PATH,
};

#[derive(Debug, PartialEq, Eq)]
enum Action {
    VerifyInstalled,
    CheckErase,
    Erase,
    CheckProtocol,
    CheckTarget(PathBuf),
    Replace(PathBuf),
    Downgrade(PathBuf),
    SwapToCopilot,
}

pub(crate) fn run_cli(args: &[OsString]) -> i32 {
    match parse(args).and_then(|action| run(action, unsafe { nix::libc::geteuid() })) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("cosh package: {error}");
            if error.usage {
                2
            } else {
                1
            }
        }
    }
}

fn usage() -> Error {
    Error { message: "usage: login-preflight --verify-installed --entry /usr/bin/cosh --record-version 1 | check-erase | check-protocol 1 --record-version 1 --entry /usr/bin/cosh | check-target|replace|downgrade ABSOLUTE_RPM | swap-to-copilot | erase".into(), usage: true }
}

fn parse(args: &[OsString]) -> Result<Action> {
    if args.len() > 6 || args.iter().any(|arg| arg.len() > 4096) {
        return Err(usage());
    }
    let args = args
        .iter()
        .map(|arg| arg.to_str().ok_or_else(usage))
        .collect::<Result<Vec<_>>>()?;
    match args.as_slice() {
        ["--verify-installed", "--entry", COSH_PATH, "--record-version", "1"] => {
            Ok(Action::VerifyInstalled)
        }
        ["check-erase"] => Ok(Action::CheckErase),
        ["erase"] => Ok(Action::Erase),
        ["check-protocol", "1", "--record-version", "1", "--entry", COSH_PATH] => {
            Ok(Action::CheckProtocol)
        }
        ["swap-to-copilot"] => Ok(Action::SwapToCopilot),
        [action @ ("check-target" | "replace" | "downgrade"), path] => {
            let path = PathBuf::from(path);
            if !path.is_absolute()
                || path.extension().is_none_or(|ext| ext != "rpm")
                || path.as_os_str().as_encoded_bytes().contains(&0)
                || path
                    .components()
                    .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
            {
                return Err(usage());
            }
            Ok(match *action {
                "check-target" => Action::CheckTarget(path),
                "replace" => Action::Replace(path),
                _ => Action::Downgrade(path),
            })
        }
        _ => Err(usage()),
    }
}

fn compatible_header(bytes: &[u8]) -> Result<bool> {
    if bytes.len() > 8192
        || !bytes.ends_with(b"\n")
        || bytes
            .iter()
            .any(|byte| !byte.is_ascii_graphic() && *byte != b'\n' && *byte != b' ')
    {
        return Err(Error::new("invalid target RPM capability header"));
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| Error::new("invalid target RPM header encoding"))?;
    let mut lines = text.lines();
    if lines.next() != Some("cosh-ng") {
        return Err(Error::new("target RPM must be cosh-ng"));
    }
    let version = lines
        .next()
        .filter(|version| !version.is_empty() && version.len() <= 256)
        .ok_or_else(|| Error::new("invalid target RPM version"))?;
    let version = semver::Version::parse(version).ok();
    let provides = lines.collect::<Vec<_>>();
    if provides.len() > 256 {
        return Err(Error::new("target RPM capability header exceeds limit"));
    }
    Ok(
        version.is_some_and(|version| version >= semver::Version::new(0, 27, 0))
            && provides.contains(&"cosh-login-lifecycle-v1"),
    )
}

fn admit(action: &Action, euid: u32) -> Result<()> {
    if euid != 0 && *action != Action::VerifyInstalled {
        return Err(Error::new(
            "package lifecycle operations require administrator privileges",
        ));
    }
    Ok(())
}

fn inherit_lock(lock: &File) -> Result<()> {
    let flags = unsafe { nix::libc::fcntl(lock.as_raw_fd(), nix::libc::F_GETFD) };
    if flags < 0
        || unsafe {
            nix::libc::fcntl(
                lock.as_raw_fd(),
                nix::libc::F_SETFD,
                flags & !nix::libc::FD_CLOEXEC,
            )
        } < 0
    {
        return Err(Error::io(std::io::Error::last_os_error()));
    }
    Ok(())
}

/// Login shells that resolve to the cosh entry points and disappear with the
/// package. `cosh-login` is the wrapper users log in through, and `/bin/cosh`
/// is the usrmerge path onto the same file as `/usr/bin/cosh`. Matching only
/// `COSH_PATH` let `rpm -e cosh-ng` erase the shell those users log in with.
/// Stripping one leading `/usr` folds every usrmerge pair onto one spelling
/// instead of enumerating paths.
fn is_cosh_login_shell(shell: &str) -> bool {
    let shell = shell.strip_prefix("/usr").unwrap_or(shell);
    matches!(shell, "/bin/cosh" | "/bin/cosh-login")
}

fn check_state(records: &[Record], accounts: &[Account], compatible: bool) -> Result<()> {
    for record in records {
        let mut matching = accounts
            .iter()
            .filter(|account| account.name == record.username || account.uid == record.uid);
        let account = matching.next().ok_or_else(|| {
            Error::new("login record has no current account; administrator review is required")
        })?;
        if matching.next().is_some() || status(account.clone(), Some(record))?.conflict {
            return Err(Error::new(
                "login record conflicts with the current account",
            ));
        }
        if !compatible && record.phase != Phase::Restored {
            return Err(Error::new(
                "restore managed login accounts before replacing this package",
            ));
        }
    }
    if !compatible
        && accounts
            .iter()
            .any(|account| is_cosh_login_shell(&account.shell))
    {
        return Err(Error::new(
            "restore accounts still using /usr/bin/cosh or /usr/bin/cosh-login before removing this package",
        ));
    }
    Ok(())
}

fn audit(store: &Store, compatible: bool) -> Result<Vec<Record>> {
    let records = store.records()?;
    let mut accounts = account::nss_accounts()?;
    for record in &records {
        let current = account::lookup(Some(&record.username), record.uid)?;
        record.validate(&current)?;
        if !accounts.contains(&current) {
            accounts.push(current);
        }
    }
    for local in account::local_accounts()?
        .into_iter()
        .filter(|account| is_cosh_login_shell(&account.shell))
    {
        let current = account::lookup(Some(&local.name), local.uid)?;
        if current != local {
            return Err(Error::new("account changed during package lifecycle audit"));
        }
        if !accounts.contains(&current) {
            accounts.push(current);
        }
    }
    check_state(&records, &accounts, compatible)?;
    Ok(records)
}

fn prepare_transaction(
    store: &Store,
    compatible: bool,
    audit: impl FnOnce(&Store, bool) -> Result<Vec<Record>>,
) -> Result<()> {
    let records = audit(store, compatible)?;
    if !compatible {
        store.clear_restored(&records)?;
    }
    Ok(())
}

fn require_erase_cleanup(records: &[Record]) -> Result<()> {
    if !records.is_empty() {
        return Err(Error::new(
            "completed login records remain; use /usr/libexec/cosh/login-preflight erase",
        ));
    }
    Ok(())
}

fn inspect_target(path: &Path) -> Result<(PathBuf, bool)> {
    let path = account::trusted_path(path, false)?;
    let mut child = account::command("/usr/bin/rpm")?
        .args([
            "-qp",
            "--qf",
            "%{NAME}\n%{VERSION}\n[%{PROVIDENAME}\n]",
            "--",
        ])
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(Error::io)?;
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .expect("piped RPM header")
        .take(8193)
        .read_to_end(&mut bytes);
    if read.is_err() || bytes.len() > 8192 {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Error::new(
            "failed to read bounded target RPM capability header",
        ));
    }
    if !child.wait().map_err(Error::io)?.success() {
        return Err(Error::new("target RPM capability query failed"));
    }
    Ok((path, compatible_header(&bytes)?))
}

fn run(action: Action, euid: u32) -> Result<()> {
    admit(&action, euid)?;
    let downgrade = matches!(action, Action::Downgrade(_));
    let store = Store::system();
    match action {
        Action::VerifyInstalled | Action::CheckProtocol => audit(&store, true).map(|_| ()),
        Action::CheckErase => require_erase_cleanup(&audit(&store, false)?),
        Action::CheckTarget(path) => {
            let (_, compatible) = inspect_target(&path)?;
            audit(&store, compatible).map(|_| ())
        }
        Action::Replace(path) | Action::Downgrade(path) => {
            let (path, compatible) = inspect_target(&path)?;
            let lock = store.lock()?;
            prepare_transaction(&store, compatible, audit)?;
            let mut command = account::command("/usr/bin/dnf")?;
            command
                .arg(if downgrade { "downgrade" } else { "install" })
                .arg("--allowerasing")
                .arg("--")
                .arg(path);
            inherit_lock(&lock)?;
            Err(Error::io(command.exec()))
        }
        Action::SwapToCopilot | Action::Erase => {
            let erase = action == Action::Erase;
            let lock = store.lock()?;
            prepare_transaction(&store, false, audit)?;
            let mut command = account::command("/usr/bin/dnf")?;
            if erase {
                command.args(["remove", "cosh-ng"]);
            } else {
                command.args(["swap", "cosh-ng", "copilot-shell"]);
            }
            inherit_lock(&lock)?;
            Err(Error::io(command.exec()))
        }
    }
}

#[cfg(test)]
mod tests;

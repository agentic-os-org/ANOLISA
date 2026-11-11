use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::process::Stdio;

use crate::config::{detect_language_from_env, parse_language_setting, resolve_language_setting};
use crate::i18n::{I18n, MessageId};

use super::{
    account, capability, native_lease::NativeLease, status, transact, Action, Error, Result, Store,
    COSH_PATH,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Options {
    pub action: Action,
    pub user: Option<String>,
    pub yes: bool,
    pub expected_uid: Option<u32>,
}

pub(super) fn parse(args: &[OsString]) -> Result<Options> {
    let action = match args.first().and_then(|a| a.to_str()) {
        Some("status") => Action::Status,
        Some("set") => Action::Set,
        Some("restore") => Action::Restore,
        _ => return Err(Error::usage()),
    };
    let mut options = Options {
        action,
        user: None,
        yes: false,
        expected_uid: None,
    };
    let mut args = args[1..].iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--user") if options.user.is_none() => {
                let user = args
                    .next()
                    .and_then(|a| a.to_str())
                    .filter(|s| account::valid_name(s))
                    .ok_or_else(Error::usage)?;
                options.user = Some(user.into());
            }
            Some("--yes") if !options.yes && action != Action::Status => options.yes = true,
            Some("--__expected-uid")
                if options.expected_uid.is_none() && action != Action::Status =>
            {
                options.expected_uid = Some(
                    args.next()
                        .and_then(|a| a.to_str())
                        .and_then(|s| s.parse().ok())
                        .ok_or_else(Error::usage)?,
                );
            }
            _ => return Err(Error::usage()),
        }
    }
    if options.expected_uid.is_some() && (!options.yes || options.user.is_none()) {
        return Err(Error::usage());
    }
    Ok(options)
}

pub(crate) fn run_cli(args: &[OsString]) -> i32 {
    let language = std::env::var("COSH_SHELL_LANG")
        .ok()
        .and_then(|value| parse_language_setting(&value))
        .map(resolve_language_setting)
        .unwrap_or_else(detect_language_from_env);
    let i18n = I18n::new(language);
    match parse(args).and_then(|options| run(options, i18n)) {
        Ok(code) => code,
        Err(error) => {
            if error.usage {
                eprintln!("{}", i18n.t(MessageId::LoginUsage));
                2
            } else {
                eprintln!(
                    "{}",
                    i18n.format(MessageId::LoginCommandFailed, &[("error", &error.message)])
                );
                1
            }
        }
    }
}

fn run(options: Options, i18n: I18n) -> Result<i32> {
    if options.action == Action::Status {
        let snapshot = super::query_status(options.user.as_deref()).map_err(Error::new)?;
        println!("{}", status_text(&snapshot, i18n));
        return Ok(i32::from(snapshot.conflict));
    }
    let euid = unsafe { nix::libc::geteuid() };
    let account = account::lookup(options.user.as_deref(), euid)?;
    if options
        .expected_uid
        .is_some_and(|uid| euid != 0 || uid != account.uid)
    {
        return Err(Error::new(i18n.t(MessageId::LoginIdentityMismatch)));
    }
    let store = Store::system();
    let record = store.read(account.uid)?;
    let snapshot = status(account.clone(), record.as_ref())?;
    if snapshot.conflict {
        return Err(Error::new(i18n.t(MessageId::LoginStateConflict)));
    }
    let bash = snapshot.original_shell.as_deref().unwrap_or(&account.shell);
    if options.action == Action::Restore && record.is_none() {
        return Err(Error::new(i18n.t(MessageId::LoginRecoveryMissing)));
    }
    let hash = account::verify_shells(options.action, bash)?;
    if options.action == Action::Set {
        capability::verify()?;
    }
    let target = if options.action == Action::Set {
        COSH_PATH
    } else {
        bash
    };
    if options.expected_uid.is_none() {
        println!(
            "{}",
            i18n.format(
                MessageId::LoginChangePreview,
                &[
                    ("user", &account.name),
                    ("uid", &account.uid.to_string()),
                    ("before", &account.shell),
                    ("after", target),
                ],
            )
        );
    }
    if !options.yes {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err(Error::usage());
        }
        print!("{}", i18n.t(MessageId::LoginConfirm));
        io::stdout().flush().map_err(Error::io)?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer).map_err(Error::io)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            return Ok(0);
        }
    }
    if euid != 0 {
        let mut sudo = account::command("/usr/bin/sudo")
            .map_err(|_| administrator_command(&account.name, options.action, i18n))?;
        let status = sudo
            .args([
                "--",
                COSH_PATH,
                "login",
                if options.action == Action::Set {
                    "set"
                } else {
                    "restore"
                },
                "--user",
                &account.name,
                "--yes",
                "--__expected-uid",
                &account.uid.to_string(),
            ])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|_| administrator_command(&account.name, options.action, i18n))?;
        if !status.success() {
            return Err(administrator_command(&account.name, options.action, i18n));
        }
        println!("{}", i18n.t(MessageId::LoginChangeComplete));
        return Ok(0);
    }
    let native_lease = if options.action == Action::Set {
        Some(NativeLease::acquire()?)
    } else {
        None
    };
    let name = account.name.clone();
    let result = transact(
        options.action,
        &account,
        &hash,
        &store,
        || {
            account::verify_shells(options.action, bash)?;
            if options.action == Action::Set {
                capability::verify()?;
            }
            account::verify_account_tools()?;
            Ok(())
        },
        || account::lookup(Some(&name), euid),
        |target, lock| super::editor::change_shell(&account, target, native_lease.as_ref(), lock),
    )?;
    if options.expected_uid.is_none() {
        println!(
            "{}",
            i18n.format(
                MessageId::LoginShellChanged,
                &[("shell", &result.account.shell)],
            )
        );
    }
    Ok(0)
}

fn administrator_command(user: &str, action: Action, i18n: I18n) -> Error {
    let command = format!(
        "/usr/bin/sudo -- {COSH_PATH} login {} --user '{user}' --yes",
        if action == Action::Set {
            "set"
        } else {
            "restore"
        }
    );
    Error::new(i18n.format(
        MessageId::LoginAuthorizationRequired,
        &[("command", &command)],
    ))
}

fn status_text(snapshot: &super::LoginStatus, i18n: I18n) -> String {
    let transaction = match snapshot.transaction.as_str() {
        "unmanaged" => i18n.t(MessageId::LoginTransactionUnmanaged),
        "prepared" => i18n.t(MessageId::LoginTransactionPrepared),
        "applied" => i18n.t(MessageId::LoginTransactionApplied),
        "restored" => i18n.t(MessageId::LoginTransactionRestored),
        "conflict" => i18n.t(MessageId::LoginTransactionConflict),
        value => value,
    };
    format!(
        "{} {} (UID {})\n{} {}\n{} {}\n{} {}\n{} {}\n{}",
        i18n.t(MessageId::LoginUser),
        snapshot.username,
        snapshot.uid,
        i18n.t(MessageId::LoginCurrentShell),
        snapshot.current_shell,
        i18n.t(MessageId::LoginOriginalBash),
        snapshot
            .original_bash
            .as_deref()
            .unwrap_or(i18n.t(MessageId::LoginNoManagedRecord)),
        i18n.t(MessageId::LoginTransaction),
        transaction,
        i18n.t(MessageId::LoginManaged),
        i18n.t(if snapshot.managed {
            MessageId::LoginManagedYes
        } else {
            MessageId::LoginManagedNo
        }),
        i18n.t(MessageId::LoginNewLoginsOnly),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Language;

    #[test]
    fn localized_status_preserves_paths_and_transaction_values() {
        let snapshot = super::super::LoginStatus {
            username: "alice".into(),
            uid: 1001,
            current_shell: "/custom/{managed}".into(),
            original_bash: Some("/bin/bash".into()),
            transaction: "conflict".into(),
            managed: false,
            conflict: true,
        };
        for (language, phase, managed) in [
            (Language::EnUs, "Transaction: conflict", "Managed: no"),
            (Language::ZhCn, "事务状态： 冲突", "受管状态： 否"),
        ] {
            let text = status_text(&snapshot, I18n::new(language));
            for value in [
                "alice",
                "1001",
                "/custom/{managed}",
                "/bin/bash",
                phase,
                managed,
            ] {
                assert!(text.contains(value), "{text}");
            }
        }
        assert_eq!(snapshot.transaction, "conflict");
    }

    #[test]
    fn localized_authorization_keeps_the_same_explicit_command() {
        for language in [Language::EnUs, Language::ZhCn] {
            for (action, verb) in [(Action::Set, "set"), (Action::Restore, "restore")] {
                let error = administrator_command("alice$", action, I18n::new(language));
                assert!(error.message.contains(&format!(
                    "/usr/bin/sudo -- /usr/bin/cosh login {verb} --user 'alice$' --yes"
                )));
                assert!(!error.usage);
            }
        }
    }
}

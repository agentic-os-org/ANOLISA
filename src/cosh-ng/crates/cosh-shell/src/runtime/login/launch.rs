pub(crate) mod probe;

use super::{account, ensure_identity, status, Error, Record, Result, Store, COSH_PATH};

pub(crate) struct LoginShellPlan {
    pub(crate) bash_path: String,
    pub(crate) warning: Option<String>,
}

pub(crate) fn apply_login_shell_plan(
    entry: &mut crate::runtime::invocation::TuiEntry,
    plan: LoginShellPlan,
) -> Option<String> {
    entry.shell_kind = Some(crate::runtime::cli_args::RawShellKind::Bash);
    entry.launch_options.trusted_login_bash = Some(plan.bash_path);
    plan.warning
}

pub(crate) fn login_shell_plan(
    login_argv0: bool,
) -> std::result::Result<Option<LoginShellPlan>, String> {
    if !login_argv0 || !cfg!(target_os = "linux") {
        return Ok(None);
    }
    (|| {
        let euid = unsafe { nix::libc::geteuid() };
        let current = account::lookup_nss(None, euid)?;
        if current.shell != COSH_PATH {
            return Ok(None);
        }
        let record = account::lookup(Some(&current.name), euid).and_then(|checked| {
            ensure_identity(&current, &checked)?;
            Store::system().read(current.uid)
        });
        select_login_bash(&current, record, |path| {
            account::verify_bash(path).map(|_| ())
        })
        .map(Some)
    })()
    .map_err(|error: Error| error.to_string())
}

pub(super) fn select_login_bash(
    account: &account::Account,
    record: Result<Option<Record>>,
    mut verify: impl FnMut(&str) -> Result<()>,
) -> Result<LoginShellPlan> {
    let mut warning = None;
    match record {
        Ok(Some(record)) => match status(account.clone(), Some(&record)) {
            Ok(snapshot) if !snapshot.conflict => {
                if verify(&record.original_shell).is_ok() {
                    return Ok(LoginShellPlan { bash_path: record.original_shell, warning: None });
                }
                warning = Some("recorded Bash is no longer trusted; using verified system Bash. Login management requires administrator review.".into());
            }
            _ => warning = Some("login recovery record conflicts with the account; ignoring its path and using verified system Bash. Login management requires administrator review.".into()),
        },
        Ok(None) => {}
        Err(error) => warning = Some(format!("login recovery record is unavailable or unsafe ({error}); ignoring its path and using verified system Bash. Login management requires administrator review.")),
    }
    for path in ["/bin/bash", "/usr/bin/bash"] {
        if verify(path).is_ok() {
            return Ok(LoginShellPlan {
                bash_path: path.into(),
                warning,
            });
        }
    }
    Err(Error::new(
        "no verified system Bash is available for this login; administrator repair is required",
    ))
}

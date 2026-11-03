//! Read installed-package records independently of runtime configuration and services.

use aw_package::database::Inventory;
use clap::Args;
use std::path::{Path, PathBuf};

#[derive(Args, Debug, PartialEq)]
pub(super) struct List {
    /// Read installed records; remote package discovery is not supported
    #[arg(long, required = true)]
    installed: bool,
    #[command(flatten)]
    pub query: Query,
}

#[derive(Args, Debug, PartialEq)]
pub(super) struct Query {
    /// Select an absolute registered AW Provider installation directory
    #[arg(long, value_name = "ABS_DIR")]
    prefix: Option<PathBuf>,
    /// Package database scope (default: user)
    #[arg(long, value_enum)]
    scope: Option<super::scope::Scope>,
    #[command(flatten)]
    output: crate::output::Options,
}

fn read(
    prefix: Option<&Path>,
    scope: Option<super::scope::Scope>,
) -> aw_package::Result<Inventory> {
    let paths = super::scope::paths(scope)?;
    let resolved = match prefix {
        Some(prefix) => paths.for_prefix(prefix)?,
        None => paths.clone(),
    };
    if scope.is_some() && resolved.scope() != paths.scope() {
        return Err(aw_package::Error::Invalid(
            "explicit --scope conflicts with the prefix registration".into(),
        ));
    }
    Inventory::read(&resolved, prefix)
}

pub(super) fn run(command: &'static str, query: Query) -> crate::cli::Result<crate::cli::Exit> {
    let result = read(query.prefix.as_deref(), query.scope).and_then(|inventory| {
        if command == "doctor" {
            Ok(crate::output::Outcome::PackageFilesChecked {
                report: inventory.doctor()?,
            })
        } else {
            Ok(crate::output::Outcome::PackagesListed { inventory })
        }
    });
    crate::output::complete(command, query.output, result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aw_package::database::{DatabasePaths, RegistrationPointer};

    #[test]
    fn explicit_user_scope_still_rejects_a_recorded_system_prefix_before_reading_its_database() {
        let temporary = tempfile::tempdir().unwrap();
        let prefix = temporary.path().canonicalize().unwrap();
        let records = prefix.join(".local-packages");
        std::fs::create_dir(&records).unwrap();
        let pointer =
            RegistrationPointer::new(&DatabasePaths::system(), "system-installation".into())
                .unwrap();
        std::fs::write(
            records.join("installation.json"),
            serde_json::to_vec(&pointer).unwrap(),
        )
        .unwrap();
        let error = read(Some(&prefix), Some(super::super::scope::Scope::User)).unwrap_err();
        assert!(matches!(error, aw_package::Error::Invalid(message)
            if message == "explicit --scope conflicts with the prefix registration"));
    }
}

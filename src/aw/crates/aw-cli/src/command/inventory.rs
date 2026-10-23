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
    /// Select an absolute installation directory, including an older .aw-packages layout
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

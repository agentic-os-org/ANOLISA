//! Read installed-package records independently of runtime configuration and services.

use aw_package::database::{DatabasePaths, Inventory};
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
    #[command(flatten)]
    output: crate::output::Options,
}

fn read(prefix: Option<&Path>) -> aw_package::Result<Inventory> {
    let state_home = std::env::var_os("XDG_STATE_HOME");
    let home = std::env::var_os("HOME");
    let paths = DatabasePaths::user(
        state_home.as_deref().map(Path::new),
        home.as_deref().map(Path::new),
    )?;
    Inventory::read(&paths, prefix)
}

pub(super) fn run(command: &'static str, query: Query) -> crate::cli::Result<crate::cli::Exit> {
    let result = read(query.prefix.as_deref()).and_then(|inventory| {
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

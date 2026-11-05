//! Select an explicit package database without inferring scope from privileges.

use aw_package::database::DatabasePaths;
use std::path::Path;

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq)]
pub(super) enum Scope {
    User,
    System,
}

pub(super) fn paths(scope: Option<Scope>) -> aw_package::Result<DatabasePaths> {
    if scope == Some(Scope::System) {
        return Ok(DatabasePaths::system());
    }
    let state = std::env::var_os("XDG_STATE_HOME");
    let home = std::env::var_os("HOME");
    DatabasePaths::user(
        state.as_deref().map(Path::new),
        home.as_deref().map(Path::new),
    )
}

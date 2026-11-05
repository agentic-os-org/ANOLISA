//! AW extension installation and offline framework configuration.
//! Provider admission wraps a private file ownership and transaction engine.

pub mod config_edit;
mod configuration;
pub mod database;
mod filesystem;
pub mod plan;
#[cfg(test)]
mod provider_tests;
mod store;
pub mod transaction;

pub use crate::store::description::ProviderDeclaration;
pub use crate::store::{
    digest, Error, MutationFailure, MutationNotice, MutationReport, MutationResult, NoticeCode,
    Payload, RecoveryState, Result,
};
pub use configuration::{configure, document, Policy, Settings};

fn require_aw_database(paths: &database::DatabasePaths) -> Result<()> {
    require(
        paths.manager() == database::PackageManager::Aw,
        "AW extension operations require an AW-owned package database",
    )
}

pub(crate) fn require(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::Invalid(message.into()))
    }
}

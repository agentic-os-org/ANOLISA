//! Native Preview package contracts, installation and selected-Agent configuration.
//! Configuration and mutable runtime state remain outside immutable prefixes.

pub mod config_edit;
mod configuration;
pub mod database;
pub mod description;
mod filesystem;
mod install;
mod manifest;
pub mod open_bundle;
pub mod package;
pub mod plan;
pub mod transaction;

pub use configuration::{configure, document, Policy, Settings};
pub use install::{
    install, install_cancellable, install_reported, uninstall, uninstall_cancellable,
    uninstall_reported, MutationFailure, MutationNotice, MutationReport, MutationResult,
    NoticeCode, RecoveryState,
};
pub use manifest::{digest, inspect, Component, Manifest, Payload};

/// Packaging failure; cleanup diagnostics never replace the original error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A running AW instance or another writer still holds the target package lock.
    #[error("package installation is in use: {}", .0.display())]
    InUse(std::path::PathBuf),
    /// An unfinished database transaction requires an explicit recovery operation.
    #[error("package database needs recovery: {}", .0.display())]
    NeedsRecovery(std::path::PathBuf),
    /// The caller's cancellation was observed before the operation committed.
    #[error("package operation interrupted")]
    Cancelled,
    /// Filesystem or process operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// A filesystem operation failed at a known package path.
    #[error("{operation} at {}: {source}", path.display())]
    Filesystem {
        /// Operation whose failure prevented safe package publication.
        operation: &'static str,
        /// Directory or file supplied to the operation.
        path: std::path::PathBuf,
        /// Original operating-system error, including its raw error number.
        #[source]
        source: std::io::Error,
    },
    /// Manifest decoding or encoding failed.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Configuration encoding failed.
    #[error(transparent)]
    Yaml(#[from] serde_yaml_ng::Error),
    /// The requested operation violates the Preview contract.
    #[error("{0}")]
    Invalid(String),
}

/// Result shared by native package operations.
pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn require(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::Invalid(message.into()))
    }
}

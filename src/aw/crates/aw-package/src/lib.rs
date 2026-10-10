//! Native Preview package contracts, installation and selected-Agent configuration.
//! Configuration and mutable runtime state remain outside immutable prefixes.

mod configuration;
mod filesystem;
mod install;
mod manifest;
pub mod package;

pub use configuration::{configure, document, Policy, Settings};
pub use install::{install, install_cancellable, uninstall, uninstall_cancellable};
pub use manifest::{digest, inspect, Component, Manifest, Payload};

/// Packaging failure; cleanup diagnostics never replace the original error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Filesystem or process operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
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

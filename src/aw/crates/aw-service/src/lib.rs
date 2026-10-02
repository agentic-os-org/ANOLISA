//! Linux local service for prepared Providers, shared event lifetimes and durable metadata.
//! Native Adapters supply capabilities and retain scheduling and effect adoption.

mod audit;
mod client;
mod deadline;
mod endpoint;
mod ipc;
mod runtime;
mod server;
mod wire;

pub mod launch_service;

pub use client::Client;
pub use server::Server;
pub use wire::{
    Binding, Capabilities, EventHandle, Identity, Operation, Request, Response, VERSION,
};

/// Service boundary failure; remote messages contain only stable error codes.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Local transport, filesystem or operating-system failure.
    #[error("AW service I/O: {0}")]
    Io(#[from] std::io::Error),
    /// Static desired configuration was rejected before service startup.
    #[error(transparent)]
    Configuration(#[from] aw_config::Error),
    /// Client or server contract validation failed.
    #[error("AW service: {0}")]
    Rejected(String),
    /// A failed preparation has durable evidence even though no binding was returned.
    #[error("AW service: {code} (audit {audit_key})")]
    Attempt {
        /// Stable rejection code.
        code: String,
        /// Queryable durable attempt key.
        audit_key: String,
    },
}

pub(crate) type Result<T> = std::result::Result<T, Error>;

pub(crate) fn rejected(code: &str) -> Error {
    Error::Rejected(code.into())
}

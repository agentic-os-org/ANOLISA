//! Root-only credential store for the secret gateway.
//!
//! The store keeps real credential values out of every Policy, plan and
//! database surface. It issues one format-preserving fake per (credential,
//! Scope) pair, keeps tombstones for removed credentials so their old
//! fakes stay rejected, and materializes the selected mappings into a
//! gateway-readable snapshot.

mod error;
mod fake;
mod filesystem;
mod materialize;
mod names;
mod secret;
mod store;

pub use error::CredentialStoreError;
pub use fake::FakeFormat;
pub use materialize::{SECRETS_SNAPSHOT_VERSION, SecretsTarget};
pub use names::{CredentialId, ScopeId};
pub use secret::{MAX_SECRET_BYTES, Secret};
pub use store::{
    Carrier, CredentialSpec, CredentialState, CredentialStore, CredentialSummary, FakeSummary,
    MAX_ACTIVE_FAKES_PER_SCOPE, MAX_FAKES_TOTAL, STORE_FILE_NAME,
};

#[cfg(test)]
mod tests;

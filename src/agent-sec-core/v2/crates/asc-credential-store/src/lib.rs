//! Explicit real/fake pair storage for the secret gateway.
//!
//! Pairs share one fake across authorized Scopes. Last-Scope revocation drops
//! the real permanently and retains a rejection tombstone. Selected live pairs
//! are materialized into a versioned gateway snapshot without authentication metadata.

mod error;
mod fake;
mod filesystem;
mod materialize;
mod names;
mod secret;
mod store;

pub use error::CredentialStoreError;
pub use fake::{FakeFormat, generate_fake, validate_pair};
pub use filesystem::{read_token_file, write_fake_file};
pub use materialize::{SECRETS_SNAPSHOT_VERSION, SecretsTarget};
pub use names::{CredentialId, ScopeId};
pub use secret::{MAX_SECRET_BYTES, Secret};
pub use store::{
    CredentialState, CredentialStore, CredentialSummary, FakeSummary, MAX_ACTIVE_FAKES_PER_SCOPE,
    MAX_FAKES_TOTAL, PairInput, STORE_FILE_NAME,
};

#[cfg(test)]
mod tests;

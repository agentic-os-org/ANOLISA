//! Local package records, transactions and selected-Agent configuration.

pub mod config_edit;
mod configuration;
mod store;

pub(crate) use store::filesystem;
pub use store::{database, description, open_bundle, plan, transaction};

pub use configuration::{configure, document, Policy, Settings};
pub use store::{
    digest, Error, MutationFailure, MutationNotice, MutationReport, MutationResult, NoticeCode,
    Payload, RecoveryState, Result,
};

pub(crate) fn require(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::Invalid(message.into()))
    }
}

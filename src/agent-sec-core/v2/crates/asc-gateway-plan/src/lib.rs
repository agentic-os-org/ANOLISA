//! Scope Policy compiler and plan format consumed by the secret gateway.
//!
//! Plans contain credential identifiers, never real values or fakes. The gateway
//! resolves issued fakes through the separately materialized secrets snapshot.

mod compile;
mod credential;
mod error;
mod plan;

pub use compile::compile_scope;
pub use error::PlanError;
pub use plan::{
    CredentialPath, Method, PLAN_FORMAT, Plan, PlanFormat, PolicySource, RequestMatch, Rule,
    RuleSource, Scheme,
};

#[cfg(test)]
mod tests;

//! Plan format consumed by the secret gateway data plane, and the built-in
//! presets that produce it.
//!
//! A plan describes, for one Agent domain, which HTTPS requests may carry
//! which managed credential. It never contains real values or fakes: the
//! gateway resolves fakes through the separately materialized secrets
//! snapshot. The mitmproxy addon validates the same JSON shape, and both
//! sides share the fixtures under `fixtures/gateway`.

mod error;
mod plan;
mod preset;
mod template;

pub use error::PlanError;
pub use plan::{
    CredentialDefault, CredentialMatch, CredentialRef, DefaultEffect, Effect, Method, MethodMatch,
    PLAN_FORMAT, Phase, Plan, PlanFormat, PlanSource, RequestMatch, ResponseRewrite, Rule, Scheme,
    Subject,
};
pub use preset::{Preset, preset_plan};

#[cfg(test)]
mod tests;

//! Pure Binding-to-AgentSight/ActPlane translation.
//!
//! This crate translates a complete immutable `PreparedBinding` into a
//! deterministic target plan. It performs no persistence, HTTP, process
//! inspection, or policy attachment.
//!
//! Native JSON uses the existing DSL subset. Cedar PST lowers to structured
//! deletion, ordinary-write and file read protection. Target filesystem
//! and live capability checks belong to the Client and deployed backend.

#![forbid(unsafe_code)]

mod plan;
pub mod selection;
mod translate;

pub use plan::{
    ACTPLANE_POLICY_MEDIA_TYPE, AGENTSIGHT_BINDING_PLAN_FORMAT,
    AGENTSIGHT_BINDING_PLAN_SCHEMA_VERSION, AgentSightBindingPlan, AgentSightPolicyPlan,
    AgentSightScopePlan, AgentSightSourceBinding, FILE_PROTECTION_POLICY_MEDIA_TYPE,
};
pub use translate::AgentSightTranslator;

/// Checks a complete projected file view with the binding translator, without attaching a policy.
///
/// # Errors
/// Returns the binding translator's rejection when the view cannot be lowered to file protection.
pub fn validate_file_protection(
    files: &cedar_policy::pst::PolicySet,
) -> Result<(), asc_policy_types::target::TranslationRejection> {
    asc_policy_cedar::file_protection::translate_pst(files, translate::cedar_target).map(|_| ())
}

//! Pure Binding-to-AgentSight/ActPlane translation.
//!
//! This crate translates a complete immutable `PreparedBinding` into a
//! deterministic target plan. It performs no persistence, HTTP, process
//! inspection, or policy attachment.
//!
//! Native JSON uses the existing DSL subset. Cedar PST lowers to structured
//! deletion/ordinary-write protection. Target filesystem and live capability
//! checks belong to the Client and deployed backend.

#![forbid(unsafe_code)]

mod cedar;
mod plan;
mod translate;

pub use plan::{
    ACTPLANE_POLICY_MEDIA_TYPE, AGENTSIGHT_BINDING_PLAN_FORMAT,
    AGENTSIGHT_BINDING_PLAN_SCHEMA_VERSION, AgentSightBindingPlan, AgentSightPolicyPlan,
    AgentSightScopePlan, AgentSightSourceBinding, FILE_PROTECTION_POLICY_MEDIA_TYPE,
};
pub use translate::AgentSightTranslator;

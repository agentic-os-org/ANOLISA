use asc_policy_types::authoring::TemplateEnvelope;
use asc_policy_types::error::ValidationError;
use asc_policy_types::policy::PolicyEnvelope;

/// Synchronous authoring-template to Canonical Policy IR boundary used by PAP.
pub trait PolicyCompiler: Send + Sync {
    /// Lowers one immutable authored template revision.
    ///
    /// # Errors
    /// Returns a path-addressed semantic validation failure when the template
    /// cannot be represented by the selected Canonical Policy IR profile.
    fn lower(&self, template: &TemplateEnvelope) -> Result<PolicyEnvelope, ValidationError>;
}

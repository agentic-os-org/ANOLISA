use asc_foundation_types::{ResourceId, Revision};
pub use asc_pap::EnqueueError;
use asc_pap::{Page, PapError, PapRepository, PapService};
use asc_policy_types::authoring::PolicyTemplate;
use asc_policy_types::binding::BindingView;
use asc_policy_types::error::ValidationError;
use asc_policy_types::policy::PreparedPolicy;
use asc_policy_types::scope::{PolicyReference, PreparedScope, ScopeDeletion, ScopeSelector};

use crate::{Principal, PrincipalRole};

/// Stable authored-input detail that is safe to expose outside the daemon.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct PolicyInputError {
    message: String,
}

impl PolicyInputError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Stable resource class used to describe a PAP not-found failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NotFoundResource {
    /// A Policy identity requested for update does not exist.
    #[error("policy was not found")]
    Policy,
    /// An exact Policy revision requested for read or delete does not exist.
    #[error("policy revision was not found")]
    PolicyRevision,
    /// A Scope identity requested for update does not exist.
    #[error("scope was not found")]
    Scope,
    /// A Binding identity does not exist.
    #[error("binding was not found")]
    Binding,
    /// A Binding's referenced Policy revision does not exist.
    #[error("referenced policy revision was not found")]
    ReferencedPolicyRevision,
}

/// Stable PAP application failures safe for a daemon adapter to project.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PolicyAdministrationError {
    /// Saved intent rejected by scheduling with unconfirmed termination.
    #[error(
        "binding {id} revision {} was saved; {reason}; could not confirm request termination; background reconciliation may still run", .revision.get()
    )]
    SchedulingRejected {
        id: ResourceId,
        revision: Revision,
        reason: asc_pap::EnqueueError,
    },
    /// The server-assigned principal lacks Policy administration authority.
    #[error("principal is not authorized to administer policy")]
    Forbidden,
    /// Authored input failed domain validation.
    #[error("{0}")]
    InvalidArgument(PolicyInputError),
    /// Current revision or lifecycle state conflicts with the request.
    #[error("policy request conflicts with current state")]
    Conflict,
    /// A changed Binding request cannot interrupt reconciliation work.
    #[error("binding reconciliation operation is in progress")]
    OperationInProgress,
    /// The requested typed resource does not exist.
    #[error("{0}")]
    NotFound(NotFoundResource),
    /// No further positive revision can be allocated.
    #[error("revision space is exhausted")]
    ResourceExhausted,
    /// Reconciliation cannot accept requests before intent is saved.
    #[error("reconciliation runtime is unavailable")]
    Unavailable,
    /// Serialization or persistence failed with details withheld.
    #[error("policy state could not be processed")]
    Internal,
}

fn project_pap_error(
    error: PapError,
    missing_resource: Option<NotFoundResource>,
) -> PolicyAdministrationError {
    match error {
        PapError::SchedulingRejected {
            id,
            revision,
            reason,
        } => PolicyAdministrationError::SchedulingRejected {
            id,
            revision,
            reason,
        },
        PapError::InvalidPolicyName(message) => PolicyAdministrationError::InvalidArgument(
            PolicyInputError::new(format!("invalid policy name: {message}")),
        ),
        PapError::InvalidPolicy(error) => {
            project_validation_error(&error, &["template"], "invalid policy")
        }
        PapError::InvalidScope(error) => project_validation_error(
            &error,
            &["selector", "pid", "cgroupId", "policyTemplates"],
            "invalid scope",
        ),
        PapError::InvalidPagination => PolicyAdministrationError::InvalidArgument(
            PolicyInputError::new("invalid pagination: limit must be between 1 and 1000"),
        ),
        PapError::Conflict => PolicyAdministrationError::Conflict,
        PapError::OperationInProgress => PolicyAdministrationError::OperationInProgress,
        PapError::NotFound => missing_resource.map_or(
            PolicyAdministrationError::Internal,
            PolicyAdministrationError::NotFound,
        ),
        PapError::ReferencedPolicyRevisionNotFound => {
            PolicyAdministrationError::NotFound(NotFoundResource::ReferencedPolicyRevision)
        }
        PapError::RevisionExhausted => PolicyAdministrationError::ResourceExhausted,
        PapError::Unavailable => PolicyAdministrationError::Unavailable,
        PapError::InvalidIdentifier(_) | PapError::InvalidBinding(_) | PapError::Persistence => {
            PolicyAdministrationError::Internal
        }
    }
}

fn project_validation_error(
    error: &ValidationError,
    authored_roots: &[&str],
    public_prefix: &str,
) -> PolicyAdministrationError {
    let is_authored_path = authored_roots.iter().any(|authored_root| {
        error
            .path
            .strip_prefix(authored_root)
            .is_some_and(|suffix| {
                suffix.is_empty() || suffix.starts_with('.') || suffix.starts_with('[')
            })
    });
    if is_authored_path {
        PolicyAdministrationError::InvalidArgument(PolicyInputError::new(format!(
            "{public_prefix}: {error}"
        )))
    } else {
        PolicyAdministrationError::Internal
    }
}

/// Bounded application query result with its total before pagination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourcePage<T> {
    /// Selected current records.
    pub items: Vec<T>,
    /// Total matching records before pagination.
    pub total: u64,
}

impl<T> From<Page<T>> for ResourcePage<T> {
    fn from(page: Page<T>) -> Self {
        Self {
            items: page.items,
            total: page.total,
        }
    }
}

/// Principal-aware, type-erased PAP application boundary used by daemon adapters.
///
/// The method set deliberately mirrors the authoritative [`PapService`] use
/// cases only to stop its repository/compiler generics at this boundary and to
/// apply server-owned authorization. Implementations must delegate domain
/// semantics to PAP rather than reimplementing them.
pub trait PolicyAdministration: Send + Sync {
    /// Creates one Policy with a server-generated identity.
    ///
    /// # Errors
    /// Returns authorization, validation, conflict, capacity, or internal failures.
    fn create_policy(
        &self,
        principal: &Principal,
        policy_name: &str,
        template: &PolicyTemplate,
    ) -> Result<PreparedPolicy, PolicyAdministrationError>;

    /// Updates one existing Policy identity.
    ///
    /// # Errors
    /// Returns authorization, validation, conflict, not-found, capacity, or internal failures.
    fn update_policy(
        &self,
        principal: &Principal,
        policy_id: &ResourceId,
        policy_name: &str,
        template: &PolicyTemplate,
    ) -> Result<PreparedPolicy, PolicyAdministrationError>;

    /// Reads one exact current Policy revision.
    ///
    /// # Errors
    /// Returns authorization, not-found, or internal failures.
    fn get_policy(
        &self,
        principal: &Principal,
        id: &ResourceId,
        revision: Revision,
    ) -> Result<PreparedPolicy, PolicyAdministrationError>;

    /// Lists current Policies.
    ///
    /// # Errors
    /// Returns authorization, pagination-validation, or internal failures.
    fn list_policies(
        &self,
        principal: &Principal,
        limit: u32,
        offset: u32,
    ) -> Result<ResourcePage<PreparedPolicy>, PolicyAdministrationError>;

    /// Deletes one exact current Policy revision.
    ///
    /// # Errors
    /// Returns authorization, conflict, not-found, or internal failures.
    fn delete_policy_revision(
        &self,
        principal: &Principal,
        id: &ResourceId,
        revision: Revision,
    ) -> Result<PreparedPolicy, PolicyAdministrationError>;

    /// Creates an assignment with exact policy references.
    /// # Errors
    /// Returns validation, admission, authorization or reference errors.
    fn create_scope_assignment(
        &self,
        principal: &Principal,
        selector: &ScopeSelector,
        policies: &[PolicyReference],
    ) -> Result<PreparedScope, PolicyAdministrationError>;

    /// Reads an immutable assignment and its lifecycle status.
    ///
    /// # Errors
    /// Returns authorization, not-found, or internal failures.
    fn get_scope(
        &self,
        principal: &Principal,
        id: &ResourceId,
    ) -> Result<PreparedScope, PolicyAdministrationError>;

    /// Lists current Scopes.
    ///
    /// # Errors
    /// Returns authorization, pagination-validation, or internal failures.
    fn list_scopes(
        &self,
        principal: &Principal,
        limit: u32,
        offset: u32,
    ) -> Result<ResourcePage<PreparedScope>, PolicyAdministrationError>;

    /// Requests discovery termination and cleanup of owned Bindings.
    ///
    /// # Errors
    /// Returns authorization, conflict, not-found, or internal failures.
    fn delete_scope(
        &self,
        principal: &Principal,
        id: &ResourceId,
    ) -> Result<ScopeDeletion, PolicyAdministrationError>;

    /// Reads one current Binding spec and lifecycle status.
    ///
    /// # Errors
    /// Returns authorization, not-found, or internal failures.
    fn get_binding(
        &self,
        principal: &Principal,
        id: &ResourceId,
    ) -> Result<BindingView, PolicyAdministrationError>;

    /// Lists current Binding specs and lifecycle statuses.
    ///
    /// # Errors
    /// Returns authorization, pagination-validation, or internal failures.
    fn list_bindings(
        &self,
        principal: &Principal,
        limit: u32,
        offset: u32,
    ) -> Result<ResourcePage<BindingView>, PolicyAdministrationError>;

    /// Explicitly retries failed work owned by a Scope.
    /// # Errors
    /// Returns authorization, not-found, admission, or internal failures.
    fn retry_scope(
        &self,
        principal: &Principal,
        id: &ResourceId,
    ) -> Result<PreparedScope, PolicyAdministrationError>;
}

impl<R> PolicyAdministration for PapService<R>
where
    R: PapRepository,
{
    fn create_policy(
        &self,
        principal: &Principal,
        policy_name: &str,
        template: &PolicyTemplate,
    ) -> Result<PreparedPolicy, PolicyAdministrationError> {
        require_policy_administrator(principal)?;
        PapService::create_policy(self, policy_name, template)
            .map_err(|error| project_pap_error(error, None))
    }

    fn update_policy(
        &self,
        principal: &Principal,
        policy_id: &ResourceId,
        policy_name: &str,
        template: &PolicyTemplate,
    ) -> Result<PreparedPolicy, PolicyAdministrationError> {
        require_policy_administrator(principal)?;
        PapService::update_policy(self, policy_id, policy_name, template)
            .map_err(|error| project_pap_error(error, Some(NotFoundResource::Policy)))
    }

    fn get_policy(
        &self,
        principal: &Principal,
        id: &ResourceId,
        revision: Revision,
    ) -> Result<PreparedPolicy, PolicyAdministrationError> {
        require_policy_administrator(principal)?;
        PapService::get_policy(self, id, revision)
            .map_err(|error| project_pap_error(error, Some(NotFoundResource::PolicyRevision)))
    }

    fn list_policies(
        &self,
        principal: &Principal,
        limit: u32,
        offset: u32,
    ) -> Result<ResourcePage<PreparedPolicy>, PolicyAdministrationError> {
        require_policy_administrator(principal)?;
        PapService::list_policies(self, limit, offset)
            .map(Into::into)
            .map_err(|error| project_pap_error(error, None))
    }

    fn delete_policy_revision(
        &self,
        principal: &Principal,
        id: &ResourceId,
        revision: Revision,
    ) -> Result<PreparedPolicy, PolicyAdministrationError> {
        require_policy_administrator(principal)?;
        PapService::delete_policy_revision(self, id, revision)
            .map_err(|error| project_pap_error(error, Some(NotFoundResource::PolicyRevision)))
    }

    fn create_scope_assignment(
        &self,
        principal: &Principal,
        selector: &ScopeSelector,
        policies: &[PolicyReference],
    ) -> Result<PreparedScope, PolicyAdministrationError> {
        require_policy_administrator(principal)?;
        PapService::create_scope_assignment(self, selector, policies)
            .map_err(|error| project_pap_error(error, None))
    }

    fn get_scope(
        &self,
        principal: &Principal,
        id: &ResourceId,
    ) -> Result<PreparedScope, PolicyAdministrationError> {
        require_policy_administrator(principal)?;
        PapService::get_scope(self, id)
            .map_err(|error| project_pap_error(error, Some(NotFoundResource::Scope)))
    }

    fn list_scopes(
        &self,
        principal: &Principal,
        limit: u32,
        offset: u32,
    ) -> Result<ResourcePage<PreparedScope>, PolicyAdministrationError> {
        require_policy_administrator(principal)?;
        PapService::list_scopes(self, limit, offset)
            .map(Into::into)
            .map_err(|error| project_pap_error(error, None))
    }

    fn delete_scope(
        &self,
        principal: &Principal,
        id: &ResourceId,
    ) -> Result<ScopeDeletion, PolicyAdministrationError> {
        require_policy_administrator(principal)?;
        PapService::delete_scope(self, id)
            .map_err(|error| project_pap_error(error, Some(NotFoundResource::Scope)))
    }

    fn get_binding(
        &self,
        principal: &Principal,
        id: &ResourceId,
    ) -> Result<BindingView, PolicyAdministrationError> {
        require_policy_administrator(principal)?;
        PapService::get_binding(self, id)
            .map_err(|error| project_pap_error(error, Some(NotFoundResource::Binding)))
    }

    fn list_bindings(
        &self,
        principal: &Principal,
        limit: u32,
        offset: u32,
    ) -> Result<ResourcePage<BindingView>, PolicyAdministrationError> {
        require_policy_administrator(principal)?;
        PapService::list_bindings(self, limit, offset)
            .map(Into::into)
            .map_err(|error| project_pap_error(error, None))
    }

    fn retry_scope(
        &self,
        principal: &Principal,
        id: &ResourceId,
    ) -> Result<PreparedScope, PolicyAdministrationError> {
        require_policy_administrator(principal)?;
        PapService::retry_scope(self, id)
            .map_err(|error| project_pap_error(error, Some(NotFoundResource::Scope)))
    }
}

fn require_policy_administrator(principal: &Principal) -> Result<(), PolicyAdministrationError> {
    if principal.role() == PrincipalRole::PolicyAdministrator {
        Ok(())
    } else {
        Err(PolicyAdministrationError::Forbidden)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PeerCredentials;

    #[test]
    fn authorization_uses_only_the_server_assigned_role() {
        let peer = PeerCredentials::new(1000, 100, 4242);
        let user = Principal::from_authenticated_peer(peer, PrincipalRole::LocalUser);
        let administrator =
            Principal::from_authenticated_peer(peer, PrincipalRole::PolicyAdministrator);

        assert_eq!(
            require_policy_administrator(&user),
            Err(PolicyAdministrationError::Forbidden)
        );
        assert_eq!(require_policy_administrator(&administrator), Ok(()));
    }

    #[test]
    fn pap_errors_are_projected_once_at_the_application_boundary() {
        assert_eq!(
            project_pap_error(PapError::Unavailable, None),
            PolicyAdministrationError::Unavailable
        );
        assert_eq!(
            project_pap_error(PapError::OperationInProgress, None),
            PolicyAdministrationError::OperationInProgress
        );
        assert_eq!(
            project_pap_error(PapError::Persistence, None),
            PolicyAdministrationError::Internal
        );
        assert_eq!(
            project_pap_error(PapError::InvalidPagination, None),
            PolicyAdministrationError::InvalidArgument(PolicyInputError::new(
                "invalid pagination: limit must be between 1 and 1000"
            ))
        );
        assert_eq!(
            project_pap_error(PapError::NotFound, Some(NotFoundResource::Policy)),
            PolicyAdministrationError::NotFound(NotFoundResource::Policy)
        );
        assert_eq!(
            project_pap_error(PapError::ReferencedPolicyRevisionNotFound, None),
            PolicyAdministrationError::NotFound(NotFoundResource::ReferencedPolicyRevision)
        );
        for path in ["pid", "cgroupId"] {
            assert_eq!(
                project_pap_error(
                    PapError::InvalidScope(ValidationError::new(path, "must be positive")),
                    None
                ),
                PolicyAdministrationError::InvalidArgument(PolicyInputError::new(format!(
                    "invalid scope: {path}: must be positive"
                )))
            );
        }
        assert_eq!(
            project_pap_error(
                PapError::InvalidPolicy(ValidationError::new(
                    "internal.validation",
                    "compiler output exposed an internal mismatch"
                )),
                None
            ),
            PolicyAdministrationError::Internal
        );
    }
}

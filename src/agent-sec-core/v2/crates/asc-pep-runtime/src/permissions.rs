//! JSON and Cedar inputs shared by direct execution and per-invocation permission providers.

mod cedar;

use asc_daemon_protocol::authorization::{
    ExecutionSpec, LaunchSpec, OsPermissionSet, PathFact, Validate,
};
use asc_policy_types::authoring::PolicySource;
use asc_policy_types::cedar::CedarPolicySource;

use crate::{PepError, PermissionProvider};

/// An explicit permission request source, not an approved or published policy.
pub struct PermissionInput(Input);

enum Input {
    Json(OsPermissionSet),
    Cedar(CedarPolicySource),
}

impl PermissionInput {
    /// Accepts OS permission JSON, raw Cedar, or the existing Cedar policy source envelope.
    ///
    /// In-memory callers can parse a fresh source for each authorization request. Parsing never
    /// contacts the PDP, invokes a model, or supplies implicit runtime dependencies.
    ///
    /// # Errors
    /// Rejects inputs over 4 MiB, malformed JSON and unsupported source envelopes. Cedar parsing
    /// and projection take place when preparing permissions for a launch.
    pub fn parse(text: &str) -> Result<Self, PermissionInputError> {
        if text.len() > 4 * 1024 * 1024 {
            return Err(PermissionInputError::Unsupported(
                "sandbox permissions exceed the 4 MiB input limit".into(),
            ));
        }
        let input = if text.trim_start().starts_with('{') {
            let value: serde_json::Value = serde_json::from_str(text)?;
            if value.get("language").is_some() {
                match serde_json::from_str(text)? {
                    PolicySource::Cedar(source) => Input::Cedar(source),
                    PolicySource::SecCore(_) => {
                        return Err(PermissionInputError::Unsupported(
                            "expected OS permissions or a Cedar source".into(),
                        ));
                    }
                }
            } else {
                Input::Json(serde_json::from_str(text)?)
            }
        } else {
            Input::Cedar(CedarPolicySource {
                schema: asc_policy_cedar::schema::embedded_reference(),
                text: text.to_owned(),
            })
        };
        Ok(Self(input))
    }

    /// Previews the source's lexical permission regions for a launch.
    ///
    /// File writes include all mutations and reads; program-specific file access becomes
    /// process-tree-wide. This projection is not proof that all Cedar restrictions are enforced.
    /// Use [`PermissionProvider::prepare`] to bind filesystem objects before execution or PDP use.
    ///
    /// # Errors
    /// Rejects invalid permissions, unsupported conditions and unrepresentable path regions.
    pub fn resolve(&self, launch: &LaunchSpec) -> Result<OsPermissionSet, PermissionInputError> {
        let permissions = match &self.0 {
            Input::Json(permissions) => permissions.clone(),
            Input::Cedar(source) => cedar::project(source, launch, None)?,
        };
        permissions.validate()?;
        Ok(permissions)
    }
}

impl PermissionProvider for PermissionInput {
    fn permissions(&self, launch: &LaunchSpec) -> Result<OsPermissionSet, PepError> {
        Ok(self.resolve(launch)?)
    }

    fn prepare(&self, launch: &LaunchSpec) -> Result<ExecutionSpec, PepError> {
        match &self.0 {
            Input::Json(permissions) => {
                crate::FixedPermissions(permissions.clone()).prepare(launch)
            }
            Input::Cedar(source) => cedar::bind(source, launch),
        }
    }

    fn validate_path_facts(
        &self,
        launch: &LaunchSpec,
        permissions: &OsPermissionSet,
        facts: &[PathFact],
    ) -> Result<(), PepError> {
        if let Input::Cedar(source) = &self.0
            && cedar::project(source, launch, Some(facts))? != *permissions
        {
            return Err(PepError::Protocol(
                "permission source changed during preparation".into(),
            ));
        }
        Ok(())
    }
}

/// Permission input failures before authorization or sandbox installation.
#[derive(Debug, thiserror::Error)]
pub enum PermissionInputError {
    /// JSON input did not match a supported versioned contract.
    #[error("invalid sandbox permissions: {0}")]
    Json(#[from] serde_json::Error),
    /// The Cedar frontend rejected syntax, schema or source metadata.
    #[error(transparent)]
    Cedar(#[from] asc_policy_cedar::CedarError),
    /// OS permission or schema-reference validation failed.
    #[error(transparent)]
    Validation(#[from] asc_policy_types::ValidationError),
    /// A path could not be inspected during projection.
    #[error("cannot inspect sandbox permission path: {0}")]
    Io(#[from] std::io::Error),
    /// The requested source cannot be represented by the supported coarse mapping.
    #[error("unsupported sandbox permission input: {0}")]
    Unsupported(String),
}

//! Transactional extension preflight, consent commit, update, recovery, and uninstall.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::git::{GitMaterializer, GitSourceError};
use super::manifest::{parse_manifest, ParsedManifest};
use super::source::{
    canonical_local_source, content_digest, copy_package_tree, write_metadata,
    ManagedInstallationMetadata, ManagedSourceKind, SourceError, StoreLock, StorePaths,
    PAYLOAD_DIR, ROLLBACK_DIR, STAGING_DIR,
};
use super::state::{self, SourceSelection, SourceSelectionRecord};
use super::EXTENSION_CONFIG_FILENAME;

const OPERATION_SCHEMA_VERSION: u32 = 1;
const INSTALLATION_SCHEMA_VERSION: u32 = 1;
const STORE_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const OPERATION_TTL_MINUTES: i64 = 30;
const CONSENT_POLICY_VERSION: u32 = 1;

mod prepare;
mod support;
mod transaction;

#[cfg(test)]
use support::read_installation_metadata;
pub use support::InstallerError;
use support::{validate_operation_id, write_json_atomic};

/// Lifecycle action represented by a prepared operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OperationAction {
    /// Copy a validated local package into the managed store.
    Install,
    /// Link a validated local package from the managed store.
    Link,
    /// Replace an existing Git HTTPS installation from the same source identity.
    Update,
}

/// Reviewable result returned before an installation may mutate the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ExtensionPreflight {
    /// Opaque operation identity used by commit or cancel.
    pub operation_id: String,
    /// Prepared lifecycle action.
    pub action: OperationAction,
    /// Validated package identity.
    pub name: String,
    /// Validated package version.
    pub version: String,
    /// Canonical source kind.
    pub source_kind: ManagedSourceKind,
    /// Canonical local source path.
    pub source_identity: String,
    /// Requested Git ref for Git HTTPS sources.
    pub requested_ref: Option<String>,
    /// Resolved immutable Git revision for Git HTTPS sources.
    pub resolved_revision: Option<String>,
    /// Version currently installed before an update.
    pub previous_version: Option<String>,
    /// Revision currently installed before an update.
    pub previous_revision: Option<String>,
    /// Content digest currently installed before an update.
    pub previous_content_digest: Option<String>,
    /// Capability fingerprint currently installed before an update.
    pub previous_capability_fingerprint: Option<String>,
    /// Digest of the prepared payload.
    pub content_digest: String,
    /// Security projection the user must consent to.
    pub capability_fingerprint: String,
    /// Canonical capability IDs represented by the fingerprint.
    pub capabilities: Vec<String>,
    /// Capability IDs introduced by this operation.
    pub capabilities_added: Vec<String>,
    /// Capability IDs removed by this operation.
    pub capabilities_removed: Vec<String>,
    /// Whether any capability projection changed, including same-ID modifications.
    pub capability_fingerprint_changed: bool,
    /// Whether the security projection changed and requires fresh consent.
    pub consent_required: bool,
    /// Whether version, revision, digest, or fingerprint differs from the installation.
    pub changed: bool,
    /// Non-fatal manifest diagnostics visible during consent.
    pub diagnostics: Vec<super::ExtensionDiagnostic>,
    /// Structured security categories shown before consent.
    pub risk_summary: CapabilityRiskSummary,
    /// Desired state expected after commit.
    pub expected_desired_state: String,
    /// Effective state remains unchanged until the activation boundary.
    pub expected_effective_state: String,
    /// Expected activation boundary after commit.
    pub expected_activation: String,
    /// Earlier consent record reused when fresh consent is unnecessary.
    pub reused_consent_reference: Option<String>,
}

/// Reviewable risk categories bound to one capability fingerprint.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct CapabilityRiskSummary {
    /// Hooks, MCP processes, commands, arguments, and host executables.
    pub execution: Vec<String>,
    /// Skills, contexts, and agent prompt contributions.
    pub instruction: Vec<String>,
    /// Agent tool, skill, and MCP requests.
    pub authorization: Vec<String>,
    /// Setting schema and child-process setting injection targets.
    pub credential: Vec<String>,
    /// Package and workspace path references.
    pub filesystem: Vec<String>,
}

/// Successful committed lifecycle mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ExtensionMutationResult {
    /// Completed operation identity.
    pub operation_id: String,
    /// Mutation action.
    pub action: String,
    /// Affected package identity.
    pub name: String,
    /// Installed version when applicable.
    pub version: Option<String>,
    /// Materialized source kind.
    pub source_kind: ManagedSourceKind,
    /// Canonical source identity committed by the mutation.
    pub source_identity: String,
    /// Resolved Git revision when applicable.
    pub resolved_revision: Option<String>,
    /// Committed package digest.
    pub content_digest: String,
    /// Committed capability security fingerprint.
    pub capability_fingerprint: String,
    /// Whether the operation replaced package content.
    pub changed: bool,
    /// Activation boundary for the catalog mutation.
    pub activation: String,
    /// Consent record authorizing this result.
    pub consent_reference: String,
    /// Acceptance time when this operation obtained fresh consent.
    pub consent_accepted_at: Option<chrono::DateTime<Utc>>,
    /// Consent policy version bound to the record.
    pub consent_policy_version: u32,
    /// Earlier record reused when this operation did not need fresh consent.
    pub reused_consent_reference: Option<String>,
    /// Risk categories bound to the accepted fingerprint.
    pub risk_summary: CapabilityRiskSummary,
}

/// Owns transactional changes to one managed extension store.
#[derive(Debug, Clone)]
pub struct ExtensionInstaller {
    store: StorePaths,
    state_dir_override: Option<PathBuf>,
    git_materializer: GitMaterializer,
}

impl ExtensionInstaller {
    /// Creates an installer for the current user's extension and state roots.
    pub fn for_current_user() -> Result<Self, InstallerError> {
        Ok(Self {
            store: StorePaths::for_current_user().map_err(InstallerError::from_source)?,
            state_dir_override: None,
            git_materializer: GitMaterializer::new(),
        })
    }

    /// Creates an installer with isolated roots for tests and embedded callers.
    pub fn new(store_root: PathBuf, state_dir: PathBuf) -> Self {
        Self {
            store: StorePaths::new(store_root),
            state_dir_override: Some(state_dir),
            git_materializer: GitMaterializer::new(),
        }
    }

    #[cfg(test)]
    fn new_with_git_fixture(store_root: PathBuf, state_dir: PathBuf, fixture: PathBuf) -> Self {
        Self {
            store: StorePaths::new(store_root),
            state_dir_override: Some(state_dir),
            git_materializer: GitMaterializer::with_test_fixture(fixture),
        }
    }

    /// Prepares a safe path-copy installation without mutating the selected catalog.
    pub fn preflight_path_copy(&self, source: &Path) -> Result<ExtensionPreflight, InstallerError> {
        self.recover()?;
        self.prepare_local(source, OperationAction::Install)
    }

    /// Prepares a development link without mutating the selected catalog.
    pub fn preflight_link(&self, source: &Path) -> Result<ExtensionPreflight, InstallerError> {
        self.recover()?;
        self.prepare_local(source, OperationAction::Link)
    }

    /// Prepares a Git HTTPS installation pinned to a resolved revision.
    pub fn preflight_git_install(
        &self,
        source: &str,
        requested_ref: Option<&str>,
    ) -> Result<ExtensionPreflight, InstallerError> {
        self.recover()?;
        self.prepare_git(source, requested_ref, None)
    }

    /// Prepares an update for an existing Git HTTPS installation.
    pub fn preflight_update(&self, name: &str) -> Result<ExtensionPreflight, InstallerError> {
        self.recover()?;
        super::identity::validate_package_name(name)
            .map_err(|error| InstallerError::new(error.code(), error.to_string()))?;
        self.prepare_git("", None, Some(name))
    }

    /// Returns a prepared operation so a later slash request can resume consent.
    pub fn operation(&self, operation_id: &str) -> Result<ExtensionPreflight, InstallerError> {
        validate_operation_id(operation_id)?;
        Ok(self.read_operation(operation_id)?.preflight)
    }

    /// Returns a durable completed mutation receipt after transport interruption.
    pub fn result(&self, operation_id: &str) -> Result<ExtensionMutationResult, InstallerError> {
        validate_operation_id(operation_id)?;
        let path = self.store.receipt(operation_id);
        let bytes = fs::read(&path).map_err(|error| {
            InstallerError::new(
                "extension_operation_result_not_found",
                format!("failed to read {}: {error}", path.display()),
            )
        })?;
        serde_json::from_slice(&bytes).map_err(|error| {
            InstallerError::new(
                "extension_operation_result_invalid",
                format!("failed to parse {}: {error}", path.display()),
            )
        })
    }

    /// Creates a durable batch operation before any item may mutate the store.
    pub fn begin_batch(&self, action: &str, items: &[String]) -> Result<Value, InstallerError> {
        self.store
            .ensure_internal_dirs()
            .map_err(InstallerError::from_source)?;
        let operation_id = Uuid::new_v4().to_string();
        let result = serde_json::json!({
            "action": action,
            "items": [],
            "operation_id": operation_id,
            "planned_items": items,
            "schema_version": OPERATION_SCHEMA_VERSION,
            "status": "prepared",
            "summary": {},
        });
        self.write_batch_result(&operation_id, &result)?;
        Ok(result)
    }

    /// Reads either a single-operation receipt or a durable batch status.
    pub fn result_value(&self, operation_id: &str) -> Result<Value, InstallerError> {
        validate_operation_id(operation_id)?;
        let path = self.store.receipt(operation_id);
        let bytes = fs::read(&path).map_err(|error| {
            InstallerError::new(
                "extension_operation_result_not_found",
                format!("failed to read {}: {error}", path.display()),
            )
        })?;
        serde_json::from_slice(&bytes).map_err(|error| {
            InstallerError::new(
                "extension_operation_result_invalid",
                format!("failed to parse {}: {error}", path.display()),
            )
        })
    }

    /// Atomically checkpoints progress for an existing batch operation.
    pub fn write_batch_result(
        &self,
        operation_id: &str,
        result: &Value,
    ) -> Result<(), InstallerError> {
        validate_operation_id(operation_id)?;
        write_json_atomic(&self.store.receipt(operation_id), result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn source_package(root: &Path, name: &str) -> PathBuf {
        let package = root.join("source");
        fs::create_dir_all(&package).unwrap();
        fs::write(
            package.join(EXTENSION_CONFIG_FILENAME),
            format!(
                r#"{{"schemaVersion":1,"name":"{name}","version":"1.0.0","compatibility":{{"cosh":">=0.12.0"}}}}"#
            ),
        )
        .unwrap();
        package
    }

    fn git_fixture(root: &Path, name: &str, version: &str, with_hook: bool) -> PathBuf {
        let repository = root.join("repository");
        fs::create_dir_all(&repository).unwrap();
        if !repository.join(".git").exists() {
            run_fixture_git(&repository, ["init", "--quiet"]);
            run_fixture_git(&repository, ["config", "user.name", "Cosh Test"]);
            run_fixture_git(
                &repository,
                ["config", "user.email", "cosh-test@example.invalid"],
            );
        }
        let hook = if with_hook {
            fs::create_dir_all(repository.join("hooks")).unwrap();
            fs::write(repository.join("hooks/guard"), "#!/bin/sh\nexit 0\n").unwrap();
            r#", "hooks":{"PreToolUse":[{"matcher":"shell","hooks":[{"type":"command","name":"guard","command":"${extensionPath}/hooks/guard"}]}]}"#
        } else {
            ""
        };
        fs::write(
            repository.join(EXTENSION_CONFIG_FILENAME),
            format!(
                r#"{{"schemaVersion":1,"name":"{name}","version":"{version}","compatibility":{{"cosh":">=0.12.0"}}{hook}}}"#
            ),
        )
        .unwrap();
        run_fixture_git(&repository, ["add", "."]);
        run_fixture_git(&repository, ["commit", "--quiet", "-m", version]);
        repository
    }

    fn run_fixture_git<'a>(repository: &Path, args: impl IntoIterator<Item = &'a str>) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repository)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git fixture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn path_copy_requires_matching_consent_fingerprint() {
        let temporary = tempfile::tempdir().unwrap();
        let source = source_package(temporary.path(), "example.ops");
        let installer = ExtensionInstaller::new(
            temporary.path().join("extensions"),
            temporary.path().join("states"),
        );
        let preflight = installer.preflight_path_copy(&source).unwrap();
        let error = installer
            .commit(&preflight.operation_id, "wrong")
            .unwrap_err();
        assert_eq!(error.code(), "extension_consent_fingerprint_mismatch");
        assert!(!installer.store.installation("example.ops").exists());
    }

    #[test]
    fn commit_rejects_expired_operation_without_publishing() {
        let temporary = tempfile::tempdir().unwrap();
        let source = source_package(temporary.path(), "example.expired");
        let installer = ExtensionInstaller::new(
            temporary.path().join("extensions"),
            temporary.path().join("states"),
        );
        let preflight = installer.preflight_path_copy(&source).unwrap();
        let mut operation = installer.read_operation(&preflight.operation_id).unwrap();
        operation.prepared_at = Utc::now() - chrono::Duration::minutes(31);
        write_json_atomic(
            &installer.store.operation(&preflight.operation_id),
            &operation,
        )
        .unwrap();

        let error = installer
            .commit(&preflight.operation_id, &preflight.capability_fingerprint)
            .unwrap_err();
        assert_eq!(error.code(), "extension_operation_expired");
        assert!(!installer.store.installation("example.expired").exists());
    }

    #[test]
    fn path_copy_commit_publishes_metadata_and_selection() {
        let temporary = tempfile::tempdir().unwrap();
        let source = source_package(temporary.path(), "example.ops");
        let state_dir = temporary.path().join("states");
        let installer =
            ExtensionInstaller::new(temporary.path().join("extensions"), state_dir.clone());
        let preflight = installer.preflight_path_copy(&source).unwrap();
        let result = installer
            .commit(&preflight.operation_id, &preflight.capability_fingerprint)
            .unwrap();
        assert_eq!(result.activation, "next_session");
        assert_eq!(result.consent_policy_version, CONSENT_POLICY_VERSION);
        assert!(result.consent_accepted_at.is_some());
        assert_eq!(installer.result(&preflight.operation_id).unwrap(), result);
        assert!(installer
            .store
            .installation("example.ops")
            .join(PAYLOAD_DIR)
            .is_dir());
        let loaded = state::load(Some(&state_dir)).unwrap();
        assert_eq!(
            loaded.state.source_selections["example.ops"].source_identity,
            source.canonicalize().unwrap().to_string_lossy()
        );
    }

    #[test]
    fn candidate_health_failure_restores_package_and_state() {
        let temporary = tempfile::tempdir().unwrap();
        let source = source_package(temporary.path(), "example.rollback");
        let state_dir = temporary.path().join("states");
        let installer =
            ExtensionInstaller::new(temporary.path().join("extensions"), state_dir.clone());
        let preflight = installer.preflight_path_copy(&source).unwrap();

        let pending = installer
            .commit_pending(&preflight.operation_id, &preflight.capability_fingerprint)
            .unwrap();
        assert!(installer.store.installation("example.rollback").exists());
        assert!(state::load(Some(&state_dir))
            .unwrap()
            .state
            .source_selections
            .contains_key("example.rollback"));

        installer.rollback_pending(pending).unwrap();

        assert!(!installer.store.installation("example.rollback").exists());
        assert!(installer.store.staging(&preflight.operation_id).exists());
        assert!(!state::load(Some(&state_dir))
            .unwrap()
            .state
            .source_selections
            .contains_key("example.rollback"));
        assert!(!installer.store.receipt(&preflight.operation_id).exists());
    }

    #[test]
    fn colliding_commit_discards_prepared_state_before_journaling() {
        let temporary = tempfile::tempdir().unwrap();
        let source = source_package(temporary.path(), "example.collision");
        let installer = ExtensionInstaller::new(
            temporary.path().join("extensions"),
            temporary.path().join("states"),
        );
        let first = installer.preflight_path_copy(&source).unwrap();
        let second = installer.preflight_path_copy(&source).unwrap();
        installer
            .commit(&first.operation_id, &first.capability_fingerprint)
            .unwrap();

        let error = installer
            .commit(&second.operation_id, &second.capability_fingerprint)
            .unwrap_err();

        assert_eq!(error.code(), "extension_already_installed");
        assert!(!installer
            .store
            .pending_commit_journal(&second.operation_id)
            .exists());
        assert!(!installer.store.staging(&second.operation_id).exists());
        assert!(!installer.store.operation(&second.operation_id).exists());
        installer.recover().unwrap();
    }

    #[test]
    fn recovery_rolls_back_published_but_unvalidated_candidate() {
        let temporary = tempfile::tempdir().unwrap();
        let source = source_package(temporary.path(), "example.interrupted");
        let state_dir = temporary.path().join("states");
        let installer =
            ExtensionInstaller::new(temporary.path().join("extensions"), state_dir.clone());
        let preflight = installer.preflight_path_copy(&source).unwrap();
        let pending = installer
            .commit_pending(&preflight.operation_id, &preflight.capability_fingerprint)
            .unwrap();
        drop(pending);

        let recovery = installer.recover().unwrap();

        assert_eq!(recovery.rolled_back_pending_commits, 1);
        assert!(!installer.store.installation("example.interrupted").exists());
        assert!(installer.store.staging(&preflight.operation_id).exists());
        assert!(!state::load(Some(&state_dir))
            .unwrap()
            .state
            .source_selections
            .contains_key("example.interrupted"));
    }

    #[test]
    fn uninstall_candidate_failure_restores_exact_package_and_state() {
        let temporary = tempfile::tempdir().unwrap();
        let source = source_package(temporary.path(), "example.keep");
        let state_dir = temporary.path().join("states");
        let installer =
            ExtensionInstaller::new(temporary.path().join("extensions"), state_dir.clone());
        let preflight = installer.preflight_path_copy(&source).unwrap();
        installer
            .commit(&preflight.operation_id, &preflight.capability_fingerprint)
            .unwrap();
        state::set_enabled("example.keep", false, Some(&state_dir)).unwrap();
        let before = state::load(Some(&state_dir)).unwrap().state;

        let pending = installer.uninstall_pending("example.keep").unwrap();
        assert!(!installer.store.installation("example.keep").exists());
        installer.rollback_uninstall(pending).unwrap();

        assert!(installer.store.installation("example.keep").exists());
        assert_eq!(state::load(Some(&state_dir)).unwrap().state, before);
    }

    #[test]
    fn recovery_restores_unvalidated_uninstall() {
        let temporary = tempfile::tempdir().unwrap();
        let source = source_package(temporary.path(), "example.uninstall-interrupted");
        let installer = ExtensionInstaller::new(
            temporary.path().join("extensions"),
            temporary.path().join("states"),
        );
        let preflight = installer.preflight_path_copy(&source).unwrap();
        installer
            .commit(&preflight.operation_id, &preflight.capability_fingerprint)
            .unwrap();
        let pending = installer
            .uninstall_pending("example.uninstall-interrupted")
            .unwrap();
        drop(pending);

        let recovery = installer.recover().unwrap();

        assert_eq!(recovery.restored_uninstalls, 1);
        assert!(installer
            .store
            .installation("example.uninstall-interrupted")
            .exists());
    }

    #[test]
    fn recovery_restores_unvalidated_desired_state_mutation() {
        let temporary = tempfile::tempdir().unwrap();
        let state_dir = temporary.path().join("states");
        let installer =
            ExtensionInstaller::new(temporary.path().join("extensions"), state_dir.clone());
        let pending = installer.begin_state_mutation().unwrap();
        state::set_enabled("example.state", false, Some(&state_dir)).unwrap();
        assert!(state::load(Some(&state_dir))
            .unwrap()
            .state
            .disabled
            .contains("example.state"));
        drop(pending);

        let recovery = installer.recover().unwrap();

        assert_eq!(recovery.rolled_back_state_mutations, 1);
        assert!(!state::load(Some(&state_dir))
            .unwrap()
            .state
            .disabled
            .contains("example.state"));
    }

    #[test]
    fn commit_resumes_after_staging_was_published() {
        let temporary = tempfile::tempdir().unwrap();
        let source = source_package(temporary.path(), "example.resume");
        let installer = ExtensionInstaller::new(
            temporary.path().join("extensions"),
            temporary.path().join("states"),
        );
        let preflight = installer.preflight_path_copy(&source).unwrap();
        fs::rename(
            installer.store.staging(&preflight.operation_id),
            installer.store.installation("example.resume"),
        )
        .unwrap();

        let result = installer
            .commit(&preflight.operation_id, &preflight.capability_fingerprint)
            .unwrap();
        assert_eq!(result.name, "example.resume");
        assert!(!installer.store.operation(&preflight.operation_id).exists());
    }

    #[test]
    fn recovery_removes_staging_without_operation_record() {
        let temporary = tempfile::tempdir().unwrap();
        let installer = ExtensionInstaller::new(
            temporary.path().join("extensions"),
            temporary.path().join("states"),
        );
        installer.store.ensure_internal_dirs().unwrap();
        let operation_id = Uuid::new_v4().to_string();
        fs::create_dir(installer.store.staging(&operation_id)).unwrap();

        let result = installer.recover().unwrap();
        assert_eq!(result.removed_orphan_staging, 1);
        assert!(!installer.store.staging(&operation_id).exists());
    }

    #[test]
    fn preflight_rejects_legacy_alias_with_same_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let source = source_package(temporary.path(), "example.duplicate");
        let extension_root = temporary.path().join("extensions");
        let legacy = extension_root.join("alias-directory");
        fs::create_dir_all(&legacy).unwrap();
        fs::copy(
            source.join(EXTENSION_CONFIG_FILENAME),
            legacy.join(EXTENSION_CONFIG_FILENAME),
        )
        .unwrap();
        let installer = ExtensionInstaller::new(extension_root, temporary.path().join("states"));

        let error = installer.preflight_path_copy(&source).unwrap_err();
        assert_eq!(error.code(), "extension_already_installed");
    }

    #[test]
    fn update_rejects_path_copy_source() {
        let temporary = tempfile::tempdir().unwrap();
        let source = source_package(temporary.path(), "example.local");
        let installer = ExtensionInstaller::new(
            temporary.path().join("extensions"),
            temporary.path().join("states"),
        );
        let preflight = installer.preflight_path_copy(&source).unwrap();
        installer
            .commit(&preflight.operation_id, &preflight.capability_fingerprint)
            .unwrap();

        let error = installer.preflight_update("example.local").unwrap_err();
        assert_eq!(error.code(), "extension_source_not_updatable");
    }

    #[test]
    fn git_update_switches_atomically_and_preserves_disabled_intent() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = git_fixture(temporary.path(), "example.git", "1.0.0", false);
        let state_dir = temporary.path().join("states");
        let installer = ExtensionInstaller::new_with_git_fixture(
            temporary.path().join("extensions"),
            state_dir.clone(),
            repository.clone(),
        );
        let install = installer
            .preflight_git_install("https://example.com/example.git", Some("HEAD"))
            .unwrap();
        assert!(install.consent_required);
        assert_eq!(install.source_kind, ManagedSourceKind::GitHttps);
        installer
            .commit(&install.operation_id, &install.capability_fingerprint)
            .unwrap();
        let installed_at = read_installation_metadata(&installer.store.installation("example.git"))
            .unwrap()
            .installed_at;
        state::set_enabled("example.git", false, Some(&state_dir)).unwrap();

        let unchanged = installer.preflight_update("example.git").unwrap();
        assert!(!unchanged.changed);
        assert!(!unchanged.consent_required);
        installer
            .commit(&unchanged.operation_id, &unchanged.capability_fingerprint)
            .unwrap();

        git_fixture(temporary.path(), "example.git", "1.1.0", true);
        let update = installer.preflight_update("example.git").unwrap();
        assert!(update.changed);
        assert!(update.consent_required);
        assert_eq!(update.previous_version.as_deref(), Some("1.0.0"));
        assert_eq!(update.capabilities_added, ["example.git/hook/guard"]);
        let result = installer
            .commit(&update.operation_id, &update.capability_fingerprint)
            .unwrap();
        assert_eq!(result.action, "update");
        let metadata =
            read_installation_metadata(&installer.store.installation("example.git")).unwrap();
        assert_eq!(metadata.version, "1.1.0");
        assert_eq!(metadata.installed_at, installed_at);
        assert!(!installer.store.rollback(&update.operation_id).exists());
        assert!(state::load(Some(&state_dir))
            .unwrap()
            .state
            .disabled
            .contains("example.git"));
        let system_dir = temporary.path().join("system");
        fs::create_dir_all(&system_dir).unwrap();
        let mut manager = super::super::manager::ExtensionManager::new_isolated_with_state(
            PathBuf::from("/workspace"),
            Some(temporary.path().join("extensions")),
            Some(system_dir),
            state_dir,
        );
        manager.refresh();
        assert_eq!(
            manager.list()[0].source,
            super::super::ExtensionSourceKind::GitHttps
        );
        assert_eq!(manager.list()[0].version, "1.1.0");
        assert_eq!(
            manager.list()[0].desired_state,
            super::super::DesiredState::Disabled
        );
    }

    #[test]
    fn git_update_resumes_after_old_installation_was_preserved() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = git_fixture(temporary.path(), "example.resume", "1.0.0", false);
        let installer = ExtensionInstaller::new_with_git_fixture(
            temporary.path().join("extensions"),
            temporary.path().join("states"),
            repository,
        );
        let install = installer
            .preflight_git_install("https://example.com/resume.git", None)
            .unwrap();
        installer
            .commit(&install.operation_id, &install.capability_fingerprint)
            .unwrap();
        git_fixture(temporary.path(), "example.resume", "1.1.0", false);
        let update = installer.preflight_update("example.resume").unwrap();
        fs::rename(
            installer.store.installation("example.resume"),
            installer.store.rollback(&update.operation_id),
        )
        .unwrap();

        installer
            .commit(&update.operation_id, &update.capability_fingerprint)
            .unwrap();
        let metadata =
            read_installation_metadata(&installer.store.installation("example.resume")).unwrap();
        assert_eq!(metadata.version, "1.1.0");
        assert!(!installer.store.rollback(&update.operation_id).exists());
    }

    #[test]
    fn recovery_restores_interrupted_update_before_publication() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = git_fixture(temporary.path(), "example.recover", "1.0.0", false);
        let installer = ExtensionInstaller::new_with_git_fixture(
            temporary.path().join("extensions"),
            temporary.path().join("states"),
            repository,
        );
        let install = installer
            .preflight_git_install("https://example.com/recover.git", None)
            .unwrap();
        installer
            .commit(&install.operation_id, &install.capability_fingerprint)
            .unwrap();
        git_fixture(temporary.path(), "example.recover", "1.1.0", false);
        let update = installer.preflight_update("example.recover").unwrap();
        fs::rename(
            installer.store.installation("example.recover"),
            installer.store.rollback(&update.operation_id),
        )
        .unwrap();

        let recovery = installer.recover().unwrap();
        assert_eq!(recovery.restored_updates, 1);
        assert!(installer.store.staging(&update.operation_id).exists());
        let metadata =
            read_installation_metadata(&installer.store.installation("example.recover")).unwrap();
        assert_eq!(metadata.version, "1.0.0");
    }

    #[test]
    fn recovery_finalizes_update_after_publication() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = git_fixture(temporary.path(), "example.finalize", "1.0.0", false);
        let installer = ExtensionInstaller::new_with_git_fixture(
            temporary.path().join("extensions"),
            temporary.path().join("states"),
            repository,
        );
        let install = installer
            .preflight_git_install("https://example.com/finalize.git", None)
            .unwrap();
        installer
            .commit(&install.operation_id, &install.capability_fingerprint)
            .unwrap();
        git_fixture(temporary.path(), "example.finalize", "1.1.0", false);
        let update = installer.preflight_update("example.finalize").unwrap();
        fs::rename(
            installer.store.installation("example.finalize"),
            installer.store.rollback(&update.operation_id),
        )
        .unwrap();
        fs::rename(
            installer.store.staging(&update.operation_id),
            installer.store.installation("example.finalize"),
        )
        .unwrap();

        let recovery = installer.recover().unwrap();
        assert_eq!(recovery.completed_updates, 1);
        assert!(!installer.store.rollback(&update.operation_id).exists());
        assert!(!installer.store.operation(&update.operation_id).exists());
        let metadata =
            read_installation_metadata(&installer.store.installation("example.finalize")).unwrap();
        assert_eq!(metadata.version, "1.1.0");
    }

    #[test]
    fn uninstall_preserves_disabled_intent() {
        let temporary = tempfile::tempdir().unwrap();
        let source = source_package(temporary.path(), "example.ops");
        let state_dir = temporary.path().join("states");
        let installer =
            ExtensionInstaller::new(temporary.path().join("extensions"), state_dir.clone());
        let preflight = installer.preflight_path_copy(&source).unwrap();
        installer
            .commit(&preflight.operation_id, &preflight.capability_fingerprint)
            .unwrap();
        state::set_enabled("example.ops", false, Some(&state_dir)).unwrap();
        installer.uninstall("example.ops").unwrap();
        let loaded = state::load(Some(&state_dir)).unwrap();
        assert!(loaded.state.disabled.contains("example.ops"));
        assert!(!loaded.state.source_selections.contains_key("example.ops"));
    }

    #[cfg(unix)]
    #[test]
    fn link_commit_keeps_external_payload() {
        let temporary = tempfile::tempdir().unwrap();
        let source = source_package(temporary.path(), "example.dev");
        let installer = ExtensionInstaller::new(
            temporary.path().join("extensions"),
            temporary.path().join("states"),
        );
        let preflight = installer.preflight_link(&source).unwrap();
        installer
            .commit(&preflight.operation_id, &preflight.capability_fingerprint)
            .unwrap();
        assert!(fs::symlink_metadata(
            installer
                .store
                .installation("example.dev")
                .join(PAYLOAD_DIR)
        )
        .unwrap()
        .file_type()
        .is_symlink());
    }
}

//! v5 installed-state store: persistence for the authority-centric domain
//! model, with the one-time legacy migration wired in at the load boundary.
//!
//! The v5 file stores [`Installation`] records directly plus quarantined
//! legacy records verbatim. A schema ≤ 4 file is parsed through the legacy
//! [`InstalledState`] wire type and pushed through
//! [`migrate_state`] exactly once —
//! the domain types never learn to read the old field soup.
//!
//! Migration is lazy on disk by design: loading never writes. The first
//! *mutating* command that saves persists the store as v5 and leaves the
//! untouched legacy file behind as a `.v4.bak` sibling, so a downgrade or a
//! migration bug never loses the original bytes. Read-only commands on a
//! legacy file keep reading (and re-migrating) the v4 bytes forever.

use std::fs;
use std::path::{Path, PathBuf};

use anolisa_platform::fs_layout::{FsLayout, InstallMode as LayoutInstallMode};
use serde::{Deserialize, Serialize};

use crate::adapter::claim::AdapterClaim;
use crate::domain::{Installation, InstallationScope};
use crate::planner::RecordFacts;
use crate::state::{
    BackupRecord, InstallMode, InstalledState, ObjectKind, OperationRecord, StateError,
    now_iso8601, write_atomic,
};
use crate::state_identity::repair_known_installation_identities;
use crate::state_migration::{MigrationRule, QuarantinedObject, migrate_state};

/// Baseline schema version: what this store writes when no schema-v6
/// feature is in use, and the newest version pre-anchor readers accept.
pub const STORE_SCHEMA_VERSION: u32 = 5;

/// Schema version written when [`StateStore::adapter_trust_roots`] is
/// non-empty. Same wire shape as v5 plus the anchor array — bumped so
/// pre-anchor writers (≤ 0.2.16) hit their `NewerSchema` guard and refuse
/// the file instead of silently dropping the security-critical anchors on
/// their next save.
pub const STORE_SCHEMA_VERSION_ANCHORED: u32 = 6;

/// v5/v6 on-disk wire shape. Kept private: the store is the API, the file
/// layout is an implementation detail. v6 is v5 plus `adapter_trust_roots`;
/// the version number on disk tracks whether that array is populated.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StateFileV5 {
    schema_version: u32,
    updated_at: String,
    install_mode: InstallMode,
    prefix: PathBuf,
    anolisa_version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    installations: Vec<Installation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    quarantined: Vec<QuarantinedObject>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    backups: Vec<BackupRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    operations: Vec<OperationRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    adapter_claims: Vec<AdapterClaim>,
    /// See [`AdapterTrustRoot`]. `default` keeps pre-anchor v5 files
    /// loading; `skip_serializing_if` keeps files without anchors
    /// byte-identical to before the field existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    adapter_trust_roots: Vec<AdapterTrustRoot>,
}

/// Manager-written record of an external adapter root validated at enable.
///
/// This anchors either a contract-validated symlink target or a framework
/// home resolved and validated from the enable-time process environment.
/// Without it, an RPM update or later environment drift could make the
/// authoritative receipt impossible to verify or clean up. Only the Manager
/// writes this record (enable upserts, disable removes); drivers never see it.
/// OpenCode migrations can retain multiple exact targets for one component and
/// framework until both the replacement activation and prior cleanup complete.
///
/// Symlink validation consumes the anchor as an **exact-equality** target
/// allowance. Drivers whose native CLI owns a persisted framework home may
/// consume that home as an external IO root; the initial value must first
/// validate against the driver's environment-derived boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdapterTrustRoot {
    /// Component the anchored receipt belongs to.
    pub component: String,
    /// Framework of the anchored receipt.
    pub framework: String,
    /// The contract-validated resource root at enable time.
    pub root: PathBuf,
}

/// Minimal probe to route a file to the right parser without committing to
/// either full shape. `schema_version` has been present since v1.
#[derive(Deserialize)]
struct VersionProbe {
    #[serde(default)]
    schema_version: u32,
}

/// In-memory installed state under the authority model.
#[derive(Debug, Clone, PartialEq)]
pub struct StateStore {
    /// Install scope used to interpret paths in this state file.
    pub install_mode: InstallMode,
    /// Prefix recorded for diagnostics and future migrations.
    pub prefix: PathBuf,
    /// Active installations (one per kind/name/scope).
    pub installations: Vec<Installation>,
    /// Legacy records preserved verbatim, inert until `repair`/`forget`.
    pub quarantined: Vec<QuarantinedObject>,
    /// Backup metadata created by lifecycle transactions.
    pub backups: Vec<BackupRecord>,
    /// Lightweight operation history mirrored by central logs.
    pub operations: Vec<OperationRecord>,
    /// Adapter receipts written by `anolisa adapter enable`.
    pub adapter_claims: Vec<AdapterClaim>,
    /// Enable-time trust anchors for adapter receipts (see
    /// [`AdapterTrustRoot`]). Manager-written, never driver-supplied.
    pub adapter_trust_roots: Vec<AdapterTrustRoot>,
    /// Audit trail of the load-boundary migration, when one ran.
    pub migration_audit: Vec<(String, MigrationRule)>,
    /// Names of legacy capability objects the migration dropped.
    pub dropped_capabilities: Vec<String>,
    /// This load parsed a schema ≤ 4 file through the migration.
    migrated_from_legacy: bool,
}

impl StateStore {
    /// A fresh, empty store (first-run case).
    pub fn empty() -> Self {
        Self {
            install_mode: InstallMode::default(),
            prefix: PathBuf::new(),
            installations: Vec::new(),
            quarantined: Vec::new(),
            backups: Vec::new(),
            operations: Vec::new(),
            adapter_claims: Vec::new(),
            adapter_trust_roots: Vec::new(),
            migration_audit: Vec::new(),
            dropped_capabilities: Vec::new(),
            migrated_from_legacy: false,
        }
    }

    /// Create an empty store whose file metadata matches `layout`.
    pub fn empty_for_layout(layout: &FsLayout) -> Self {
        let mut store = Self::empty();
        store.set_layout_metadata(layout);
        store
    }

    fn set_layout_metadata(&mut self, layout: &FsLayout) {
        self.install_mode = match layout.mode {
            LayoutInstallMode::System => InstallMode::System,
            LayoutInstallMode::User => InstallMode::User,
        };
        self.prefix = layout.prefix.clone();
    }

    /// Load state for a concrete filesystem layout and enforce its scope.
    ///
    /// A state without lifecycle, recovery, backup, or adapter records may
    /// adopt the selected layout. Otherwise file metadata and every
    /// installation scope must already match; conflicting state is rejected
    /// before planning or save.
    pub fn load_for_layout(path: &Path, uid: u32, layout: &FsLayout) -> Result<Self, StateError> {
        let mut store = Self::load(path, uid)?;
        if store.installations.is_empty()
            && store.quarantined.is_empty()
            && store.backups.is_empty()
            && store.operations.is_empty()
            && store.adapter_claims.is_empty()
        {
            store.set_layout_metadata(layout);
            return Ok(store);
        }

        let (expected_mode, expected_scope, layout_label) = match layout.mode {
            LayoutInstallMode::System => (
                InstallMode::System,
                InstallationScope::System,
                "system".to_string(),
            ),
            LayoutInstallMode::User => (
                InstallMode::User,
                InstallationScope::User { uid },
                format!("user(uid={uid})"),
            ),
        };
        if store.install_mode != expected_mode || store.prefix != layout.prefix {
            return Err(StateError::LayoutMismatch {
                path: path.to_path_buf(),
                reason: format!(
                    "state metadata install_mode={}, prefix={} does not match the active {layout_label} layout install_mode={}, prefix={}",
                    install_mode_label(store.install_mode),
                    store.prefix.display(),
                    install_mode_label(expected_mode),
                    layout.prefix.display(),
                ),
            });
        }
        if let Some(installation) = store
            .installations
            .iter()
            .find(|installation| installation.scope != expected_scope)
        {
            return Err(StateError::LayoutMismatch {
                path: path.to_path_buf(),
                reason: format!(
                    "installation '{}' has scope {}, which does not match the active {layout_label} layout",
                    installation.name,
                    installation_scope_label(installation.scope),
                ),
            });
        }
        Ok(store)
    }

    /// Load state from `path`, migrating a legacy (schema ≤ 4) file in
    /// memory. A missing file is a fresh store, not an error.
    ///
    /// `uid` disambiguates the scope of a user-mode legacy file — the old
    /// format recorded only the mode, not whose user scope it was.
    pub fn load(path: &Path, uid: u32) -> Result<Self, StateError> {
        if !path.exists() {
            return Ok(Self::empty());
        }
        let content = fs::read_to_string(path).map_err(|source| StateError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let probe: VersionProbe = toml::from_str(&content).map_err(|source| StateError::Parse {
            path: path.to_path_buf(),
            source,
        })?;

        // Refuse files from a newer schema: serde would silently drop the
        // fields that schema keeps its records in, and the next save would
        // rewrite the file at our version — destroying data the newer
        // writer owned.
        if probe.schema_version > STORE_SCHEMA_VERSION_ANCHORED {
            return Err(StateError::NewerSchema {
                path: path.to_path_buf(),
                found: probe.schema_version,
                supported: STORE_SCHEMA_VERSION_ANCHORED,
            });
        }

        // v6 shares the v5 wire shape (plus anchors), so both parse here.
        if probe.schema_version >= STORE_SCHEMA_VERSION {
            let file: StateFileV5 =
                toml::from_str(&content).map_err(|source| StateError::Parse {
                    path: path.to_path_buf(),
                    source,
                })?;
            let mut installations = file.installations;
            repair_known_installation_identities(&mut installations);
            return Ok(Self {
                install_mode: file.install_mode,
                prefix: file.prefix,
                installations,
                quarantined: file.quarantined,
                backups: file.backups,
                operations: file.operations,
                adapter_claims: file.adapter_claims,
                adapter_trust_roots: file.adapter_trust_roots,
                migration_audit: Vec::new(),
                dropped_capabilities: Vec::new(),
                migrated_from_legacy: false,
            });
        }

        let legacy: InstalledState =
            toml::from_str(&content).map_err(|source| StateError::Parse {
                path: path.to_path_buf(),
                source,
            })?;
        let scope = match legacy.install_mode {
            InstallMode::System => InstallationScope::System,
            InstallMode::User => InstallationScope::User { uid },
        };
        let migration = migrate_state(&legacy.objects, scope);
        let mut installations = migration.active;
        repair_known_installation_identities(&mut installations);
        Ok(Self {
            install_mode: legacy.install_mode,
            prefix: legacy.prefix,
            installations,
            quarantined: migration.quarantined,
            backups: legacy.backups,
            operations: legacy.operations,
            adapter_claims: legacy.adapter_claims,
            // Legacy schemas predate trust anchors; receipts they carry
            // resolve inside datadir roots, which need no anchor.
            adapter_trust_roots: Vec::new(),
            migration_audit: migration.audit,
            dropped_capabilities: migration.dropped_capabilities,
            migrated_from_legacy: true,
        })
    }

    /// Whether this load ran the legacy migration (useful for surfacing the
    /// audit trail on the first write).
    pub fn migrated_from_legacy(&self) -> bool {
        self.migrated_from_legacy
    }

    /// Atomically persist the store: schema v6 when trust anchors are
    /// present (so pre-anchor writers refuse the file instead of dropping
    /// them — see [`STORE_SCHEMA_VERSION_ANCHORED`]), plain v5 otherwise
    /// (anchor-free files stay fully interchangeable with 0.2.16).
    ///
    /// If the on-disk file is still legacy (schema ≤ 4), its original bytes
    /// are first preserved as a `.v4.bak` sibling — exactly once: an
    /// existing backup is never overwritten, so the true pre-migration
    /// content survives repeated saves.
    pub fn save(&self, path: &Path) -> Result<(), StateError> {
        self.backup_legacy_file(path)?;

        let file = StateFileV5 {
            schema_version: if self.adapter_trust_roots.is_empty() {
                STORE_SCHEMA_VERSION
            } else {
                STORE_SCHEMA_VERSION_ANCHORED
            },
            updated_at: now_iso8601(),
            install_mode: self.install_mode,
            prefix: self.prefix.clone(),
            anolisa_version: env!("CARGO_PKG_VERSION").to_string(),
            installations: self.installations.clone(),
            quarantined: self.quarantined.clone(),
            backups: self.backups.clone(),
            operations: self.operations.clone(),
            adapter_claims: self.adapter_claims.clone(),
            adapter_trust_roots: self.adapter_trust_roots.clone(),
        };
        let content = toml::to_string_pretty(&file)?;
        write_atomic(path, content.as_bytes()).map_err(|source| StateError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(())
    }

    /// Copy a still-legacy state file aside before the first v5 write. The
    /// on-disk schema version is probed at save time (not trusted from the
    /// load) so a v5 file is never mislabelled as a v4 backup.
    fn backup_legacy_file(&self, path: &Path) -> Result<(), StateError> {
        let Ok(content) = fs::read_to_string(path) else {
            return Ok(()); // no existing file — nothing to preserve
        };
        let Ok(probe) = toml::from_str::<VersionProbe>(&content) else {
            return Ok(()); // unparsable content is not a legacy state file
        };
        if probe.schema_version >= STORE_SCHEMA_VERSION {
            return Ok(());
        }
        let backup = legacy_backup_path(path);
        if backup.exists() {
            return Ok(()); // first backup wins — it holds the original bytes
        }
        fs::copy(path, &backup)
            .map(|_| ())
            .map_err(|source| StateError::Io {
                path: backup,
                source,
            })
    }

    /// Active installation for `(kind, name)`, if any.
    pub fn find(&self, kind: ObjectKind, name: &str) -> Option<&Installation> {
        self.installations
            .iter()
            .find(|i| i.kind == kind && i.name == name)
    }

    /// Whether active or quarantined state owns the exact object name.
    ///
    /// Name resolution uses this before consulting package aliases so an
    /// inert quarantine remains directly addressable by `repair` and
    /// `forget` instead of being remapped to an unrelated component.
    pub fn contains_record(&self, kind: ObjectKind, name: &str) -> bool {
        self.find(kind, name).is_some()
            || self
                .quarantined
                .iter()
                .any(|entry| entry.record.kind == kind && entry.record.name == name)
    }

    /// Mutable access to an active installation.
    pub fn find_mut(&mut self, kind: ObjectKind, name: &str) -> Option<&mut Installation> {
        self.installations
            .iter_mut()
            .find(|i| i.kind == kind && i.name == name)
    }

    /// What the store knows about `(kind, name)` — the planner's record
    /// facts, straight from the aggregate.
    pub fn record_facts(&self, kind: ObjectKind, name: &str) -> RecordFacts {
        if let Some(installation) = self.find(kind, name) {
            return RecordFacts::Active(installation.clone());
        }
        if let Some(q) = self
            .quarantined
            .iter()
            .find(|q| q.record.kind == kind && q.record.name == name)
        {
            return RecordFacts::Quarantined(q.reason.clone());
        }
        RecordFacts::Absent
    }

    /// Insert or replace an installation, deduped by `(kind, name)`. Also
    /// consumes any quarantined record of the same identity — writing an
    /// active record *is* the repair exit for a quarantined one.
    pub fn upsert(&mut self, installation: Installation) {
        self.quarantined.retain(|q| {
            !(q.record.kind == installation.kind && q.record.name == installation.name)
        });
        if let Some(slot) = self.find_mut(installation.kind, &installation.name) {
            *slot = installation;
        } else {
            self.installations.push(installation);
        }
    }

    /// Remove the record for `(kind, name)` — active or quarantined.
    /// Returns whether anything was removed.
    pub fn remove(&mut self, kind: ObjectKind, name: &str) -> bool {
        let before = self.installations.len() + self.quarantined.len();
        self.installations
            .retain(|i| !(i.kind == kind && i.name == name));
        self.quarantined
            .retain(|q| !(q.record.kind == kind && q.record.name == name));
        before != self.installations.len() + self.quarantined.len()
    }

    /// Find an adapter receipt by `(component, framework)`.
    pub fn find_adapter_claim(&self, component: &str, framework: &str) -> Option<&AdapterClaim> {
        self.adapter_claims
            .iter()
            .find(|c| c.component == component && c.framework == framework)
    }

    /// Insert or replace an adapter receipt, deduped by
    /// `(component, framework)`.
    pub fn upsert_adapter_claim(&mut self, claim: AdapterClaim) {
        if let Some(slot) = self
            .adapter_claims
            .iter_mut()
            .find(|c| c.component == claim.component && c.framework == claim.framework)
        {
            *slot = claim;
        } else {
            self.adapter_claims.push(claim);
        }
    }

    /// Remove an adapter receipt by `(component, framework)`, returning the
    /// removed value.
    pub fn remove_adapter_claim(
        &mut self,
        component: &str,
        framework: &str,
    ) -> Option<AdapterClaim> {
        let idx = self
            .adapter_claims
            .iter()
            .position(|c| c.component == component && c.framework == framework)?;
        Some(self.adapter_claims.remove(idx))
    }

    /// Find the enable-time trust anchor for `(component, framework)`.
    pub fn find_adapter_trust_root(&self, component: &str, framework: &str) -> Option<&Path> {
        self.adapter_trust_roots
            .iter()
            .find(|a| a.component == component && a.framework == framework)
            .map(|a| a.root.as_path())
    }

    /// Insert or replace the trust anchor for `(component, framework)`.
    /// Called by the Manager only, with a contract-validated root.
    pub fn upsert_adapter_trust_root(&mut self, component: &str, framework: &str, root: PathBuf) {
        if let Some(slot) = self
            .adapter_trust_roots
            .iter_mut()
            .find(|a| a.component == component && a.framework == framework)
        {
            slot.root = root;
        } else {
            self.adapter_trust_roots.push(AdapterTrustRoot {
                component: component.to_string(),
                framework: framework.to_string(),
                root,
            });
        }
    }

    /// Drop the trust anchor for `(component, framework)`. Called when the
    /// receipt it anchored is removed.
    pub fn remove_adapter_trust_root(&mut self, component: &str, framework: &str) {
        self.adapter_trust_roots
            .retain(|a| !(a.component == component && a.framework == framework));
    }

    /// All adapter receipts for a component, across frameworks.
    pub fn adapter_claims_for_component(&self, component: &str) -> Vec<&AdapterClaim> {
        self.adapter_claims
            .iter()
            .filter(|c| c.component == component)
            .collect()
    }
}

const fn install_mode_label(mode: InstallMode) -> &'static str {
    match mode {
        InstallMode::System => "system",
        InstallMode::User => "user",
    }
}

fn installation_scope_label(scope: InstallationScope) -> String {
    match scope {
        InstallationScope::System => "system".to_string(),
        InstallationScope::User { uid } => format!("user(uid={uid})"),
    }
}

/// Sibling path preserving the legacy file's original bytes.
pub fn legacy_backup_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "installed.toml".to_string());
    path.with_file_name(format!("{file_name}.v4.bak"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        LifecycleStatus, ManagementRelation, NativePm, OwnedArtifact, PackageIdentity,
        ProviderBinding,
    };
    use crate::state_migration::QuarantineReason;

    /// A legacy v4 file with one object per migration bucket: a clean raw
    /// component, a dropped capability, an evidence-free quarantine, and an
    /// unknown-backend quarantine.
    const LEGACY_V4: &str = r#"
schema_version = 4
updated_at = "2026-07-01T00:00:00Z"
install_mode = "system"
prefix = "/"
anolisa_version = "0.2.4"

[[objects]]
kind = "component"
name = "cosh"
version = "2.7.0"
status = "installed"
install_backend = "raw"
ownership = "raw_managed"
distribution_source = "https://repo.example/raw/cosh-2.7.0.tar.gz"
installed_at = "2026-07-01T00:00:00Z"

[[objects.files]]
path = "/usr/local/bin/cosh"
owner = "anolisa"
sha256 = "aa"
kind = "file"

[[objects]]
kind = "capability"
name = "legacy-cap"
version = "1.0.0"
status = "installed"
installed_at = "2026-07-01T00:00:00Z"

[[objects]]
kind = "component"
name = "mystery"
version = "0.1.0"
status = "installed"
installed_at = "2026-07-01T00:00:00Z"

[[objects]]
kind = "component"
name = "flatpak-thing"
version = "0.2.0"
status = "installed"
install_backend = "flatpak"
installed_at = "2026-07-01T00:00:00Z"

[[operations]]
id = "op-1"
command = "install"
status = "ok"
started_at = "2026-07-01T00:00:00Z"
"#;

    const LEGACY_COSH_NG_RPM: &str = r#"
schema_version = 4
updated_at = "2026-07-01T00:00:00Z"
install_mode = "system"
prefix = "/"
anolisa_version = "0.2.4"

[[objects]]
kind = "component"
name = "cosh"
version = "0.13.0"
status = "installed"
install_backend = "rpm"
ownership = "rpm_managed"
installed_at = "2026-07-01T00:00:00Z"

[objects.rpm_metadata]
package_name = "cosh-ng"
evr = "0.13.0-1"
arch = "x86_64"
source_repo = "anolisa"
"#;

    fn write_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, content).expect("write fixture");
        path
    }

    fn owned_installation(name: &str, version: &str) -> Installation {
        Installation {
            kind: ObjectKind::Component,
            name: name.to_string(),
            scope: InstallationScope::System,
            binding: ProviderBinding::Owned {
                artifact: OwnedArtifact {
                    version: version.to_string(),
                    distribution_source: None,
                    raw_package: None,
                    manifest_digest: None,
                    files: Vec::new(),
                    services: Vec::new(),
                    external_modified_files: Vec::new(),
                    provisioned_packages: Vec::new(),
                },
            },
            status: LifecycleStatus::Installed,
            installed_at: "2026-07-16T00:00:00Z".to_string(),
            last_operation_id: None,
            subscription_scope: Default::default(),
            enabled_features: Vec::new(),
            health: Vec::new(),
        }
    }

    #[test]
    fn missing_file_loads_an_empty_store() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let store = StateStore::load(&tmp.path().join("installed.toml"), 1000).expect("load empty");
        assert!(store.installations.is_empty());
        assert!(store.quarantined.is_empty());
        assert!(!store.migrated_from_legacy());
    }

    #[test]
    fn scoped_load_initializes_empty_layout_metadata() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let layout = FsLayout::system(Some(tmp.path().join("system")));
        let path = layout.state_dir.join("installed.toml");

        let store = StateStore::load_for_layout(&path, 1000, &layout).expect("load empty");

        assert_eq!(store.install_mode, InstallMode::System);
        assert_eq!(store.prefix, layout.prefix);
    }

    #[test]
    fn scoped_load_rejects_file_metadata_mismatch() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let system_layout = FsLayout::system(Some(tmp.path().join("system")));
        let user_layout =
            FsLayout::user_with_overrides(tmp.path().join("home"), None, None, None, None, None);
        let path = system_layout.state_dir.join("installed.toml");
        let mut store = StateStore::empty();
        store.set_layout_metadata(&system_layout);
        store.upsert(owned_installation("cosh", "2.7.0"));
        store.save(&path).expect("save system state");

        let err = StateStore::load_for_layout(&path, 1000, &user_layout)
            .expect_err("metadata mismatch must fail");

        assert!(matches!(err, StateError::LayoutMismatch { .. }));
        assert!(err.to_string().contains("state metadata"));
    }

    #[test]
    fn scoped_load_rejects_operation_only_metadata_mismatch() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let system_layout = FsLayout::system(Some(tmp.path().join("system")));
        let user_layout =
            FsLayout::user_with_overrides(tmp.path().join("home"), None, None, None, None, None);
        let path = system_layout.state_dir.join("installed.toml");
        let mut store = StateStore::empty_for_layout(&user_layout);
        store.operations.push(OperationRecord {
            id: "op-1".to_string(),
            command: "install cosh".to_string(),
            status: "started".to_string(),
            started_at: "2026-07-21T00:00:00Z".to_string(),
            finished_at: None,
            parent_operation_id: None,
        });
        store.save(&path).expect("save user operation state");

        let err = StateStore::load_for_layout(&path, 1000, &system_layout)
            .expect_err("operation history must preserve its original scope");

        assert!(matches!(err, StateError::LayoutMismatch { .. }));
        assert!(err.to_string().contains("state metadata"));
    }

    #[test]
    fn scoped_load_rejects_installation_scope_mismatch() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let layout = FsLayout::system(Some(tmp.path().join("system")));
        let path = layout.state_dir.join("installed.toml");
        let mut store = StateStore::empty();
        store.set_layout_metadata(&layout);
        let mut installation = owned_installation("cosh", "2.7.0");
        installation.scope = InstallationScope::User { uid: 1000 };
        store.upsert(installation);
        store.save(&path).expect("save mismatched record");

        let err = StateStore::load_for_layout(&path, 1000, &layout)
            .expect_err("record scope mismatch must fail");

        assert!(matches!(err, StateError::LayoutMismatch { .. }));
        assert!(err.to_string().contains("installation 'cosh'"));
    }

    #[test]
    fn legacy_file_migrates_at_the_load_boundary() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = write_file(tmp.path(), "installed.toml", LEGACY_V4);

        let store = StateStore::load(&path, 1000).expect("load legacy");

        assert!(store.migrated_from_legacy());
        // cosh cleanly maps to an active Owned installation.
        assert_eq!(store.installations.len(), 1);
        let cosh = store.find(ObjectKind::Component, "cosh").expect("cosh");
        assert!(matches!(
            &cosh.binding,
            ProviderBinding::Owned { artifact } if artifact.version == "2.7.0"
        ));
        assert_eq!(cosh.scope, InstallationScope::System);
        // The capability object is dropped, the other two are quarantined.
        assert_eq!(store.dropped_capabilities, vec!["legacy-cap".to_string()]);
        assert_eq!(store.quarantined.len(), 2);
        assert!(matches!(
            store.record_facts(ObjectKind::Component, "mystery"),
            RecordFacts::Quarantined(QuarantineReason::NoEvidence)
        ));
        assert!(matches!(
            store.record_facts(ObjectKind::Component, "flatpak-thing"),
            RecordFacts::Quarantined(QuarantineReason::UnknownBackend { backend }) if backend == "flatpak"
        ));
        // Non-object payloads carry over untouched.
        assert_eq!(store.operations.len(), 1);
        assert_eq!(store.migration_audit.len(), 4);
    }

    /// A file written by a newer schema must be refused, not read as the
    /// current shape: serde would drop the newer schema's fields and the
    /// next save would rewrite the file, silently destroying that data.
    #[test]
    fn newer_schema_file_is_refused_and_left_untouched() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let future_v7 = "schema_version = 7\n\
                         updated_at = \"2026-07-20T00:00:00Z\"\n\
                         install_mode = \"system\"\n\
                         prefix = \"/\"\n\
                         anolisa_version = \"future\"\n\
                         future_only = \"must-survive\"\n";
        let path = write_file(tmp.path(), "installed.toml", future_v7);

        let err = StateStore::load(&path, 1000).expect_err("v7 must be refused");
        assert!(matches!(
            err,
            StateError::NewerSchema {
                found: 7,
                supported: STORE_SCHEMA_VERSION_ANCHORED,
                ..
            }
        ));
        // Refusal is read-only: the newer writer's bytes are untouched.
        assert_eq!(fs::read_to_string(&path).expect("read back"), future_v7);
    }

    /// Anchors force the schema to v6 on disk, so pre-anchor writers
    /// (≤ 0.2.16, which refuse schemas newer than v5) fail closed instead
    /// of silently deleting the security-critical anchor on their next
    /// save. The anchors themselves must round-trip.
    #[test]
    fn anchored_state_round_trips_at_v6() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("installed.toml");
        let mut store = StateStore::empty();
        store.install_mode = InstallMode::System;
        store.prefix = PathBuf::from("/");
        store.upsert_adapter_trust_root("sec-core", "codex", PathBuf::from("/opt/sec-core"));
        store.save(&path).expect("save anchored");

        let content = fs::read_to_string(&path).expect("read back");
        assert!(
            content.contains("schema_version = 6"),
            "anchored state must be marked v6, got:\n{content}"
        );

        let loaded = StateStore::load(&path, 0).expect("reload v6");
        assert_eq!(
            loaded.find_adapter_trust_root("sec-core", "codex"),
            Some(Path::new("/opt/sec-core")),
            "anchor must survive the round-trip"
        );
    }

    /// Without anchors nothing changed on the wire: the file stays v5 and
    /// fully interchangeable with pre-anchor writers.
    #[test]
    fn anchor_free_state_still_writes_v5() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("installed.toml");
        let mut store = StateStore::empty();
        store.install_mode = InstallMode::System;
        store.prefix = PathBuf::from("/");
        store.upsert_adapter_trust_root("sec-core", "codex", PathBuf::from("/opt/sec-core"));
        store.remove_adapter_trust_root("sec-core", "codex");
        store.save(&path).expect("save");

        let content = fs::read_to_string(&path).expect("read back");
        assert!(
            content.contains("schema_version = 5"),
            "anchor-free state must stay v5, got:\n{content}"
        );
        assert!(!content.contains("adapter_trust_roots"));
    }

    #[test]
    fn user_mode_legacy_scope_carries_the_uid() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let user_v4 = LEGACY_V4.replace("install_mode = \"system\"", "install_mode = \"user\"");
        let path = write_file(tmp.path(), "installed.toml", &user_v4);

        let store = StateStore::load(&path, 1000).expect("load legacy");

        let cosh = store.find(ObjectKind::Component, "cosh").expect("cosh");
        assert_eq!(cosh.scope, InstallationScope::User { uid: 1000 });
    }

    #[test]
    fn first_save_backs_up_the_legacy_file_and_writes_v5() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = write_file(tmp.path(), "installed.toml", LEGACY_V4);

        let store = StateStore::load(&path, 1000).expect("load legacy");
        store.save(&path).expect("save v5");

        // The original bytes survive as the backup sibling.
        let backup = legacy_backup_path(&path);
        assert_eq!(fs::read_to_string(&backup).expect("read backup"), LEGACY_V4);
        // The main file is now v5 and loads without migration.
        let reloaded = StateStore::load(&path, 1000).expect("reload");
        assert!(!reloaded.migrated_from_legacy());
        assert_eq!(reloaded.installations, store.installations);
        assert_eq!(reloaded.quarantined, store.quarantined);
        assert_eq!(reloaded.operations, store.operations);
    }

    #[test]
    fn later_saves_never_overwrite_the_first_backup() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = write_file(tmp.path(), "installed.toml", LEGACY_V4);
        let backup = legacy_backup_path(&path);

        let mut store = StateStore::load(&path, 1000).expect("load legacy");
        store.save(&path).expect("first save");

        // Plant a marker: if a later save re-created the backup from the
        // (now v5) main file, the marker would vanish.
        let original = fs::read_to_string(&backup).expect("read backup");
        store.upsert(owned_installation("tokenless", "0.7.0"));
        store.save(&path).expect("second save");

        assert_eq!(
            fs::read_to_string(&backup).expect("re-read backup"),
            original
        );
    }

    #[test]
    fn read_only_load_never_touches_the_legacy_file() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = write_file(tmp.path(), "installed.toml", LEGACY_V4);

        let _store = StateStore::load(&path, 1000).expect("load legacy");

        // Loading is pure: same bytes, no v5 file, no backup.
        assert_eq!(fs::read_to_string(&path).expect("re-read"), LEGACY_V4);
        assert!(!legacy_backup_path(&path).exists());
    }

    #[test]
    fn v5_roundtrip_preserves_all_buckets() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("installed.toml");

        let mut store = StateStore::empty();
        store.install_mode = InstallMode::System;
        store.upsert(owned_installation("cosh", "2.7.0"));
        store.upsert(Installation {
            binding: ProviderBinding::Delegated {
                pm: NativePm::Rpm,
                package: PackageIdentity::Resolved {
                    name: "agentsight".to_string(),
                },
                relation: ManagementRelation::Adopted {
                    since: "2026-07-16T00:00:00Z".to_string(),
                },
                last_observed: None,
            },
            ..owned_installation("agentsight", "0.8.0")
        });
        store.save(&path).expect("save");

        let reloaded = StateStore::load(&path, 1000).expect("reload");
        assert_eq!(reloaded.installations, store.installations);
        assert!(matches!(
            reloaded
                .find(ObjectKind::Component, "agentsight")
                .expect("agentsight")
                .binding,
            ProviderBinding::Delegated {
                relation: ManagementRelation::Adopted { .. },
                ..
            }
        ));
    }

    #[test]
    fn v5_load_repairs_cosh_ng_rpm_identity_in_memory() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("installed.toml");
        let mut store = StateStore::empty();
        store.upsert(Installation {
            binding: ProviderBinding::Delegated {
                pm: NativePm::Rpm,
                package: PackageIdentity::Resolved {
                    name: "cosh-ng".to_string(),
                },
                relation: ManagementRelation::Managed {
                    since: "2026-07-01T00:00:00Z".to_string(),
                },
                last_observed: None,
            },
            ..owned_installation("cosh", "0.13.0")
        });
        store.save(&path).expect("save legacy identity as v5");

        let reloaded = StateStore::load(&path, 1000).expect("reload");

        assert!(
            reloaded.find(ObjectKind::Component, "cosh").is_none(),
            "the incorrect identity must disappear from active state"
        );
        assert!(
            reloaded.find(ObjectKind::Component, "cosh-ng").is_some(),
            "the backing package identifies the canonical component"
        );
    }

    #[test]
    fn legacy_load_repairs_cosh_ng_rpm_identity_after_migration() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = write_file(tmp.path(), "installed.toml", LEGACY_COSH_NG_RPM);

        let store = StateStore::load(&path, 1000).expect("load legacy RPM record");

        assert!(store.migrated_from_legacy());
        assert!(store.find(ObjectKind::Component, "cosh").is_none());
        assert!(store.find(ObjectKind::Component, "cosh-ng").is_some());
    }

    #[test]
    fn upsert_consumes_a_quarantined_record_of_the_same_identity() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = write_file(tmp.path(), "installed.toml", LEGACY_V4);
        let mut store = StateStore::load(&path, 1000).expect("load legacy");
        assert!(matches!(
            store.record_facts(ObjectKind::Component, "mystery"),
            RecordFacts::Quarantined(_)
        ));

        // Repair resolved the mystery record into a real installation.
        store.upsert(owned_installation("mystery", "0.1.0"));

        assert!(matches!(
            store.record_facts(ObjectKind::Component, "mystery"),
            RecordFacts::Active(_)
        ));
        assert_eq!(store.quarantined.len(), 1, "only flatpak-thing remains");
    }

    #[test]
    fn remove_drops_active_and_quarantined_records() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = write_file(tmp.path(), "installed.toml", LEGACY_V4);
        let mut store = StateStore::load(&path, 1000).expect("load legacy");

        assert!(store.contains_record(ObjectKind::Component, "cosh"));
        assert!(store.contains_record(ObjectKind::Component, "mystery"));

        assert!(store.remove(ObjectKind::Component, "cosh"));
        assert!(store.remove(ObjectKind::Component, "mystery"));
        assert!(!store.remove(ObjectKind::Component, "cosh"), "already gone");

        assert!(matches!(
            store.record_facts(ObjectKind::Component, "cosh"),
            RecordFacts::Absent
        ));
        assert!(matches!(
            store.record_facts(ObjectKind::Component, "mystery"),
            RecordFacts::Absent
        ));
    }

    #[test]
    fn corrupt_file_is_a_parse_error_not_a_fresh_store() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = write_file(tmp.path(), "installed.toml", "schema_version = [broken");

        let err = StateStore::load(&path, 1000).unwrap_err();
        assert!(matches!(err, StateError::Parse { .. }));
    }
}

//! Legacy installed-state wire types and shared persistence primitives.
//!
//! Current state access uses [`crate::state_store::StateStore`]; legacy
//! records remain readable at its migration boundary.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::adapter::claim::AdapterClaim;
use crate::manifest::ServiceScope;

/// Last legacy `installed.toml` schema version; current writes use `StateStore`.
/// Legacy versions are migrated by [`crate::state_store::StateStore::load`]
/// into the current in-memory shape before returning.
///
/// v2 added the `adapter_claims` array (adapter receipts). The field
/// default-deserializes, so v1 files load unchanged and are silently
/// upgraded to v2 on the next save.
///
/// v3 added `ownership` (provenance model) and `rpm_metadata` to
/// [`InstalledObject`]. Both fields default-deserialize (`None`), so
/// older files load unchanged and gain the new fields on next save.
///
/// v4 added `kind` ([`OwnedFileKind`]) and `referent` to [`OwnedFile`]
/// so the integrity probe can distinguish managed symlinks from regular
/// files. Both default-deserialize (`File` / `None`); pre-v4 symlink
/// entries remain `kind = File` until migrated by
/// `commands::common::migrate_v3_symlinks`, which uses the installed
/// component manifest as the migration authority.
pub const STATE_SCHEMA_VERSION: u32 = 4;

pub(crate) fn is_legacy_rpm_backend(backend: Option<&str>) -> bool {
    matches!(backend, Some("rpm" | "yum"))
}

/// Default for `bool` fields that should serialise to `true` when absent.
fn default_true() -> bool {
    true
}

/// Install mode reported in `installed.toml`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstallMode {
    /// Per-user (`file-hierarchy(7)`) install scope.
    #[default]
    User,
    /// System-wide FHS install scope.
    System,
}

/// Discriminator for objects tracked in installed state.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    /// Legacy capability object. The capability concept is removed; the
    /// variant survives only so `installed.toml` files written by older
    /// releases still deserialize. New code must never create objects of
    /// this kind; queries are limited to legacy-migration paths (see
    /// [`crate::state_migration::migrate_object`]).
    Capability,
    /// Runtime/osbase component.
    Component,
    /// Agent-framework adapter object.
    Adapter,
    /// OS base-layer object.
    Osbase,
}

/// Lifecycle status for an installed object.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObjectStatus {
    /// Object is fully installed and active.
    Installed,
    /// Object was partially installed or has a degraded dependency.
    Partial,
    /// Object is present but intentionally inactive.
    Disabled,
    /// Last mutating operation failed or health checks found a hard error.
    Failed,
    /// Object is tracked but not fully owned by ANOLISA.
    Adopted,
}

/// Subscription scope attached to an object.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionScope {
    /// No subscription entitlement is attached.
    #[default]
    None,
    /// Registered with a subscription backend.
    Registered,
    /// Entitlement was granted for this object.
    Entitled,
    /// Object reports usage or health to a subscription backend.
    Reporting,
}

/// Provenance and lifecycle ownership of an installed object.
///
/// Determines who holds removal authority and how upgrades are executed.
/// See `raw_rpm_lifecycle_proposal.md` §5 for the full ownership table.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Ownership {
    /// Installed via ANOLISA raw backend; ANOLISA manages owned files and
    /// may remove them on uninstall.
    RawManaged,
    /// Installed via ANOLISA-delegated RPM backend (`dnf install`);
    /// file transactions are owned by rpm/dnf, uninstall delegates to
    /// `dnf remove`.
    RpmManaged,
    /// Pre-existing system RPM tracked without package-removal authority.
    RpmObserved,
}

/// RPM package metadata recorded when a component is managed or observed
/// through an RPM backend. Populated from `rpmdb` queries at adopt/install
/// time; refreshed on `repair` and `update`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RpmMetadata {
    /// RPM package name (e.g. `copilot-shell`).
    pub package_name: String,
    /// Full EVR (epoch:version-release) string from rpmdb.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evr: Option<String>,
    /// Package architecture (`x86_64`, `aarch64`, `noarch`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    /// Source repository or label that supplied the package (e.g.
    /// `@System`, `anolisa-release`, `alinux-updates`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_repo: Option<String>,
}

/// File ownership: ANOLISA-owned vs. external (third-party).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileOwner {
    /// ANOLISA may install, verify, remove, and roll back this file.
    Anolisa,
    /// ANOLISA must preserve this file and only touch it through explicit
    /// external-file backup contracts.
    External,
}

/// Whether an [`OwnedFile`] is a regular file or a managed symlink.
///
/// Older `installed.toml` files (schema ≤ 3) lack this field; serde
/// defaults to `File` so they load unchanged. New installs of symlink
/// entries record `Symlink` together with a `referent` path so the
/// integrity probe can verify the link target instead of refusing it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum OwnedFileKind {
    /// Immutable regular file (data, executable, library).
    #[default]
    File,
    /// Administrator-editable regular file; integrity checks skip its digest.
    Config,
    /// Symbolic link created by the install runner. The integrity probe
    /// verifies `readlink` against the recorded [`OwnedFile::referent`]
    /// instead of hashing content through the link.
    Symlink,
}

fn is_default_owned_file_kind(kind: &OwnedFileKind) -> bool {
    *kind == OwnedFileKind::File
}

/// File installed and owned by ANOLISA.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OwnedFile {
    /// Absolute path recorded at install time; status probes revalidate it
    /// against owned roots before any filesystem access.
    pub path: PathBuf,
    /// Ownership contract for uninstall and integrity checks.
    pub owner: FileOwner,
    /// Recorded content digest. Older state or externally adopted files may
    /// omit it, so integrity checks surface `unverified` instead of guessing.
    /// Symlink entries omit this field — they record a [`referent`](Self::referent)
    /// instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Immutable file, editable config, or managed symlink. Defaults to `File`
    /// for backward compatibility with state written before schema v4.
    #[serde(default, skip_serializing_if = "is_default_owned_file_kind")]
    pub kind: OwnedFileKind,
    /// Expected symlink target (only meaningful when `kind == Symlink`).
    /// The integrity probe verifies `readlink` matches this path and that
    /// the referent stays within ANOLISA-owned roots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referent: Option<PathBuf>,
    /// Expected Unix permission bits, normalized as a four-digit octal
    /// string (for example, `"0755"`). Older v5 records omit this field and
    /// are enriched from their saved component manifest before probing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Linux file capabilities that were successfully applied at install
    /// time. Each name uses the manifest vocabulary (for example,
    /// `"CAP_BPF"`); an empty list means no capability contract is known.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

/// External (non-ANOLISA) file that an operation modified. Linked back to
/// the originating [`BackupRecord`] by `backup_id`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalModifiedFile {
    /// Absolute path of the third-party file touched by an operation.
    pub path: PathBuf,
    /// Ownership marker; should remain [`FileOwner::External`] so uninstall
    /// refuses deletion.
    pub owner: FileOwner,
    /// Backup record that can restore the pre-operation content.
    pub backup_id: String,
    /// Digest before modification, when the file was readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256_before: Option<String>,
    /// Digest after modification, when ANOLISA can verify its own write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256_after: Option<String>,
}

/// Service unit installed or managed by an object.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceRef {
    /// Native unit name such as `agentsight.service`.
    pub name: String,
    /// Service manager namespace (`systemd`, `launchd`, `none`, ...).
    pub manager: String,
    /// Whether `anolisa restart` may target this unit.
    #[serde(default)]
    pub restartable: bool,
    /// Desired enabled-on-boot state when a manager supports it.
    #[serde(default)]
    pub enabled: bool,
    /// Manager scope: `system` units are driven by `systemctl`, `user`
    /// units by `systemctl --user`. Persisted so uninstall can pick the
    /// right manager. State files written before this field deserialize as
    /// [`ServiceScope::System`].
    #[serde(default)]
    pub scope: ServiceScope,
}

/// Last-known health probe result for an object.
///
/// `reason` is an optional human-readable detail that callers (status
/// renderer, JSON wire) surface alongside the status label. Manifest-driven
/// probes (file existence, command exit, systemd unit state) populate it
/// with a short pointer at why the check landed where it did so a user can
/// triage without re-running the probe by hand. Older state files written
/// before this field existed deserialize with `reason = None`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthEntry {
    /// Probe name or manifest health-check identifier.
    pub name: String,
    /// Status label rendered by `anolisa status`.
    pub status: String,
    /// RFC3339 UTC timestamp when the probe last ran.
    pub checked_at: String,
    /// Optional explanation for non-obvious status outcomes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A single installed object (component, adapter, or osbase).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledObject {
    /// Object vocabulary used by commands and state lookup.
    pub kind: ObjectKind,
    /// Stable object name from the manifest/catalog.
    pub name: String,
    /// Version installed or adopted into state.
    pub version: String,
    /// Lifecycle state used by list/status filters.
    pub status: ObjectStatus,
    /// Digest of the manifest used for install. Optional for older state and
    /// adopted objects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_digest: Option<String>,
    /// Distribution entry URL or backend-specific source that supplied bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribution_source: Option<String>,
    /// Raw backend package this component resolved to at install time.
    ///
    /// Preserves a `--package` override (or any package that differs from the
    /// component name) so a later `update` re-fetches the same package instead
    /// of re-deriving a possibly different one from repo.toml. `None` for
    /// non-raw installs and for raw state written before this field existed;
    /// update then falls back to deriving the package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_package: Option<String>,
    /// Backend that resolved and installed this object.
    ///
    /// Install refuses a later attempt through a different backend so a
    /// component's provenance stays deterministic across updates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_backend: Option<String>,
    /// Provenance/ownership class for lifecycle decisions (removal,
    /// upgrade delegation). `None` on state files written before v3;
    /// callers fall back to inspecting `managed` / `adopted` /
    /// `install_backend` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership: Option<Ownership>,
    /// RPM metadata populated when [`ownership`](Self::ownership) is
    /// [`RpmManaged`](Ownership::RpmManaged) or
    /// [`RpmObserved`](Ownership::RpmObserved). `None` for raw installs
    /// and pre-v3 state files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpm_metadata: Option<RpmMetadata>,
    /// RFC3339 UTC timestamp when this object entered state.
    pub installed_at: String,
    /// Last operation that changed this object, shared with central log rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_operation_id: Option<String>,
    /// False for externally adopted objects that ANOLISA should not mutate as
    /// normal owned installs.
    #[serde(default = "default_true")]
    pub managed: bool,
    /// Explicit adoption marker kept separate from `managed` for UI/audit
    /// vocabulary.
    #[serde(default)]
    pub adopted: bool,
    /// Subscription entitlement attached to this object.
    #[serde(default)]
    pub subscription_scope: SubscriptionScope,
    /// Enabled feature names, omitted from TOML when empty to preserve compact
    /// state files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enabled_features: Vec<String>,
    /// Legacy capability-to-component linkage; retained so old state files
    /// still deserialize. Component objects leave it empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub component_refs: Vec<String>,
    /// ANOLISA-owned files that status/uninstall may verify or remove.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<OwnedFile>,
    /// Third-party files touched under explicit backup contracts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_modified_files: Vec<ExternalModifiedFile>,
    /// Service units associated with this object.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ServiceRef>,
    /// Cached health results from the last status/probe pass.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub health: Vec<HealthEntry>,
    /// System packages that were auto-installed by the provisioner during
    /// this component's install (system mode only). Tracked so `status` can
    /// report them and `uninstall` can hint at orphan cleanup. Never
    /// auto-removed on uninstall.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provisioned_packages: Vec<String>,
}

/// Backup metadata recorded when an operation touched an external file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupRecord {
    /// Stable backup identifier used by state and logs.
    pub id: String,
    /// Operation that created this backup.
    pub operation_id: String,
    /// Original file path before the mutating operation.
    pub original_path: PathBuf,
    /// Backup copy path under the ANOLISA backup root.
    pub backup_path: PathBuf,
    /// Strategy hint for future repair tooling.
    pub restore_strategy: String,
}

/// Operation record for an `installed.toml` audit trail entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationRecord {
    /// Operation id shared with central logs and transaction journals.
    pub id: String,
    /// User-facing command or operation verb.
    pub command: String,
    /// Terminal status label (`started`, `ok`, `failed`, ...).
    pub status: String,
    /// RFC3339 UTC start timestamp.
    pub started_at: String,
    /// RFC3339 UTC finish timestamp; absent while an operation is in flight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// Batch operation this record belongs to — its members shared one
    /// native transaction (`install --all`, `update all`). Absent for
    /// standalone operations and for state written before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_operation_id: Option<String>,
}

/// Legacy on-disk record retained for deserialization and migration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledState {
    /// On-disk schema version for migration decisions.
    pub schema_version: u32,
    /// RFC3339 UTC timestamp recorded by the legacy writer.
    pub updated_at: String,
    /// Install scope used to interpret paths in this state file.
    pub install_mode: InstallMode,
    /// Prefix recorded for diagnostics and future migrations.
    pub prefix: PathBuf,
    /// ANOLISA version that last wrote the state file.
    pub anolisa_version: String,
    /// Installed/adopted objects tracked by name and kind.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<InstalledObject>,
    /// Backup metadata created by lifecycle transactions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backups: Vec<BackupRecord>,
    /// Lightweight operation history mirrored by central logs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<OperationRecord>,
    /// Adapter receipts written by `anolisa adapter enable`. Per-user
    /// state: each records a framework driver's takeover of framework-side
    /// state for one component. Empty on fresh and pre-v2 state files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adapter_claims: Vec<AdapterClaim>,
}

impl Default for InstalledState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            updated_at: now_iso8601(),
            install_mode: InstallMode::User,
            prefix: PathBuf::new(),
            anolisa_version: env!("CARGO_PKG_VERSION").to_string(),
            objects: Vec::new(),
            backups: Vec::new(),
            operations: Vec::new(),
            adapter_claims: Vec::new(),
        }
    }
}

/// Errors raised while loading or persisting installed state.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// Filesystem error while reading or writing state.
    #[error("io error while accessing {path}: {source}")]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying IO error.
        #[source]
        source: io::Error,
    },
    /// TOML parse error while loading state.
    #[error("failed to parse installed state at {path}: {source}")]
    Parse {
        /// State path being parsed.
        path: PathBuf,
        /// TOML parser error.
        #[source]
        source: toml::de::Error,
    },
    /// TOML serialization error while saving state.
    #[error("failed to serialize installed state: {0}")]
    Serialize(#[from] toml::ser::Error),
    /// The on-disk file was written by a newer schema this reader cannot
    /// represent.
    #[error(
        "installed state at {path} uses schema version {found} (this reader \
         understands up to {supported}); refusing to read it as if it were empty"
    )]
    NewerSchema {
        /// State path that carries the newer schema.
        path: PathBuf,
        /// Schema version found on disk.
        found: u32,
        /// Highest schema version this reader supports.
        supported: u32,
    },
    /// File-level metadata or a record scope conflicts with the state root
    /// selected by the caller.
    #[error("installed state at {path} does not match the active layout: {reason}")]
    LayoutMismatch {
        /// State path whose scope contract is inconsistent.
        path: PathBuf,
        /// Expected and observed layout facts.
        reason: String,
    },
}

#[cfg(test)]
pub(crate) fn write_legacy_fixture(state: &InstalledState, path: &Path) -> io::Result<()> {
    fs::create_dir_all(path.parent().expect("fixture parent"))?;
    fs::write(path, toml::to_string_pretty(state).expect("legacy fixture"))
}

pub(crate) fn now_iso8601() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Monotonic, process-wide counter mixed into [`tmp_path_for`] so that
/// concurrent writers on the same `path` don't pick the same tmp name.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a unique tmp sibling path for `path`.
///
/// Pattern: `.{file_name}.{pid}.{counter}.{nanos}.tmp`. Combined with
/// `O_CREAT|O_EXCL` in [`open_excl_nofollow`], a stale tmp (or a hostile
/// plant) at the *exact* generated path is a hard error, not a silent
/// overwrite. Mirrors the pattern in `transaction::tmp_path_for`.
fn tmp_path_for(path: &Path) -> PathBuf {
    let mut tmp = path.to_path_buf();
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "installed.toml".to_string());
    let pid = std::process::id();
    let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    tmp.set_file_name(format!(".{file_name}.{pid}.{counter}.{nanos}.tmp"));
    tmp
}

/// Open `tmp` for writing with `O_CREAT|O_EXCL` (+ `O_NOFOLLOW` on Unix).
/// Mirrors `transaction::open_excl_nofollow`.
fn open_excl_nofollow(tmp: &Path) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(nix::libc::O_NOFOLLOW);
    }
    opts.open(tmp)
}

/// `tmp` + `rename` write so a crash mid-write cannot leave a truncated
/// file. Mirrors `transaction::write_atomic`.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let tmp = tmp_path_for(path);
    let mut f = open_excl_nofollow(&tmp)?;
    if let Err(err) = f.write_all(bytes) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    let _ = f.sync_all();
    drop(f);
    if let Err(err) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_file_metadata_defaults_and_round_trips() {
        let legacy: OwnedFile = toml::from_str(
            r#"
path = "/usr/local/bin/tool"
owner = "anolisa"
sha256 = "deadbeef"
"#,
        )
        .expect("legacy owned file");
        assert!(legacy.mode.is_none());
        assert!(legacy.capabilities.is_empty());

        let mut current = legacy;
        current.mode = Some("0755".to_string());
        current.capabilities = vec!["CAP_BPF".to_string()];
        let encoded = toml::to_string(&current).expect("serialize");
        let decoded: OwnedFile = toml::from_str(&encoded).expect("deserialize");

        assert_eq!(decoded, current);
        assert!(encoded.contains("mode = \"0755\""));
        assert!(encoded.contains("capabilities = [\"CAP_BPF\"]"));
    }

    #[test]
    fn service_ref_scope_defaults_to_system_when_absent() {
        // State files written before `scope` existed must load as System so
        // uninstall keeps driving them through the (root) system manager.
        let legacy: ServiceRef = toml::from_str(
            "name = \"agentsight.service\"\nmanager = \"systemd\"\nrestartable = true\nenabled = true\n",
        )
        .expect("legacy ServiceRef parses");
        assert_eq!(legacy.scope, ServiceScope::System);

        let user: ServiceRef = toml::from_str(
            "name = \"anolisa-memory@alice.service\"\nmanager = \"systemd-user\"\nscope = \"user\"\n",
        )
        .expect("user-scope ServiceRef parses");
        assert_eq!(user.scope, ServiceScope::User);
    }

    fn sample_object(kind: ObjectKind, name: &str, version: &str) -> InstalledObject {
        InstalledObject {
            kind,
            name: name.to_string(),
            version: version.to_string(),
            status: ObjectStatus::Installed,
            manifest_digest: Some("sha256:abc".to_string()),
            distribution_source: Some("builtin".to_string()),
            raw_package: None,
            install_backend: Some("raw".to_string()),
            ownership: Some(Ownership::RawManaged),
            rpm_metadata: None,
            installed_at: now_iso8601(),
            last_operation_id: Some("op-1".to_string()),
            managed: true,
            adopted: false,
            subscription_scope: SubscriptionScope::None,
            enabled_features: vec!["alpha".to_string()],
            component_refs: vec!["agentsight".to_string()],
            files: vec![OwnedFile {
                path: PathBuf::from("/tmp/anolisa/bin/foo"),
                owner: FileOwner::Anolisa,
                sha256: Some("deadbeef".to_string()),
                kind: OwnedFileKind::File,
                referent: None,
                mode: None,
                capabilities: Vec::new(),
            }],
            external_modified_files: Vec::new(),
            services: vec![ServiceRef {
                name: "foo.service".to_string(),
                manager: "systemd".to_string(),
                restartable: true,
                enabled: true,
                scope: ServiceScope::System,
            }],
            health: vec![HealthEntry {
                name: "binary".to_string(),
                status: "ok".to_string(),
                checked_at: now_iso8601(),
                reason: None,
            }],
            provisioned_packages: Vec::new(),
        }
    }

    fn migrate_fixture(object: &InstalledObject) -> crate::domain::Installation {
        let result = crate::state_migration::migrate_object(
            object,
            crate::domain::InstallationScope::System,
        );
        match result.outcome {
            crate::state_migration::MigrationOutcome::Active(installation) => installation,
            other => panic!("expected active installation, got {other:?}"),
        }
    }

    fn sample_backup(id: &str, op: &str) -> BackupRecord {
        BackupRecord {
            id: id.to_string(),
            operation_id: op.to_string(),
            original_path: PathBuf::from("/etc/openclaw/config.toml"),
            backup_path: PathBuf::from("/var/lib/anolisa/backups/op-1/openclaw/config.toml"),
            restore_strategy: "replace-file".to_string(),
        }
    }

    fn sample_operation(id: &str) -> OperationRecord {
        OperationRecord {
            id: id.to_string(),
            command: "enable agent-observability".to_string(),
            status: "ok".to_string(),
            started_at: now_iso8601(),
            finished_at: Some(now_iso8601()),
            parent_operation_id: None,
        }
    }

    #[test]
    fn default_state_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("installed.toml");

        let state = InstalledState::default();
        write_legacy_fixture(&state, &path).expect("save default");

        let loaded =
            toml::from_str::<InstalledState>(&fs::read_to_string(&path).expect("read fixture"))
                .expect("load default");
        assert_eq!(loaded.schema_version, STATE_SCHEMA_VERSION);
        assert_eq!(loaded.install_mode, InstallMode::User);
        assert_eq!(loaded.anolisa_version, env!("CARGO_PKG_VERSION"));
        assert!(loaded.objects.is_empty());
        assert!(loaded.backups.is_empty());
        assert!(loaded.operations.is_empty());
    }

    /// The batch parent link is a soft schema extension: files written
    /// before the field existed must load with `None`, and standalone
    /// operations must not serialize the key at all — an old anolisa
    /// reading a new file only ever sees keys it knows.
    #[test]
    fn operation_parent_link_roundtrips_and_stays_optional() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("installed.toml");

        let mut state = InstalledState::default();
        state.operations.push(sample_operation("op-batch-1"));
        state.operations.push(OperationRecord {
            parent_operation_id: Some("op-batch-1".to_string()),
            ..sample_operation("op-member-1")
        });
        write_legacy_fixture(&state, &path).expect("save");

        let raw = fs::read_to_string(&path).expect("read raw toml");
        assert_eq!(
            raw.matches("parent_operation_id").count(),
            1,
            "only the member serializes the key:\n{raw}"
        );

        let loaded =
            toml::from_str::<InstalledState>(&fs::read_to_string(&path).expect("read fixture"))
                .expect("load");
        assert_eq!(loaded.operations[0].parent_operation_id, None);
        assert_eq!(
            loaded.operations[1].parent_operation_id.as_deref(),
            Some("op-batch-1")
        );
    }

    #[test]
    fn parse_template_round_trip() {
        let template_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("templates")
            .join("installed-state.toml");
        let content = fs::read_to_string(&template_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", template_path.display()));
        let state: InstalledState =
            toml::from_str(&content).expect("template parses into InstalledState");

        assert_eq!(state.schema_version, STATE_SCHEMA_VERSION);
        assert_eq!(state.install_mode, InstallMode::User);
        assert!(!state.objects.is_empty(), "expected at least one object");
        assert!(!state.backups.is_empty(), "expected at least one backup");
        assert!(
            !state.operations.is_empty(),
            "expected at least one operation"
        );

        let comp = state
            .objects
            .iter()
            .find(|o| o.kind == ObjectKind::Component)
            .expect("template has component object");
        assert_eq!(comp.name, "agentsight");
        assert!(!comp.external_modified_files.is_empty());
        assert_eq!(
            comp.external_modified_files[0].backup_id,
            state.backups[0].id
        );
    }

    /// State files written before the capability concept was removed still
    /// carry `kind = "capability"` objects; loading must not reject them.
    #[test]
    fn legacy_capability_object_still_deserializes() {
        let toml_text = r#"
            schema_version = 1
            updated_at = "2026-06-01T10:00:00Z"
            install_mode = "user"
            prefix = "~/.local"
            anolisa_version = "0.1.0"

            [[objects]]
            kind = "capability"
            name = "agent-observability"
            version = "0.1.0"
            status = "installed"
            installed_at = "2026-06-01T10:00:00Z"
        "#;
        let state: InstalledState = toml::from_str(toml_text).expect("legacy state parses");
        assert_eq!(state.objects[0].kind, ObjectKind::Capability);
    }

    /// Future schemas must not be read as an empty store and overwritten.
    #[test]
    fn load_rejects_newer_schema_instead_of_reading_it_as_empty() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("installed.toml");
        fs::write(
            &path,
            r#"
            schema_version = 999
            updated_at = "2026-07-16T10:00:00Z"
            install_mode = "user"
            prefix = "~/.local"
            anolisa_version = "0.3.0"
            "#,
        )
        .expect("write state");

        let err = crate::state_store::StateStore::load(&path, 1000).unwrap_err();

        match err {
            StateError::NewerSchema {
                found, supported, ..
            } => {
                assert_eq!(found, 999);
                assert_eq!(supported, crate::state_store::STORE_SCHEMA_VERSION_ANCHORED);
            }
            other => panic!("expected NewerSchema, got {other:?}"),
        }
    }

    #[test]
    fn load_boundary_drops_only_legacy_capability_objects() {
        let mut state = InstalledState::default();
        state.objects.push(sample_object(
            ObjectKind::Capability,
            "agent-observability",
            "0.1.0",
        ));
        state
            .objects
            .push(sample_object(ObjectKind::Component, "agentsight", "0.2.0"));

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("installed.toml");
        write_legacy_fixture(&state, &path).expect("save legacy fixture");
        let before = fs::read(&path).expect("read fixture");
        let migrated = crate::state_store::StateStore::load(&path, 1000).expect("migrate");

        assert_eq!(
            migrated.dropped_capabilities,
            vec!["agent-observability".to_string()]
        );
        assert_eq!(migrated.installations.len(), 1);
        assert_eq!(migrated.installations[0].kind, ObjectKind::Component);
        assert_eq!(migrated.installations[0].name, "agentsight");
        assert!(migrated.quarantined.is_empty());
        assert_eq!(fs::read(&path).expect("read after load"), before);

        migrated.save(&path).expect("persist migrated state");
        let reloaded = crate::state_store::StateStore::load(&path, 1000).expect("reload");
        assert!(reloaded.dropped_capabilities.is_empty());
        assert_eq!(reloaded.installations, migrated.installations);
    }

    #[test]
    fn upsert_then_find_installation() {
        let mut state = crate::state_store::StateStore::empty();
        let first = sample_object(ObjectKind::Component, "agentsight", "0.1.0");
        state.upsert(migrate_fixture(&first));
        assert_eq!(
            state
                .find(ObjectKind::Component, "agentsight")
                .expect("present")
                .binding
                .version(),
            Some("0.1.0")
        );
        let second = sample_object(ObjectKind::Component, "agentsight", "0.2.0");
        state.upsert(migrate_fixture(&second));
        assert_eq!(state.installations.len(), 1, "upsert dedupes by identity");
        assert_eq!(
            state
                .find(ObjectKind::Component, "agentsight")
                .expect("present")
                .binding
                .version(),
            Some("0.2.0")
        );
    }

    #[test]
    fn remove_installation_reports_removal() {
        let mut state = crate::state_store::StateStore::empty();
        state.upsert(migrate_fixture(&sample_object(
            ObjectKind::Component,
            "agentsight",
            "0.1.0",
        )));
        assert!(state.remove(ObjectKind::Component, "agentsight"));
        assert!(state.find(ObjectKind::Component, "agentsight").is_none());
        assert!(!state.remove(ObjectKind::Component, "agentsight"));
    }

    #[test]
    fn legacy_backup_and_operation_records_round_trip() {
        let mut state = InstalledState::default();
        assert_eq!(state.backups.len(), 0);
        assert_eq!(state.operations.len(), 0);

        state.backups.push(sample_backup("backup-op-1", "op-1"));
        state.operations.push(sample_operation("op-1"));
        state.operations.push(sample_operation("op-2"));

        let encoded = toml::to_string(&state).expect("serialize legacy state");
        let loaded: InstalledState = toml::from_str(&encoded).expect("read legacy state");
        assert_eq!(loaded.backups, state.backups);
        assert_eq!(loaded.operations, state.operations);
        assert_eq!(loaded.backups.len(), 1);
        assert_eq!(loaded.operations.len(), 2);
    }

    #[test]
    fn external_modified_files_links_backup_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("installed.toml");

        let mut state = InstalledState::default();
        let mut obj = sample_object(ObjectKind::Adapter, "openclaw", "0.1.0");
        obj.external_modified_files.push(ExternalModifiedFile {
            path: PathBuf::from("/etc/openclaw/config.toml"),
            owner: FileOwner::External,
            backup_id: "backup-op-1".to_string(),
            sha256_before: Some("before".to_string()),
            sha256_after: Some("after".to_string()),
        });
        state.objects.push(obj);
        state.backups.push(sample_backup("backup-op-1", "op-1"));
        state.operations.push(sample_operation("op-1"));

        write_legacy_fixture(&state, &path).expect("save");
        let loaded =
            toml::from_str::<InstalledState>(&fs::read_to_string(&path).expect("read fixture"))
                .expect("load");

        let adapter = loaded
            .objects
            .iter()
            .find(|object| object.kind == ObjectKind::Adapter && object.name == "openclaw")
            .expect("adapter present");
        assert_eq!(adapter.external_modified_files.len(), 1);
        assert_eq!(
            adapter.external_modified_files[0].backup_id,
            loaded.backups[0].id
        );
    }

    #[test]
    fn tmp_path_for_is_unique_across_calls() {
        let p = Path::new("/var/lib/anolisa/installed.toml");
        let a = tmp_path_for(p);
        let b = tmp_path_for(p);
        let an = a.file_name().expect("a name").to_string_lossy();
        let bn = b.file_name().expect("b name").to_string_lossy();
        assert!(an.starts_with(".installed.toml."));
        assert!(an.ends_with(".tmp"));
        assert_ne!(an, bn, "two tmp paths for the same target must differ");
    }

    #[test]
    fn open_excl_nofollow_refuses_existing_regular_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let plant = dir.path().join(".already-here.tmp");
        fs::write(&plant, b"stale").expect("seed stale");
        let err = open_excl_nofollow(&plant).expect_err("must refuse existing");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }

    #[cfg(unix)]
    #[test]
    fn open_excl_nofollow_refuses_existing_symlink() {
        // Direct test of the primitive: a symlink planted at the tmp path
        // must error out instead of letting save() write through to the
        // victim outside the state dir.
        let dir = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        let victim = outside.path().join("victim");
        fs::write(&victim, b"do not touch").expect("seed victim");

        let plant = dir.path().join(".target.tmp");
        std::os::unix::fs::symlink(&victim, &plant).expect("plant symlink");

        let err = open_excl_nofollow(&plant).expect_err("must refuse symlink");
        let kind = err.kind();
        assert!(
            kind == io::ErrorKind::AlreadyExists || err.raw_os_error() == Some(nix::libc::ELOOP),
            "expected EEXIST or ELOOP, got {err:?}"
        );
        let bytes = fs::read(&victim).expect("victim still readable");
        assert_eq!(
            bytes, b"do not touch",
            "symlinked tmp must never be written through"
        );
    }

    #[test]
    fn back_to_back_save_calls_both_succeed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("installed.toml");

        let mut state = crate::state_store::StateStore::empty();
        state.upsert(migrate_fixture(&sample_object(
            ObjectKind::Component,
            "agentsight",
            "0.1.0",
        )));
        state.save(&path).expect("first save");
        state.upsert(migrate_fixture(&sample_object(
            ObjectKind::Component,
            "tokenless",
            "0.1.0",
        )));
        state.save(&path).expect("second save");

        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "save must not leak tmp siblings: {leftovers:?}"
        );

        let loaded = crate::state_store::StateStore::load(&path, 1000).expect("load");
        assert_eq!(loaded.installations.len(), 2);
    }

    #[test]
    fn save_failure_preserves_prior_installed_toml() {
        // If save fails after the file already exists, the prior bytes
        // must remain intact (the rename never executed).
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("installed.toml");

        let mut state = crate::state_store::StateStore::empty();
        state.upsert(migrate_fixture(&sample_object(
            ObjectKind::Component,
            "agentsight",
            "0.1.0",
        )));
        state.save(&path).expect("seed save");
        let prior = fs::read(&path).expect("read prior");

        // Replace the parent directory with a regular file so create_dir_all
        // and write would both fail.
        let cleanly_isolated = dir.path().join("inner");
        fs::write(&cleanly_isolated, b"blocker").expect("seed blocker");
        let blocked_path = cleanly_isolated.join("installed.toml");

        let mut blocked_state = state.clone();
        blocked_state.upsert(migrate_fixture(&sample_object(
            ObjectKind::Component,
            "tokenless",
            "0.1.0",
        )));
        let err = blocked_state.save(&blocked_path).expect_err("must fail");
        match err {
            StateError::Io { .. } => {}
            other => panic!("expected Io, got {other:?}"),
        }

        // Independent valid path is unchanged byte-for-byte.
        let after = fs::read(&path).expect("read after");
        assert_eq!(after, prior, "prior installed.toml must be untouched");
    }

    #[cfg(unix)]
    #[test]
    fn save_replaces_symlinked_target_without_writing_through_to_victim() {
        // If the *final* installed.toml is a symlink to a victim outside the
        // state dir, rename(2) replaces the symlink itself rather than
        // writing through it.
        let dir = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        let victim = outside.path().join("victim");
        fs::write(&victim, b"do not touch").expect("seed victim");

        let path = dir.path().join("installed.toml");
        std::os::unix::fs::symlink(&victim, &path).expect("plant symlink at target");

        let state = crate::state_store::StateStore::empty();
        state.save(&path).expect("save over symlink");

        let meta = fs::symlink_metadata(&path).expect("stat target");
        assert!(meta.file_type().is_file(), "target must be regular file");
        let after = fs::read(&victim).expect("read victim");
        assert_eq!(
            after, b"do not touch",
            "rename must replace the symlink, not write through it"
        );
    }

    #[test]
    fn serialize_skips_optional_none() {
        let mut state = InstalledState::default();
        let mut obj = sample_object(ObjectKind::Component, "agentsight", "0.1.0");
        obj.manifest_digest = None;
        obj.distribution_source = None;
        obj.install_backend = None;
        obj.last_operation_id = None;
        obj.ownership = None;
        obj.rpm_metadata = None;
        state.objects.push(obj);

        let rendered = toml::to_string_pretty(&state).expect("serialize");
        assert!(
            !rendered.contains("manifest_digest"),
            "None manifest_digest must be skipped, got:\n{rendered}"
        );
        assert!(
            !rendered.contains("distribution_source"),
            "None distribution_source must be skipped"
        );
        assert!(
            !rendered.contains("install_backend"),
            "None install_backend must be skipped"
        );
        assert!(
            !rendered.contains("last_operation_id"),
            "None last_operation_id must be skipped"
        );
        assert!(
            !rendered.contains("ownership"),
            "None ownership must be skipped"
        );
        assert!(
            !rendered.contains("rpm_metadata"),
            "None rpm_metadata must be skipped"
        );
    }

    #[test]
    fn legacy_ownership_migrates_to_current_authority() {
        use crate::domain::{ManagementRelation, ProviderBinding};
        for (ownership, backend, managed, adopted, delegated, removable) in [
            (Some(Ownership::RawManaged), "raw", true, false, false, true),
            (Some(Ownership::RpmManaged), "rpm", true, false, true, true),
            (
                Some(Ownership::RpmObserved),
                "rpm",
                false,
                true,
                true,
                false,
            ),
            (None, "raw", true, false, false, true),
            (None, "rpm", true, false, true, true),
            (None, "rpm", false, true, true, false),
            (None, "rpm", false, false, true, false),
            (None, "yum", true, false, true, true),
            (None, "yum", false, true, true, false),
        ] {
            let mut obj = sample_object(ObjectKind::Component, "test", "1.0.0");
            obj.ownership = ownership;
            obj.install_backend = Some(backend.to_string());
            obj.managed = managed;
            obj.adopted = adopted;
            let binding = migrate_fixture(&obj).binding;
            assert_eq!(binding.is_delegated(), delegated, "{obj:?}");
            assert_eq!(binding.owns_removal(), removable, "{obj:?}");
            if let ProviderBinding::Delegated { relation, .. } = binding {
                assert!(matches!(
                    (adopted, removable, relation),
                    (true, false, ManagementRelation::Adopted { .. })
                        | (false, true, ManagementRelation::Managed { .. })
                        | (false, false, ManagementRelation::Observed)
                ));
            }
            assert_eq!(
                obj.ownership, ownership,
                "migration must not backfill legacy authority"
            );
        }
    }

    #[test]
    fn rpm_observed_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("installed.toml");

        let mut state = InstalledState::default();
        let mut obj = sample_object(ObjectKind::Component, "copilot-shell", "1.2.3");
        obj.ownership = Some(Ownership::RpmObserved);
        obj.status = ObjectStatus::Adopted;
        obj.managed = false;
        obj.adopted = true;
        obj.install_backend = Some("rpm".to_string());
        obj.rpm_metadata = Some(RpmMetadata {
            package_name: "copilot-shell".to_string(),
            evr: Some("0:1.2.3-1.al8".to_string()),
            arch: Some("x86_64".to_string()),
            source_repo: Some("@System".to_string()),
        });
        obj.files = Vec::new();
        state.objects.push(obj);

        write_legacy_fixture(&state, &path).expect("save");
        let loaded =
            toml::from_str::<InstalledState>(&fs::read_to_string(&path).expect("read fixture"))
                .expect("load");

        let comp = loaded
            .objects
            .iter()
            .find(|object| object.kind == ObjectKind::Component && object.name == "copilot-shell")
            .expect("present");
        assert_eq!(comp.ownership, Some(Ownership::RpmObserved));
        assert!(migrate_fixture(comp).binding.is_delegated());
        assert!(!migrate_fixture(comp).binding.owns_removal());

        let rpm = comp.rpm_metadata.as_ref().expect("rpm_metadata present");
        assert_eq!(rpm.package_name, "copilot-shell");
        assert_eq!(rpm.evr.as_deref(), Some("0:1.2.3-1.al8"));
        assert_eq!(rpm.arch.as_deref(), Some("x86_64"));
        assert_eq!(rpm.source_repo.as_deref(), Some("@System"));
    }

    #[test]
    fn rpm_managed_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("installed.toml");

        let mut state = InstalledState::default();
        let mut obj = sample_object(ObjectKind::Component, "copilot-shell", "1.2.3");
        obj.ownership = Some(Ownership::RpmManaged);
        obj.install_backend = Some("rpm".to_string());
        obj.rpm_metadata = Some(RpmMetadata {
            package_name: "copilot-shell".to_string(),
            evr: Some("0:1.2.3-1.al8".to_string()),
            arch: Some("x86_64".to_string()),
            source_repo: Some("anolisa-release".to_string()),
        });
        state.objects.push(obj);

        write_legacy_fixture(&state, &path).expect("save");
        let loaded =
            toml::from_str::<InstalledState>(&fs::read_to_string(&path).expect("read fixture"))
                .expect("load");

        let comp = loaded
            .objects
            .iter()
            .find(|object| object.kind == ObjectKind::Component && object.name == "copilot-shell")
            .expect("present");
        assert_eq!(comp.ownership, Some(Ownership::RpmManaged));
        assert!(migrate_fixture(comp).binding.is_delegated());
        assert!(migrate_fixture(comp).binding.owns_removal());
    }

    /// Pre-v3 state files omit `ownership` and `rpm_metadata`; loading
    /// must not reject them (backward compatibility).
    #[test]
    fn pre_v3_state_without_ownership_deserializes() {
        let toml_text = r#"
            schema_version = 2
            updated_at = "2026-06-01T10:00:00Z"
            install_mode = "system"
            prefix = "/"
            anolisa_version = "0.2.0"

            [[objects]]
            kind = "component"
            name = "copilot-shell"
            version = "1.0.0"
            status = "adopted"
            install_backend = "rpm"
            installed_at = "2026-06-01T10:00:00Z"
            managed = false
            adopted = true
        "#;
        let state: InstalledState = toml::from_str(toml_text).expect("pre-v3 state parses");
        let obj = state
            .objects
            .iter()
            .find(|object| object.kind == ObjectKind::Component && object.name == "copilot-shell")
            .expect("present");
        assert_eq!(obj.ownership, None);
        assert_eq!(obj.rpm_metadata, None);
        let migrated = migrate_fixture(obj);
        assert!(migrated.binding.is_delegated());
        assert!(!migrated.binding.owns_removal());
        assert_eq!(obj.ownership, None);
    }

    /// Loading an older state file and saving it must stamp the current
    /// `schema_version`, silently upgrading the on-disk version while
    /// preserving the object payload.
    #[test]
    fn save_upgrades_schema_version_from_older_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("installed.toml");

        let v2_text = r#"
            schema_version = 2
            updated_at = "2026-06-01T10:00:00Z"
            install_mode = "system"
            prefix = "/"
            anolisa_version = "0.2.0"

            [[objects]]
            kind = "component"
            name = "copilot-shell"
            version = "1.0.0"
            status = "installed"
            install_backend = "raw"
            installed_at = "2026-06-01T10:00:00Z"
            managed = true
            adopted = false
        "#;
        fs::write(&path, v2_text).expect("seed v2 file");

        let state = crate::state_store::StateStore::load(&path, 1000).expect("load v2");
        assert_eq!(state.installations.len(), 1);

        state.save(&path).expect("save");

        let upgraded = crate::state_store::StateStore::load(&path, 1000).expect("reload");
        let saved: toml::Value =
            toml::from_str(&fs::read_to_string(&path).expect("read")).expect("parse");
        assert_eq!(
            saved["schema_version"].as_integer(),
            Some(i64::from(crate::state_store::STORE_SCHEMA_VERSION))
        );
        assert_eq!(upgraded.installations, state.installations);
        assert!(
            upgraded
                .find(ObjectKind::Component, "copilot-shell")
                .is_some(),
            "object payload survives the upgrade"
        );
    }

    #[test]
    fn ownership_serde_snake_case() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("installed.toml");

        let mut state = InstalledState::default();
        let mut obj = sample_object(ObjectKind::Component, "test", "1.0.0");
        obj.ownership = Some(Ownership::RpmObserved);
        state.objects.push(obj);
        write_legacy_fixture(&state, &path).expect("save");

        let content = fs::read_to_string(&path).expect("read");
        assert!(
            content.contains("rpm_observed"),
            "ownership must serialize as snake_case, got:\n{content}"
        );
    }
}

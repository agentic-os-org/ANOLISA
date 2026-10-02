// SPDX-License-Identifier: Apache-2.0
//! Filesystem-backed checkpoint catalog owned by the daemon.
//!
//! The catalog, sandbox directories, staging directories, committed
//! checkpoints, and artifacts are opened relative to retained directory
//! descriptors. Configured pathnames are retained only for diagnostics.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use blaze_core::checkpoint::{
    CHECKPOINT_FORMAT_V1, CHECKPOINT_FORMAT_VERSION, CheckpointArtifact, CheckpointInfo,
    CheckpointMetadata, CheckpointValidationError, CommitCheckpoint, PAYLOAD_BACKEND_DIR,
    PAYLOAD_STORAGE_DIR, validate_artifact_path, validate_checkpoint_id,
    validate_checkpoint_manifest, validate_commit_checkpoint,
};
use chrono::Utc;
use rustix::fs::{
    AtFlags, Dir, FileType, Mode, OFlags, RenameFlags, fchmod, fstat, fsync, mkdirat, openat,
    renameat, renameat_with, statat, unlinkat,
};
use rustix::io::Errno;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::error::BlazeDaemonError;
use crate::state_store::{OwnedStateDirectory, StateStore};

const METADATA_FILE: &str = "metadata.json";
const HEAD_FILE: &str = "HEAD";
const STAGING_SUFFIX: &str = ".tmp";
const TOMBSTONE_SUFFIX: &str = ".tombstone";
const ABORT_TOMBSTONE_PREFIX: &str = ".abort.";
const PRUNE_TOMBSTONE_PREFIX: &str = ".prune.";
const CHECKPOINT_DIRECTORY_MODE: Mode = Mode::RWXU;
const CHECKPOINT_FILE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);
/// Deepest payload nesting the daemon will walk. Matches the pure
/// artifact-path bound, so every committable payload is enumerable.
const MAX_PAYLOAD_DEPTH: usize = 16;
/// Deepest nesting removal will traverse. Deliberately far above
/// [`MAX_PAYLOAD_DEPTH`]: a payload rejected for being too deep must still
/// be removable by the compensation path, so this bound only protects the
/// daemon from descriptor exhaustion on a pathological tree. Backend
/// adapters are in-process trait implementations, not untrusted input; a
/// tree deeper than this is a daemon-level defect, and the removal error
/// keeps the sandbox in recovery rather than walking an unbounded tree.
const MAX_REMOVAL_DEPTH: usize = 256;
/// Writable-root artifact the storage provider must capture. Restore
/// consumes it unconditionally, so publication refuses a checkpoint
/// without it.
const STORAGE_ROOTFS_FILE: &str = "rootfs.snap";

/// Failure while reading or mutating the daemon checkpoint catalog.
#[derive(Debug, Error)]
pub enum CheckpointStoreError {
    /// A checkpoint record failed pure model validation.
    #[error(transparent)]
    Validation(#[from] CheckpointValidationError),

    /// Opening the namespace through the retained state root failed.
    #[error("checkpoint catalog state-root operation failed: {0}")]
    State(#[source] BlazeDaemonError),

    /// A catalog filesystem operation failed.
    #[error("checkpoint catalog {operation} failed for {}: {source}", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A metadata file could not be encoded or decoded.
    #[error("checkpoint metadata at {} is invalid: {source}", path.display())]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    /// The catalog layout violates an invariant required for safe mutation.
    #[error("checkpoint catalog invariant failed: {0}")]
    Invariant(String),
}

/// Convenient result type for checkpoint catalog operations.
pub type Result<T> = std::result::Result<T, CheckpointStoreError>;

/// Result of removing unreachable checkpoint branches.
#[derive(Debug)]
pub enum PruneOutcome {
    /// Every selected checkpoint was removed and no prune tombstone remains.
    Complete { removed: Vec<String> },
    /// At least one checkpoint may already have left the committed catalog.
    /// The caller must keep the sandbox unavailable until normal recovery
    /// removes any retained tombstone.
    Incomplete {
        removed: Vec<String>,
        uncertain: Option<String>,
        source: Box<CheckpointStoreError>,
    },
}

/// Failure while creating a checkpoint stage.
#[derive(Debug, Error)]
#[error("{source}")]
pub struct CheckpointBeginError {
    recovery_checkpoint_id: Option<String>,
    #[source]
    source: Box<CheckpointStoreError>,
}

impl CheckpointBeginError {
    /// Return the checkpoint whose stage cleanup could not be confirmed.
    pub fn recovery_checkpoint_id(&self) -> Option<&str> {
        self.recovery_checkpoint_id.as_deref()
    }

    fn recovery_required(checkpoint_id: String, source: CheckpointStoreError) -> Self {
        Self {
            recovery_checkpoint_id: Some(checkpoint_id),
            source: Box::new(source),
        }
    }
}

impl From<CheckpointStoreError> for CheckpointBeginError {
    fn from(source: CheckpointStoreError) -> Self {
        Self {
            recovery_checkpoint_id: None,
            source: Box::new(source),
        }
    }
}

/// Result of creating a checkpoint stage.
pub type BeginResult<T> = std::result::Result<T, CheckpointBeginError>;

/// Namespace outcome reported when checkpoint publication fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointPublishOutcome {
    /// The staging directory is known not to have been renamed.
    KnownUnpublished,
    /// The staging-to-catalog rename may have completed.
    Unknown,
}

/// Checkpoint publication failure with its observable namespace outcome.
#[derive(Debug, Error)]
#[error("{source}")]
pub struct CheckpointPublishError {
    outcome: CheckpointPublishOutcome,
    #[source]
    source: CheckpointStoreError,
}

impl CheckpointPublishError {
    /// Return the strongest namespace outcome known at the failure boundary.
    pub fn outcome(&self) -> CheckpointPublishOutcome {
        self.outcome
    }

    /// Return the underlying catalog error.
    pub fn into_store_error(self) -> CheckpointStoreError {
        self.source
    }

    fn known_unpublished(source: CheckpointStoreError) -> Self {
        Self {
            outcome: CheckpointPublishOutcome::KnownUnpublished,
            source,
        }
    }

    fn unknown(source: CheckpointStoreError) -> Self {
        Self {
            outcome: CheckpointPublishOutcome::Unknown,
            source,
        }
    }
}

/// Result of publishing a checkpoint staging directory.
pub type PublishResult<T> = std::result::Result<T, CheckpointPublishError>;

/// Namespace outcome reported when moving checkpoint HEAD fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointHeadOutcome {
    /// HEAD retains its previous value and temporary cleanup is durable.
    KnownUnchanged,
    /// HEAD replacement may have completed, or temporary cleanup is uncertain.
    Unknown,
}

/// HEAD-update failure with its observable namespace outcome.
#[derive(Debug, Error)]
#[error("{source}")]
pub struct CheckpointHeadError {
    outcome: CheckpointHeadOutcome,
    #[source]
    source: CheckpointStoreError,
}

impl CheckpointHeadError {
    /// Return the strongest namespace outcome known at the failure boundary.
    pub fn outcome(&self) -> CheckpointHeadOutcome {
        self.outcome
    }

    /// Return the underlying catalog error.
    pub fn into_store_error(self) -> CheckpointStoreError {
        self.source
    }

    fn known_unchanged(source: CheckpointStoreError) -> Self {
        Self {
            outcome: CheckpointHeadOutcome::KnownUnchanged,
            source,
        }
    }

    fn unknown(source: CheckpointStoreError) -> Self {
        Self {
            outcome: CheckpointHeadOutcome::Unknown,
            source,
        }
    }
}

/// Result of atomically moving checkpoint HEAD.
pub type SetHeadResult<T> = std::result::Result<T, CheckpointHeadError>;

/// Temporary checkpoint directory populated before atomic publication.
#[derive(Debug)]
pub struct CheckpointStage {
    id: String,
    sandbox_id: Uuid,
    catalog: OwnedStateDirectory,
    sandbox: OwnedStateDirectory,
    directory: OwnedStateDirectory,
    backend_dir: OwnedStateDirectory,
    storage_dir: OwnedStateDirectory,
    staging_name: String,
}

impl CheckpointStage {
    /// Generated checkpoint identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Payload subtree owned by the backend adapter for this capture.
    ///
    /// The layout below this directory is private to the backend; publication
    /// inventories whatever regular files it wrote. The configured pathname
    /// is returned because capture may exec an external backend process that
    /// cannot resolve this daemon's `/proc/self/fd` names; integrity never
    /// depends on this path, since publication reopens, hashes, and
    /// revalidates every entry through the retained stage descriptors.
    pub fn backend_payload_dir(&self) -> PathBuf {
        self.backend_dir.configured_path().to_path_buf()
    }

    /// Payload subtree owned by the storage provider for this capture.
    pub fn storage_payload_dir(&self) -> PathBuf {
        self.storage_dir.configured_path().to_path_buf()
    }
}

struct OwnedArtifact {
    path: PathBuf,
    file: File,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct PruneScratchKey {
    checkpoint_id: String,
    nonce: Uuid,
}

enum CandidateSweep {
    Removed,
    Retained(CheckpointStoreError),
    RemovedWithUnfinishedCleanup(CheckpointStoreError),
    Uncertain(CheckpointStoreError),
}

/// Retained payload subtree descriptors of one committed checkpoint.
///
/// Version-1 checkpoints keep every artifact in the checkpoint root, so both
/// producer subtrees map onto the root directory itself. This is what lets a
/// pre-split checkpoint stay restorable without rewriting it on disk.
#[derive(Debug)]
struct PayloadDirs {
    backend: Option<OwnedStateDirectory>,
    storage: Option<OwnedStateDirectory>,
}

struct VerifiedCheckpoint {
    metadata: CheckpointMetadata,
    directory: OwnedStateDirectory,
    metadata_file: OwnedArtifact,
    artifacts: Vec<(String, OwnedArtifact)>,
    payload: PayloadDirs,
}

/// Restore target retained through the complete replacement operation.
///
/// The catalog, sandbox, checkpoint directory, payload subtrees, and artifact
/// descriptors stay open so path replacement cannot redirect either restore
/// input or HEAD.
pub(crate) struct RestoreCheckpoint {
    catalog: OwnedStateDirectory,
    sandbox: OwnedStateDirectory,
    verified: VerifiedCheckpoint,
}

impl RestoreCheckpoint {
    pub(crate) fn metadata(&self) -> &CheckpointMetadata {
        &self.verified.metadata
    }

    /// Backend-owned payload subtree for the restore adapter to consume.
    ///
    /// The configured pathname is returned because restore may exec an
    /// external backend process that cannot resolve this daemon's
    /// `/proc/self/fd` names. Trust does not derive from the path: the
    /// artifacts were hashed against the retained descriptors during
    /// verification, the catalog tree is daemon-owned with mode 0700, and
    /// [`CheckpointStore::set_head_verified`] revalidates every retained
    /// artifact identity through the held descriptors before the restore
    /// can commit — a descendant swapped in after verification fails that
    /// revalidation and aborts the transaction instead of becoming HEAD.
    pub(crate) fn backend_payload_dir(&self) -> PathBuf {
        match &self.verified.payload.backend {
            Some(directory) => directory.configured_path().to_path_buf(),
            None => self.verified.directory.configured_path().to_path_buf(),
        }
    }

    /// Storage-owned payload subtree for the storage provider to consume.
    pub(crate) fn storage_payload_dir(&self) -> PathBuf {
        match &self.verified.payload.storage {
            Some(directory) => directory.configured_path().to_path_buf(),
            None => self.verified.directory.configured_path().to_path_buf(),
        }
    }
}

struct LoadedCheckpointMetadata {
    metadata: CheckpointMetadata,
    directory: OwnedStateDirectory,
    metadata_file: OwnedArtifact,
    artifacts: Vec<(String, OwnedArtifact)>,
    payload: PayloadDirs,
}

pub(crate) struct PublishedCheckpoint {
    catalog: OwnedStateDirectory,
    sandbox: OwnedStateDirectory,
    loaded: LoadedCheckpointMetadata,
}

impl PublishedCheckpoint {
    pub(crate) fn metadata(&self) -> &CheckpointMetadata {
        &self.loaded.metadata
    }

    fn require_linked(&self) -> Result<()> {
        let sandbox_name = self.loaded.metadata.sandbox_id.to_string();
        require_linked_directory(&self.catalog, &sandbox_name, &self.sandbox)?;
        self.loaded
            .require_linked(&self.sandbox, &self.loaded.metadata.id)
    }

    fn into_metadata(self) -> CheckpointMetadata {
        self.loaded.metadata
    }
}

impl LoadedCheckpointMetadata {
    fn require_linked(&self, sandbox: &OwnedStateDirectory, directory_name: &str) -> Result<()> {
        require_linked_checkpoint(
            &self.metadata,
            &self.directory,
            &self.metadata_file,
            &self.artifacts,
            sandbox,
            directory_name,
        )
    }
}

impl VerifiedCheckpoint {
    fn require_linked(&self, sandbox: &OwnedStateDirectory, checkpoint_id: &str) -> Result<()> {
        require_linked_checkpoint(
            &self.metadata,
            &self.directory,
            &self.metadata_file,
            &self.artifacts,
            sandbox,
            checkpoint_id,
        )
    }
}

/// Revalidate that a committed checkpoint still has exactly the layout its
/// manifest records, and that every retained descriptor is still the object
/// linked at its catalog name.
///
/// The manifest is the single source of truth here: version-2 checkpoints
/// carry backend-private layouts, so the daemon cannot whitelist names. What
/// it can still reject is any file the manifest does not account for.
fn require_linked_checkpoint(
    metadata: &CheckpointMetadata,
    directory: &OwnedStateDirectory,
    metadata_file: &OwnedArtifact,
    artifacts: &[(String, OwnedArtifact)],
    sandbox: &OwnedStateDirectory,
    directory_name: &str,
) -> Result<()> {
    if metadata.format_version == CHECKPOINT_FORMAT_V1 {
        let mut expected: Vec<&str> = metadata
            .artifacts
            .iter()
            .map(|artifact| artifact.name.as_str())
            .collect();
        expected.push(METADATA_FILE);
        validate_exact_entries(directory, &expected)?;
    } else {
        validate_exact_entries(
            directory,
            &[METADATA_FILE, PAYLOAD_BACKEND_DIR, PAYLOAD_STORAGE_DIR],
        )?;
        require_manifest_matches_payload(metadata, directory)?;
    }
    require_linked_file(directory, METADATA_FILE, metadata_file)?;
    for (rel_path, artifact) in artifacts {
        require_linked_path(directory, rel_path, artifact)?;
    }
    require_linked_directory(sandbox, directory_name, directory)
}

/// Reject any payload file the manifest does not record, and any recorded
/// file that is no longer present.
fn require_manifest_matches_payload(
    metadata: &CheckpointMetadata,
    directory: &OwnedStateDirectory,
) -> Result<()> {
    let mut found = Vec::new();
    let mut dirs = Vec::new();
    for subtree in [PAYLOAD_BACKEND_DIR, PAYLOAD_STORAGE_DIR] {
        let payload =
            required_child_directory(directory, subtree, "open checkpoint payload subtree")?;
        collect_payload_files(&payload, subtree, 1, &mut found, &mut dirs)?;
    }
    let mut found_names: Vec<&str> = found.iter().map(|(name, _)| name.as_str()).collect();
    found_names.sort_unstable();
    let manifest_names: Vec<&str> = metadata
        .artifacts
        .iter()
        .map(|artifact| artifact.name.as_str())
        .collect();
    if found_names != manifest_names {
        return Err(invariant(format!(
            "checkpoint {} payload does not match its manifest: found {:?}, recorded {:?}",
            metadata.id, found_names, manifest_names
        )));
    }
    Ok(())
}

#[cfg(test)]
type BeforePublishRevalidation = Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>;

/// Filesystem-backed checkpoint catalog.
#[derive(Clone)]
pub struct CheckpointStore {
    state_store: StateStore,
    root: Arc<Mutex<Option<OwnedStateDirectory>>>,
    #[cfg(test)]
    before_publish_revalidation: BeforePublishRevalidation,
    #[cfg(test)]
    verified_checkpoint_calls: Arc<AtomicUsize>,
}

impl std::fmt::Debug for CheckpointStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CheckpointStore")
            .field("state_store", &self.state_store)
            .finish_non_exhaustive()
    }
}

impl CheckpointStore {
    /// Bind the catalog to the daemon's retained state-root owner.
    pub fn new(state_store: StateStore) -> Self {
        Self {
            state_store,
            root: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            before_publish_revalidation: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            verified_checkpoint_calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Create and durably expose a unique staging directory.
    pub fn begin(&self, sandbox_id: Uuid) -> BeginResult<CheckpointStage> {
        let catalog = self.root()?;
        let sandbox = self.ensure_sandbox_dir(&catalog, sandbox_id)?;
        loop {
            let id = format!("ckpt-{}", Uuid::new_v4());
            let staging_name = format!(".{id}{STAGING_SUFFIX}");
            let stage_path = sandbox.configured_path().join(&staging_name);
            match mkdirat(
                sandbox.descriptor(),
                staging_name.as_str(),
                CHECKPOINT_DIRECTORY_MODE,
            ) {
                Ok(()) => {}
                Err(Errno::EXIST) => continue,
                Err(source) => {
                    return Err(io_error(
                        "create checkpoint directory",
                        &stage_path,
                        std::io::Error::from(source),
                    )
                    .into());
                }
            }
            let directory =
                match checkpoint_store_failpoint("checkpoint-store-stage-open", &stage_path)
                    .and_then(|()| open_child_directory(&sandbox, &staging_name))
                {
                    Ok(directory) => directory,
                    Err(open_error) => {
                        let cleanup_result = checkpoint_store_failpoint(
                            "checkpoint-store-stage-open-cleanup-before-unlink",
                            &stage_path,
                        )
                        .and_then(|()| {
                            unlinkat(
                                sandbox.descriptor(),
                                staging_name.as_str(),
                                AtFlags::REMOVEDIR,
                            )
                            .map_err(|source| {
                                io_error(
                                    "remove unopened checkpoint stage",
                                    &stage_path,
                                    std::io::Error::from(source),
                                )
                            })
                        })
                        .and_then(|()| {
                            checkpoint_store_failpoint(
                                "checkpoint-store-stage-open-cleanup-parent-sync",
                                sandbox.configured_path(),
                            )
                            .and_then(|()| sync_directory(&sandbox))
                        });
                        return match cleanup_result {
                            Ok(()) => Err(open_error.into()),
                            Err(cleanup_error) => Err(CheckpointBeginError::recovery_required(
                                id,
                                invariant(format!(
                                    "checkpoint stage opening failed: {open_error}; \
                                 created-stage cleanup also failed: {cleanup_error}"
                                )),
                            )),
                        };
                    }
                };
            // Both producer subtrees exist from the start, so a producer can
            // never be confused about where its payload belongs and an empty
            // subtree stays representable.
            let subtrees = (|| -> Result<(OwnedStateDirectory, OwnedStateDirectory)> {
                let backend_dir = create_child_directory(&directory, PAYLOAD_BACKEND_DIR)?;
                let storage_dir = create_child_directory(&directory, PAYLOAD_STORAGE_DIR)?;
                checkpoint_store_failpoint(
                    "checkpoint-store-stage-payload-sync",
                    directory.configured_path(),
                )?;
                sync_directory(&directory)?;
                Ok((backend_dir, storage_dir))
            })();
            let (backend_dir, storage_dir) = match subtrees {
                Ok(subtrees) => subtrees,
                Err(subtree_error) => {
                    return match self.abort_owned_stage(&sandbox, &staging_name, directory) {
                        Ok(()) => Err(subtree_error.into()),
                        Err(cleanup_error) => Err(CheckpointBeginError::recovery_required(
                            id,
                            invariant(format!(
                                "checkpoint stage payload preparation failed: \
                                 {subtree_error}; owned-stage cleanup also failed: \
                                 {cleanup_error}"
                            )),
                        )),
                    };
                }
            };
            let sync_result = checkpoint_store_failpoint(
                "checkpoint-store-stage-parent-sync",
                sandbox.configured_path(),
            )
            .and_then(|()| sync_directory(&sandbox));
            if let Err(sync_error) = sync_result {
                return match self.abort_owned_stage(&sandbox, &staging_name, directory) {
                    Ok(()) => Err(sync_error.into()),
                    Err(cleanup_error) => Err(CheckpointBeginError::recovery_required(
                        id,
                        invariant(format!(
                            "checkpoint stage parent synchronization failed: \
                             {sync_error}; owned-stage cleanup also failed: \
                             {cleanup_error}"
                        )),
                    )),
                };
            }
            return Ok(CheckpointStage {
                id,
                sandbox_id,
                catalog,
                sandbox,
                directory,
                backend_dir,
                storage_dir,
                staging_name,
            });
        }
    }

    /// Hash, sync, and atomically publish a populated stage without moving HEAD.
    #[cfg(test)]
    pub fn publish(
        &self,
        stage: &CheckpointStage,
        input: CommitCheckpoint,
    ) -> PublishResult<CheckpointMetadata> {
        self.publish_retained(stage, input)
            .map(PublishedCheckpoint::into_metadata)
    }

    pub(crate) fn publish_retained(
        &self,
        stage: &CheckpointStage,
        input: CommitCheckpoint,
    ) -> PublishResult<PublishedCheckpoint> {
        let loaded = (|| -> Result<LoadedCheckpointMetadata> {
            self.validate_stage(stage)?;
            validate_commit_checkpoint(&stage.id, &input)?;
            if let Some(parent) = &input.parent {
                self.validated_chain_from(&stage.sandbox, stage.sandbox_id, parent)?;
            }
            if optional_child_directory(&stage.sandbox, &stage.id)?.is_some() {
                return Err(invariant(format!(
                    "checkpoint publication target {} already exists",
                    stage.sandbox.configured_path().join(&stage.id).display()
                )));
            }
            validate_exact_entries(
                &stage.directory,
                &[PAYLOAD_BACKEND_DIR, PAYLOAD_STORAGE_DIR],
            )?;
            // `validate_exact_entries` only inspects the staging directory. A
            // capture that escaped its configured payload path (for example
            // through `payload_dir.join("../../capture.log")`) can instead have
            // dropped an entry directly in the sandbox checkpoint namespace,
            // which publication would leave in place and a later destroy would
            // reject through `validate_checkpoint_id`, stranding the sandbox in
            // `RecoveryRequired` with storage retained. Reject it now, before
            // the capture is committed.
            require_publishable_sandbox_namespace(&stage.sandbox, &stage.staging_name)?;

            // Inventory whatever the producers wrote. The backend owns its
            // subtree layout, so publication walks the payload instead of
            // expecting fixed names; the walk also pins every file and
            // directory it will hash or sync.
            let mut opened_artifacts = Vec::new();
            let mut payload_dirs = Vec::new();
            let backend_file_count = collect_payload_files(
                &stage.backend_dir,
                PAYLOAD_BACKEND_DIR,
                1,
                &mut opened_artifacts,
                &mut payload_dirs,
            )?;
            collect_payload_files(
                &stage.storage_dir,
                PAYLOAD_STORAGE_DIR,
                1,
                &mut opened_artifacts,
                &mut payload_dirs,
            )?;
            opened_artifacts.sort_by(|left, right| left.0.cmp(&right.0));
            if opened_artifacts.is_empty() {
                return Err(invariant(format!(
                    "checkpoint stage {} has an empty payload",
                    stage.directory.configured_path().display()
                )));
            }
            if backend_file_count == 0 {
                return Err(invariant(format!(
                    "checkpoint stage {} has an empty backend payload subtree; \
                     the snapshot adapter must produce at least one artifact",
                    stage.directory.configured_path().display()
                )));
            }
            // The restore path consumes the writable-root capture
            // unconditionally, so a checkpoint without it would publish as
            // valid and only fail when someone tries to restore it.
            let rootfs_rel = format!("{PAYLOAD_STORAGE_DIR}/{STORAGE_ROOTFS_FILE}");
            if !opened_artifacts
                .iter()
                .any(|(rel_path, _)| rel_path == &rootfs_rel)
            {
                return Err(invariant(format!(
                    "checkpoint stage {} has no {rootfs_rel} capture",
                    stage.directory.configured_path().display()
                )));
            }

            // Ownership validation finishes across the whole inventory before
            // any permission changes, so a rejected payload is left exactly
            // as the producers wrote it.
            for (_, artifact) in &opened_artifacts {
                validate_checkpoint_artifact_owner(artifact)?;
            }
            let mut artifacts = Vec::with_capacity(opened_artifacts.len());
            for (rel_path, artifact) in &mut opened_artifacts {
                fchmod(&artifact.file, CHECKPOINT_FILE_MODE).map_err(|source| {
                    io_error(
                        "restrict checkpoint artifact permissions",
                        &artifact.path,
                        std::io::Error::from(source),
                    )
                })?;
                artifact.file.sync_all().map_err(|source| {
                    io_error("sync checkpoint artifact", &artifact.path, source)
                })?;
                artifacts.push(hash_artifact(artifact, rel_path)?);
            }
            // Nested directory entries become durable with their parents;
            // the stage root is synced after the manifest lands below.
            for payload_dir in &payload_dirs {
                sync_directory(payload_dir)?;
            }
            sync_directory(&stage.backend_dir)?;
            sync_directory(&stage.storage_dir)?;

            let metadata = CheckpointMetadata {
                format_version: CHECKPOINT_FORMAT_VERSION,
                id: stage.id.clone(),
                parent: input.parent,
                sandbox_id: stage.sandbox_id,
                policy_name: input.policy_name,
                image_digest: input.image_digest,
                backend: input.backend,
                backend_version: input.backend_version,
                created_at: Utc::now(),
                snapshot_kind: input.snapshot_kind,
                artifacts,
            };
            validate_checkpoint_manifest(&metadata, stage.sandbox_id, &stage.id)?;
            let metadata_file = write_json_new(&stage.directory, METADATA_FILE, &metadata)?;
            sync_directory(&stage.directory)?;

            #[cfg(test)]
            self.run_before_publish_revalidation();

            let loaded = LoadedCheckpointMetadata {
                metadata,
                directory: stage.directory.clone(),
                metadata_file,
                artifacts: opened_artifacts,
                payload: PayloadDirs {
                    backend: Some(stage.backend_dir.clone()),
                    storage: Some(stage.storage_dir.clone()),
                },
            };
            loaded.require_linked(&stage.sandbox, &stage.staging_name)?;
            checkpoint_store_failpoint(
                "checkpoint-store-publish-before-rename",
                &stage.sandbox.configured_path().join(&stage.staging_name),
            )?;
            Ok(loaded)
        })()
        .map_err(CheckpointPublishError::known_unpublished)?;

        renameat_with(
            stage.sandbox.descriptor(),
            stage.staging_name.as_str(),
            stage.sandbox.descriptor(),
            stage.id.as_str(),
            RenameFlags::NOREPLACE,
        )
        .map_err(|source| {
            let source = std::io::Error::from(source);
            io_error(
                "publish checkpoint directory",
                stage.sandbox.configured_path().join(&stage.id),
                source,
            )
        })
        .map_err(CheckpointPublishError::unknown)?;
        checkpoint_store_failpoint(
            "checkpoint-store-publish-after-rename",
            &stage.sandbox.configured_path().join(&stage.id),
        )
        .map_err(CheckpointPublishError::unknown)?;
        loaded
            .require_linked(&stage.sandbox, &stage.id)
            .map_err(CheckpointPublishError::unknown)?;
        sync_directory(&stage.sandbox).map_err(CheckpointPublishError::unknown)?;
        Ok(PublishedCheckpoint {
            catalog: stage.catalog.clone(),
            sandbox: stage.sandbox.clone(),
            loaded,
        })
    }

    /// Remove an unpublished stage owned by this process.
    pub fn abort(&self, stage: CheckpointStage) -> Result<()> {
        self.validate_stage(&stage)?;
        self.abort_owned_stage(&stage.sandbox, &stage.staging_name, stage.directory)
    }

    /// Read and validate one committed checkpoint and all artifact hashes.
    #[cfg(test)]
    pub fn verify(&self, sandbox_id: Uuid, checkpoint_id: &str) -> Result<CheckpointMetadata> {
        let catalog = self.root()?;
        let sandbox = required_child_directory(
            &catalog,
            &sandbox_id.to_string(),
            "open checkpoint sandbox directory",
        )?;
        Ok(self
            .verified_checkpoint(&sandbox, sandbox_id, checkpoint_id)?
            .metadata)
    }

    /// Verify and retain a restore target and its complete ancestry.
    pub(crate) fn verify_restore_target(
        &self,
        sandbox_id: Uuid,
        checkpoint_id: &str,
    ) -> Result<RestoreCheckpoint> {
        let catalog = self.root()?;
        let sandbox_name = sandbox_id.to_string();
        let sandbox =
            required_child_directory(&catalog, &sandbox_name, "open checkpoint sandbox directory")?;
        self.validated_chain_from(&sandbox, sandbox_id, checkpoint_id)?;
        let verified = self.verified_checkpoint(&sandbox, sandbox_id, checkpoint_id)?;
        require_linked_directory(&catalog, &sandbox_name, &sandbox)?;
        Ok(RestoreCheckpoint {
            catalog,
            sandbox,
            verified,
        })
    }

    /// Atomically move HEAD to a restore target retained by this process.
    pub(crate) fn set_head_verified(&self, target: &RestoreCheckpoint) -> SetHeadResult<()> {
        let checkpoint_id = target.verified.metadata.id.clone();
        let sandbox_name = target.verified.metadata.sandbox_id.to_string();
        self.set_head_with_revalidation(&target.sandbox, &checkpoint_id, || {
            let root = self.root()?;
            if !same_directory(&root, &target.catalog)? {
                return Err(invariant(
                    "restore target belongs to a different checkpoint catalog root",
                ));
            }
            require_linked_directory(&target.catalog, &sandbox_name, &target.sandbox)?;
            target
                .verified
                .require_linked(&target.sandbox, &checkpoint_id)
        })
    }

    /// List committed checkpoints and mark the lineage reachable from HEAD.
    pub fn list(&self, sandbox_id: Uuid) -> Result<Vec<CheckpointInfo>> {
        let catalog_root = self.root()?;
        let Some(sandbox) = optional_child_directory(&catalog_root, &sandbox_id.to_string())?
        else {
            return Ok(Vec::new());
        };
        let catalog = self.load_catalog(&sandbox, sandbox_id)?;
        let head = self.read_head_id_from(&sandbox)?;
        let on_head_chain = match head.as_deref() {
            Some(head) => lineage_from(&catalog, head)?,
            None => HashSet::new(),
        };

        let mut checkpoints = Vec::with_capacity(catalog.len());
        for metadata in catalog.into_values() {
            let size_bytes = metadata
                .artifacts
                .iter()
                .try_fold(0_u64, |total, artifact| {
                    total.checked_add(artifact.size_bytes)
                })
                .ok_or_else(|| {
                    invariant(format!(
                        "checkpoint {} artifact sizes overflow u64",
                        metadata.id
                    ))
                })?;
            checkpoints.push(CheckpointInfo {
                id: metadata.id.clone(),
                parent: metadata.parent,
                created_at: metadata.created_at,
                size_bytes,
                is_head: head.as_deref() == Some(metadata.id.as_str()),
                on_head_chain: on_head_chain.contains(&metadata.id),
            });
        }
        checkpoints.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(checkpoints)
    }

    /// Atomically move HEAD to an already committed, verified checkpoint.
    #[cfg(test)]
    pub fn set_head(&self, sandbox_id: Uuid, checkpoint_id: &str) -> SetHeadResult<()> {
        let catalog = self.root().map_err(CheckpointHeadError::known_unchanged)?;
        let sandbox = required_child_directory(
            &catalog,
            &sandbox_id.to_string(),
            "open checkpoint sandbox directory",
        )
        .map_err(CheckpointHeadError::known_unchanged)?;
        let verified = self
            .verified_checkpoint(&sandbox, sandbox_id, checkpoint_id)
            .map_err(CheckpointHeadError::known_unchanged)?;
        let sandbox_name = sandbox_id.to_string();
        self.set_head_with_revalidation(&sandbox, checkpoint_id, || {
            require_linked_directory(&catalog, &sandbox_name, &sandbox)?;
            verified.require_linked(&sandbox, checkpoint_id)
        })
    }

    pub(crate) fn set_head_published(
        &self,
        published: PublishedCheckpoint,
    ) -> SetHeadResult<CheckpointMetadata> {
        let root = self.root().map_err(CheckpointHeadError::known_unchanged)?;
        let checkpoint_id = published.metadata().id.clone();
        self.set_head_with_revalidation(&published.sandbox, &checkpoint_id, || {
            if !same_directory(&root, &published.catalog)? {
                return Err(invariant(
                    "published checkpoint belongs to a different catalog root",
                ));
            }
            published.require_linked()
        })?;
        Ok(published.into_metadata())
    }

    fn set_head_with_revalidation<F>(
        &self,
        sandbox: &OwnedStateDirectory,
        checkpoint_id: &str,
        mut revalidate: F,
    ) -> SetHeadResult<()>
    where
        F: FnMut() -> Result<()>,
    {
        revalidate().map_err(CheckpointHeadError::known_unchanged)?;
        // Opening with O_NOFOLLOW performs the complete type validation needed
        // before atomic replacement. A missing HEAD is also valid.
        let _existing_head = optional_file(sandbox, HEAD_FILE, "inspect existing checkpoint HEAD")
            .map_err(CheckpointHeadError::known_unchanged)?;

        let temporary_name = format!(".HEAD.{}{STAGING_SUFFIX}", Uuid::new_v4());
        let mut temporary = create_new_file(sandbox, &temporary_name, "create temporary HEAD")
            .map_err(CheckpointHeadError::known_unchanged)?;
        let before_rename = (|| {
            write_all(
                &mut temporary.file,
                &temporary.path,
                checkpoint_id.as_bytes(),
            )?;
            write_all(&mut temporary.file, &temporary.path, b"\n")?;
            temporary
                .file
                .sync_all()
                .map_err(|source| io_error("sync temporary HEAD", &temporary.path, source))?;
            revalidate()?;
            checkpoint_store_failpoint("checkpoint-store-head-before-rename", &temporary.path)
        })();
        if let Err(source) = before_rename {
            let cleanup =
                checkpoint_store_failpoint("checkpoint-store-head-cleanup", &temporary.path)
                    .and_then(|()| remove_file_if_exists(sandbox, &temporary_name))
                    .and_then(|()| sync_directory(sandbox));
            return match cleanup {
                Ok(()) => Err(CheckpointHeadError::known_unchanged(source)),
                Err(cleanup) => Err(CheckpointHeadError::unknown(invariant(format!(
                    "{source}; temporary HEAD cleanup failed: {cleanup}"
                )))),
            };
        }

        renameat(
            sandbox.descriptor(),
            temporary_name.as_str(),
            sandbox.descriptor(),
            HEAD_FILE,
        )
        .map_err(|source| {
            io_error(
                "publish checkpoint HEAD",
                sandbox.configured_path().join(HEAD_FILE),
                std::io::Error::from(source),
            )
        })
        .map_err(CheckpointHeadError::unknown)?;
        checkpoint_store_failpoint(
            "checkpoint-store-head-after-rename",
            &sandbox.configured_path().join(HEAD_FILE),
        )
        .map_err(CheckpointHeadError::unknown)?;
        require_linked_file(sandbox, HEAD_FILE, &temporary)
            .map_err(CheckpointHeadError::unknown)?;
        sync_directory(sandbox).map_err(CheckpointHeadError::unknown)
    }

    /// Remove committed checkpoints that are unreachable from the current
    /// HEAD.
    ///
    /// A candidate first moves to a uniquely named tombstone with a
    /// no-replace rename. The committed catalog therefore changes at one
    /// atomic boundary, while normal sandbox recovery can still recognise and
    /// remove an interrupted cleanup. Every candidate is revalidated against
    /// the original catalog and HEAD immediately before that boundary.
    /// `checkpoint_history_expected` is derived from the durable sandbox
    /// lifecycle record and prevents a vanished namespace from being mistaken
    /// for a sandbox that has never captured a checkpoint.
    pub fn prune_unreachable(
        &self,
        sandbox_id: Uuid,
        checkpoint_history_expected: bool,
    ) -> Result<PruneOutcome> {
        let catalog_root = self.root()?;
        let Some(sandbox) = optional_child_directory(&catalog_root, &sandbox_id.to_string())?
        else {
            if checkpoint_history_expected {
                return Err(invariant(format!(
                    "sandbox {sandbox_id} records completed checkpoints, but its checkpoint namespace is missing"
                )));
            }
            return Ok(PruneOutcome::Complete {
                removed: Vec::new(),
            });
        };
        require_prunable_sandbox_namespace(&sandbox)?;
        let catalog = self.load_catalog(&sandbox, sandbox_id)?;
        validate_catalog_lineages(&catalog)?;
        let head = self.read_head_id_from(&sandbox)?;
        if head.is_none() && !catalog.is_empty() {
            return Err(invariant(format!(
                "checkpoint catalog for sandbox {sandbox_id} has committed checkpoints but no HEAD"
            )));
        }
        if head.is_none() && checkpoint_history_expected {
            return Err(invariant(format!(
                "sandbox {sandbox_id} records completed checkpoints, but its checkpoint namespace has no HEAD"
            )));
        }
        self.verify_catalog_artifacts(&sandbox, sandbox_id, &catalog)?;
        let mut keep = HashSet::new();
        if let Some(head_id) = head.as_deref() {
            keep.extend(lineage_from(&catalog, head_id)?);
        }
        let mut candidates: Vec<String> = catalog
            .keys()
            .filter(|checkpoint_id| !keep.contains(*checkpoint_id))
            .cloned()
            .collect();
        candidates.sort();

        let mut removed = Vec::with_capacity(candidates.len());
        for checkpoint_id in candidates {
            match self.sweep_prune_candidate(
                &sandbox,
                sandbox_id,
                &catalog,
                head.as_deref(),
                &checkpoint_id,
            ) {
                CandidateSweep::Removed => removed.push(checkpoint_id),
                CandidateSweep::Retained(source) if removed.is_empty() => return Err(source),
                CandidateSweep::Retained(source) => {
                    return Ok(PruneOutcome::Incomplete {
                        removed,
                        uncertain: None,
                        source: Box::new(source),
                    });
                }
                CandidateSweep::RemovedWithUnfinishedCleanup(source) => {
                    removed.push(checkpoint_id);
                    return Ok(PruneOutcome::Incomplete {
                        removed,
                        uncertain: None,
                        source: Box::new(source),
                    });
                }
                CandidateSweep::Uncertain(source) => {
                    return Ok(PruneOutcome::Incomplete {
                        removed,
                        uncertain: Some(checkpoint_id),
                        source: Box::new(source),
                    });
                }
            }
        }
        Ok(PruneOutcome::Complete { removed })
    }

    fn sweep_prune_candidate(
        &self,
        sandbox: &OwnedStateDirectory,
        sandbox_id: Uuid,
        catalog: &HashMap<String, CheckpointMetadata>,
        planned_head: Option<&str>,
        checkpoint_id: &str,
    ) -> CandidateSweep {
        macro_rules! retain {
            ($expression:expr) => {
                match $expression {
                    Ok(value) => value,
                    Err(source) => return CandidateSweep::Retained(source),
                }
            };
        }

        let expected = match catalog.get(checkpoint_id).cloned() {
            Some(expected) => expected,
            None => {
                return CandidateSweep::Retained(invariant(
                    "checkpoint prune candidate disappeared from its validated plan",
                ));
            }
        };
        let candidate = retain!(self.load_checkpoint_metadata(sandbox, sandbox_id, checkpoint_id));
        if candidate.metadata != expected {
            return CandidateSweep::Retained(invariant(format!(
                "checkpoint prune candidate {checkpoint_id} changed after planning"
            )));
        }
        let current_head = retain!(self.read_head_id_from(sandbox));
        if current_head.as_deref() != planned_head {
            return CandidateSweep::Retained(invariant(
                "checkpoint HEAD changed after the prune plan was validated",
            ));
        }
        if current_head.as_deref() == Some(checkpoint_id) {
            return CandidateSweep::Retained(invariant(
                "checkpoint prune candidate became HEAD after planning",
            ));
        }

        let key = PruneScratchKey {
            checkpoint_id: checkpoint_id.to_string(),
            nonce: Uuid::new_v4(),
        };
        let tombstone_name = prune_tombstone_name(&key);
        if let Err(source) = checkpoint_store_failpoint(
            "checkpoint-prune-before-rename",
            candidate.directory.configured_path(),
        ) {
            return CandidateSweep::Retained(source);
        }
        let rename_error = renameat_with(
            sandbox.descriptor(),
            checkpoint_id,
            sandbox.descriptor(),
            tombstone_name.as_str(),
            RenameFlags::NOREPLACE,
        )
        .map_err(|source| {
            io_error(
                "publish checkpoint prune tombstone",
                sandbox.configured_path().join(&tombstone_name),
                std::io::Error::from(source),
            )
        })
        .err();

        let probe = (|| -> Result<(bool, bool, bool)> {
            let source = optional_child_directory(sandbox, checkpoint_id)?;
            let tombstone = optional_child_directory(sandbox, &tombstone_name)?;
            let source_matches = source
                .as_ref()
                .map(|directory| same_directory(directory, &candidate.directory))
                .transpose()?
                .unwrap_or(false);
            let tombstone_matches = tombstone
                .as_ref()
                .map(|directory| same_directory(directory, &candidate.directory))
                .transpose()?
                .unwrap_or(false);
            Ok((source.is_some(), source_matches, tombstone_matches))
        })();
        let (source_present, source_matches, tombstone_matches) = match probe {
            Ok(probe) => probe,
            Err(source) => {
                return CandidateSweep::Uncertain(invariant(format!(
                    "checkpoint prune rename could not be verified: {source}{}",
                    rename_error
                        .as_ref()
                        .map(|error| format!("; rename reported: {error}"))
                        .unwrap_or_default()
                )));
            }
        };

        if !source_present && tombstone_matches {
            // The namespace proves that the rename took effect, even when the
            // system call reported an error after applying it.
        } else if source_matches && !tombstone_matches {
            return CandidateSweep::Retained(rename_error.unwrap_or_else(|| {
                invariant(format!(
                    "checkpoint prune rename reported success but {checkpoint_id} remained committed"
                ))
            }));
        } else {
            return CandidateSweep::Uncertain(invariant(format!(
                "checkpoint prune rename has an uncertain namespace outcome{}",
                rename_error
                    .as_ref()
                    .map(|error| format!(": {error}"))
                    .unwrap_or_default()
            )));
        }

        let cleanup = (|| -> Result<()> {
            require_linked_directory(sandbox, &tombstone_name, &candidate.directory)?;
            sync_directory(sandbox)?;
            checkpoint_store_failpoint(
                "checkpoint-prune-after-tombstone",
                &sandbox.configured_path().join(&tombstone_name),
            )?;
            remove_owned_directory(sandbox, &tombstone_name, candidate.directory)?;
            sync_directory(sandbox)
        })();
        match cleanup {
            Ok(()) => CandidateSweep::Removed,
            Err(source) => CandidateSweep::RemovedWithUnfinishedCleanup(source),
        }
    }

    /// Return the persisted HEAD, if present.
    pub fn read_head(&self, sandbox_id: Uuid) -> Result<Option<String>> {
        let catalog = self.root()?;
        let Some(sandbox) = optional_child_directory(&catalog, &sandbox_id.to_string())? else {
            return Ok(None);
        };
        self.read_head_from(&sandbox, sandbox_id)
    }

    /// Return the recorded HEAD identifier without verifying its artifacts.
    ///
    /// Callers that only need to report which checkpoint HEAD names must use
    /// this instead of [`Self::read_head`]. Hashing a complete checkpoint would
    /// make the observation cost proportional to the guest image size, and an
    /// unreadable artifact would replace the recorded identifier with an
    /// integrity error exactly when a caller needs the identifier to describe
    /// an interrupted operation.
    pub fn read_head_id(&self, sandbox_id: Uuid) -> Result<Option<String>> {
        let catalog = self.root()?;
        let Some(sandbox) = optional_child_directory(&catalog, &sandbox_id.to_string())? else {
            return Ok(None);
        };
        self.read_head_id_from(&sandbox)
    }

    /// Remove every checkpoint artifact owned by one sandbox.
    ///
    /// A missing sandbox directory is already clean. Any unexpected entry or
    /// changed identity fails closed so the lifecycle owner can retain a
    /// recoverable destroy record instead of deleting an unrelated object.
    pub fn remove_sandbox(&self, sandbox_id: Uuid) -> Result<()> {
        enum OwnedEntry {
            Directory(std::ffi::OsString, OwnedStateDirectory),
            File(String, OwnedArtifact),
            Stray(std::ffi::OsString),
        }

        let catalog = self.root()?;
        let name = sandbox_id.to_string();
        let Some(sandbox) = optional_child_directory(&catalog, &name)? else {
            checkpoint_store_failpoint(
                "checkpoint-store-sandbox-remove-parent-sync",
                catalog.configured_path(),
            )?;
            return sync_directory(&catalog);
        };
        let mut names = directory_names_os(&sandbox, "scan sandbox checkpoint namespace")?;
        names.sort();

        let mut entries = Vec::with_capacity(names.len());
        for entry in names {
            let named_kind = entry.to_str().and_then(|entry| {
                if entry == HEAD_FILE {
                    Some(ScratchKind::File)
                } else if let Ok(Some(kind)) = classify_scratch_name(entry) {
                    Some(kind)
                } else if validate_checkpoint_id(entry).is_ok() {
                    Some(ScratchKind::Directory)
                } else {
                    None
                }
            });
            match named_kind {
                Some(ScratchKind::Directory) => entries.push(OwnedEntry::Directory(
                    entry.clone(),
                    required_child_directory_os(
                        &sandbox,
                        &entry,
                        "open owned checkpoint directory",
                    )?,
                )),
                Some(ScratchKind::File) => {
                    let name = entry
                        .to_str()
                        .expect("recognised checkpoint file names are UTF-8")
                        .to_owned();
                    entries.push(OwnedEntry::File(
                        name.clone(),
                        open_required_file(&sandbox, &name, "open owned checkpoint file")?,
                    ));
                }
                None => {
                    // An unrecognised entry can only be here if a capture escaped
                    // its payload path and dropped it directly in the namespace
                    // (publication now rejects that, but a record predating the
                    // check may still carry one). Destroy must reclaim it whatever
                    // its type, or the terminal transition is permanently blocked;
                    // classify it by inspecting the filesystem rather than the
                    // name.
                    let stat = statat(sandbox.descriptor(), &entry, AtFlags::SYMLINK_NOFOLLOW)
                        .map_err(|source| {
                            io_error(
                                "inspect stray checkpoint namespace entry",
                                sandbox.configured_path().join(&entry),
                                std::io::Error::from(source),
                            )
                        })?;
                    if FileType::from_raw_mode(stat.st_mode as _) == FileType::Directory {
                        entries.push(OwnedEntry::Directory(
                            entry.clone(),
                            required_child_directory_os(
                                &sandbox,
                                &entry,
                                "open stray checkpoint directory",
                            )?,
                        ));
                    } else {
                        entries.push(OwnedEntry::Stray(entry));
                    }
                }
            }
        }

        for entry in entries {
            match entry {
                OwnedEntry::Directory(name, directory) => {
                    remove_owned_directory_bounded(&sandbox, &name, directory, 0)?;
                }
                OwnedEntry::File(name, file) => remove_owned_file(&sandbox, &name, file)?,
                OwnedEntry::Stray(name) => {
                    unlinkat(sandbox.descriptor(), &name, AtFlags::empty()).map_err(|source| {
                        io_error(
                            "remove stray checkpoint namespace entry",
                            sandbox.configured_path().join(&name),
                            std::io::Error::from(source),
                        )
                    })?;
                }
            }
        }
        sync_directory(&sandbox)?;
        require_linked_directory(&catalog, &name, &sandbox)?;
        checkpoint_store_failpoint(
            "checkpoint-store-sandbox-remove-before-unlink",
            sandbox.configured_path(),
        )?;
        unlinkat(catalog.descriptor(), name.as_str(), AtFlags::REMOVEDIR).map_err(|source| {
            io_error(
                "remove sandbox checkpoint namespace",
                catalog.configured_path().join(&name),
                std::io::Error::from(source),
            )
        })?;
        checkpoint_store_failpoint(
            "checkpoint-store-sandbox-remove-parent-sync",
            catalog.configured_path(),
        )?;
        sync_directory(&catalog)
    }

    fn root(&self) -> Result<OwnedStateDirectory> {
        let mut root = self
            .root
            .lock()
            .map_err(|_| invariant("checkpoint root owner lock poisoned"))?;
        if let Some(root) = root.as_ref() {
            return Ok(root.clone());
        }
        let opened = self
            .state_store
            .checkpoint_directory()
            .map_err(CheckpointStoreError::State)?;
        *root = Some(opened.clone());
        Ok(opened)
    }

    fn ensure_sandbox_dir(
        &self,
        catalog: &OwnedStateDirectory,
        sandbox_id: Uuid,
    ) -> Result<OwnedStateDirectory> {
        let name = sandbox_id.to_string();
        match create_child_directory(catalog, &name) {
            Ok(directory) => {
                checkpoint_store_failpoint(
                    "checkpoint-store-sandbox-parent-sync",
                    catalog.configured_path(),
                )?;
                sync_directory(catalog)?;
                Ok(directory)
            }
            Err(CheckpointStoreError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::AlreadyExists =>
            {
                let directory =
                    required_child_directory(catalog, &name, "open checkpoint sandbox directory")?;
                checkpoint_store_failpoint(
                    "checkpoint-store-sandbox-parent-sync",
                    catalog.configured_path(),
                )?;
                sync_directory(catalog)?;
                Ok(directory)
            }
            Err(error) => Err(error),
        }
    }

    fn validate_stage(&self, stage: &CheckpointStage) -> Result<()> {
        validate_checkpoint_id(&stage.id)?;
        let root = self.root()?;
        if !same_directory(&root, &stage.catalog)? {
            return Err(invariant("checkpoint stage belongs to a different catalog"));
        }
        require_linked_directory(&root, &stage.sandbox_id.to_string(), &stage.sandbox)?;
        require_linked_directory(&stage.sandbox, &stage.staging_name, &stage.directory)?;
        if optional_child_directory(&stage.sandbox, &stage.id)?.is_some() {
            return Err(invariant(format!(
                "checkpoint publication target {} already exists",
                stage.sandbox.configured_path().join(&stage.id).display()
            )));
        }
        Ok(())
    }

    fn abort_owned_stage(
        &self,
        sandbox: &OwnedStateDirectory,
        staging_name: &str,
        stage: OwnedStateDirectory,
    ) -> Result<()> {
        require_linked_directory(sandbox, staging_name, &stage)?;
        let checkpoint_id = staging_name
            .strip_prefix('.')
            .and_then(|name| name.strip_suffix(STAGING_SUFFIX))
            .ok_or_else(|| invariant(format!("invalid staging name {staging_name:?}")))?;
        validate_checkpoint_id(checkpoint_id)?;
        let tombstone_name = format!(
            "{ABORT_TOMBSTONE_PREFIX}{checkpoint_id}.{}{TOMBSTONE_SUFFIX}",
            Uuid::new_v4()
        );
        checkpoint_store_failpoint(
            "checkpoint-store-abort-before-rename",
            &sandbox.configured_path().join(staging_name),
        )?;
        renameat_with(
            sandbox.descriptor(),
            staging_name,
            sandbox.descriptor(),
            tombstone_name.as_str(),
            RenameFlags::NOREPLACE,
        )
        .map_err(|source| {
            io_error(
                "tombstone aborted checkpoint stage",
                sandbox.configured_path().join(&tombstone_name),
                std::io::Error::from(source),
            )
        })?;
        require_linked_directory(sandbox, &tombstone_name, &stage)?;
        sync_directory(sandbox)?;
        remove_owned_directory(sandbox, &tombstone_name, stage)?;
        sync_directory(sandbox)
    }

    fn verified_checkpoint(
        &self,
        sandbox: &OwnedStateDirectory,
        sandbox_id: Uuid,
        checkpoint_id: &str,
    ) -> Result<VerifiedCheckpoint> {
        #[cfg(test)]
        self.verified_checkpoint_calls
            .fetch_add(1, Ordering::SeqCst);

        let LoadedCheckpointMetadata {
            metadata,
            directory,
            metadata_file,
            mut artifacts,
            payload,
        } = self.load_checkpoint_metadata(sandbox, sandbox_id, checkpoint_id)?;

        for (expected, (rel_path, artifact)) in metadata.artifacts.iter().zip(&mut artifacts) {
            if &expected.name != rel_path {
                return Err(invariant(format!(
                    "validated checkpoint {checkpoint_id} has no record for {rel_path}"
                )));
            }
            let actual = hash_artifact(artifact, rel_path)?;
            if &actual != expected {
                return Err(invariant(format!(
                    "checkpoint {checkpoint_id} artifact {rel_path} failed integrity validation"
                )));
            }
        }
        let verified = VerifiedCheckpoint {
            metadata,
            directory,
            metadata_file,
            artifacts,
            payload,
        };
        verified.require_linked(sandbox, checkpoint_id)?;
        Ok(verified)
    }

    fn load_checkpoint_metadata(
        &self,
        sandbox: &OwnedStateDirectory,
        sandbox_id: Uuid,
        checkpoint_id: &str,
    ) -> Result<LoadedCheckpointMetadata> {
        validate_checkpoint_id(checkpoint_id)?;
        let directory = required_child_directory(
            sandbox,
            checkpoint_id,
            "open committed checkpoint directory",
        )?;
        let mut metadata_file =
            open_required_file(&directory, METADATA_FILE, "open checkpoint metadata")?;
        let bytes = read_file(&mut metadata_file, "read checkpoint metadata")?;
        let metadata: CheckpointMetadata =
            serde_json::from_slice(&bytes).map_err(|source| CheckpointStoreError::Json {
                path: metadata_file.path.clone(),
                source,
            })?;
        validate_checkpoint_manifest(&metadata, sandbox_id, checkpoint_id)?;

        // The validated manifest is the inventory; the directory must agree
        // with it exactly. Version 1 keeps its frozen flat layout, version 2
        // is walked through the two producer subtrees.
        let (artifacts, payload) = if metadata.format_version == CHECKPOINT_FORMAT_V1 {
            let mut expected: Vec<&str> = metadata
                .artifacts
                .iter()
                .map(|artifact| artifact.name.as_str())
                .collect();
            expected.push(METADATA_FILE);
            validate_exact_entries(&directory, &expected)?;
            let mut artifacts = Vec::with_capacity(metadata.artifacts.len());
            for record in &metadata.artifacts {
                artifacts.push((
                    record.name.clone(),
                    open_required_file(&directory, &record.name, "open checkpoint artifact")?,
                ));
            }
            (
                artifacts,
                PayloadDirs {
                    backend: None,
                    storage: None,
                },
            )
        } else {
            validate_exact_entries(
                &directory,
                &[METADATA_FILE, PAYLOAD_BACKEND_DIR, PAYLOAD_STORAGE_DIR],
            )?;
            let backend_dir = required_child_directory(
                &directory,
                PAYLOAD_BACKEND_DIR,
                "open checkpoint payload subtree",
            )?;
            let storage_dir = required_child_directory(
                &directory,
                PAYLOAD_STORAGE_DIR,
                "open checkpoint payload subtree",
            )?;
            let mut found = Vec::new();
            let mut dirs = Vec::new();
            collect_payload_files(&backend_dir, PAYLOAD_BACKEND_DIR, 1, &mut found, &mut dirs)?;
            collect_payload_files(&storage_dir, PAYLOAD_STORAGE_DIR, 1, &mut found, &mut dirs)?;
            found.sort_by(|left, right| left.0.cmp(&right.0));
            let found_names: Vec<&str> = found.iter().map(|(name, _)| name.as_str()).collect();
            let manifest_names: Vec<&str> = metadata
                .artifacts
                .iter()
                .map(|artifact| artifact.name.as_str())
                .collect();
            if found_names != manifest_names {
                return Err(invariant(format!(
                    "checkpoint {checkpoint_id} payload does not match its manifest: \
                     found {found_names:?}, recorded {manifest_names:?}"
                )));
            }
            let rootfs_rel = format!("{PAYLOAD_STORAGE_DIR}/{STORAGE_ROOTFS_FILE}");
            if !manifest_names.contains(&rootfs_rel.as_str()) {
                return Err(invariant(format!(
                    "checkpoint {checkpoint_id} has no {rootfs_rel} capture"
                )));
            }
            (
                found,
                PayloadDirs {
                    backend: Some(backend_dir),
                    storage: Some(storage_dir),
                },
            )
        };
        let loaded = LoadedCheckpointMetadata {
            metadata,
            directory,
            metadata_file,
            artifacts,
            payload,
        };
        loaded.require_linked(sandbox, checkpoint_id)?;
        Ok(loaded)
    }

    fn validated_chain_from(
        &self,
        sandbox: &OwnedStateDirectory,
        sandbox_id: Uuid,
        checkpoint_id: &str,
    ) -> Result<Vec<String>> {
        validate_checkpoint_id(checkpoint_id)?;
        let mut current = checkpoint_id.to_string();
        let mut lineage = Vec::new();
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(current.clone()) {
                return Err(invariant(format!(
                    "checkpoint parent cycle reaches {current}"
                )));
            }
            let metadata = self
                .load_checkpoint_metadata(sandbox, sandbox_id, &current)?
                .metadata;
            lineage.push(current);
            let Some(parent) = metadata.parent else {
                break;
            };
            current = parent;
        }
        Ok(lineage)
    }

    fn load_catalog(
        &self,
        sandbox: &OwnedStateDirectory,
        sandbox_id: Uuid,
    ) -> Result<HashMap<String, CheckpointMetadata>> {
        let mut catalog = HashMap::new();
        for name in directory_names(sandbox, "scan checkpoint catalog")? {
            if !name.starts_with("ckpt-") {
                continue;
            }
            validate_checkpoint_id(&name)?;
            let metadata = self
                .load_checkpoint_metadata(sandbox, sandbox_id, &name)?
                .metadata;
            catalog.insert(name, metadata);
        }
        Ok(catalog)
    }

    fn verify_catalog_artifacts(
        &self,
        sandbox: &OwnedStateDirectory,
        sandbox_id: Uuid,
        catalog: &HashMap<String, CheckpointMetadata>,
    ) -> Result<()> {
        let mut checkpoint_ids: Vec<&str> = catalog.keys().map(String::as_str).collect();
        checkpoint_ids.sort_unstable();
        for checkpoint_id in checkpoint_ids {
            let verified = self.verified_checkpoint(sandbox, sandbox_id, checkpoint_id)?;
            if catalog.get(checkpoint_id) != Some(&verified.metadata) {
                return Err(invariant(format!(
                    "checkpoint {checkpoint_id} failed integrity validation: metadata changed during prune validation"
                )));
            }
        }
        Ok(())
    }

    fn read_head_id_from(&self, sandbox: &OwnedStateDirectory) -> Result<Option<String>> {
        let Some(mut head) = optional_file(sandbox, HEAD_FILE, "open checkpoint HEAD")? else {
            return Ok(None);
        };
        let bytes = read_file(&mut head, "read checkpoint HEAD")?;
        require_linked_file(sandbox, HEAD_FILE, &head)?;
        let raw = std::str::from_utf8(&bytes)
            .map_err(|error| invariant(format!("checkpoint HEAD is not UTF-8: {error}")))?;
        let checkpoint_id = raw
            .strip_suffix('\n')
            .filter(|value| !value.contains('\n') && !value.contains('\r'))
            .ok_or_else(|| invariant("checkpoint HEAD is not one canonical line"))?;
        validate_checkpoint_id(checkpoint_id)?;
        Ok(Some(checkpoint_id.to_string()))
    }

    fn read_head_from(
        &self,
        sandbox: &OwnedStateDirectory,
        sandbox_id: Uuid,
    ) -> Result<Option<String>> {
        let checkpoint_id = self.read_head_id_from(sandbox)?;
        if let Some(checkpoint_id) = checkpoint_id.as_deref() {
            let _verified = self.verified_checkpoint(sandbox, sandbox_id, checkpoint_id)?;
        }
        Ok(checkpoint_id)
    }

    #[cfg(test)]
    fn set_before_publish_revalidation<F>(&self, hook: F)
    where
        F: FnOnce() + Send + 'static,
    {
        *self
            .before_publish_revalidation
            .lock()
            .expect("checkpoint test hook lock") = Some(Box::new(hook));
    }

    #[cfg(test)]
    fn verified_checkpoint_count(&self) -> usize {
        self.verified_checkpoint_calls.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    fn run_before_publish_revalidation(&self) {
        if let Some(hook) = self
            .before_publish_revalidation
            .lock()
            .expect("checkpoint test hook lock")
            .take()
        {
            hook();
        }
    }

    #[cfg(test)]
    fn configured_root(&self) -> PathBuf {
        self.root()
            .expect("open checkpoint root")
            .configured_path()
            .to_path_buf()
    }
}

#[derive(Clone, Copy)]
enum ScratchKind {
    Directory,
    File,
}

fn lineage_from(
    catalog: &HashMap<String, CheckpointMetadata>,
    checkpoint_id: &str,
) -> Result<HashSet<String>> {
    let mut current = checkpoint_id.to_string();
    let mut lineage = HashSet::new();
    loop {
        if !lineage.insert(current.clone()) {
            return Err(invariant(format!(
                "checkpoint parent cycle reaches {current}"
            )));
        }
        let metadata = catalog.get(&current).ok_or_else(|| {
            invariant(format!(
                "checkpoint lineage references missing parent {current}"
            ))
        })?;
        let Some(parent) = &metadata.parent else {
            break;
        };
        current = parent.clone();
    }
    Ok(lineage)
}

fn validate_catalog_lineages(catalog: &HashMap<String, CheckpointMetadata>) -> Result<()> {
    let mut complete = HashSet::new();
    for checkpoint_id in catalog.keys() {
        if complete.contains(checkpoint_id) {
            continue;
        }
        let mut current = checkpoint_id.clone();
        let mut branch = HashSet::new();
        loop {
            if complete.contains(&current) {
                break;
            }
            if !branch.insert(current.clone()) {
                return Err(invariant(format!(
                    "checkpoint parent cycle reaches {current}"
                )));
            }
            let metadata = catalog.get(&current).ok_or_else(|| {
                invariant(format!(
                    "checkpoint lineage references missing parent {current}"
                ))
            })?;
            let Some(parent) = &metadata.parent else {
                break;
            };
            current = parent.clone();
        }
        complete.extend(branch);
    }
    Ok(())
}

fn create_child_directory(parent: &OwnedStateDirectory, name: &str) -> Result<OwnedStateDirectory> {
    mkdirat(parent.descriptor(), name, CHECKPOINT_DIRECTORY_MODE).map_err(|source| {
        io_error(
            "create checkpoint directory",
            parent.configured_path().join(name),
            std::io::Error::from(source),
        )
    })?;
    match open_child_directory(parent, name) {
        Ok(directory) => Ok(directory),
        Err(error) => {
            let _ = unlinkat(parent.descriptor(), name, AtFlags::REMOVEDIR);
            Err(error)
        }
    }
}

fn open_child_directory(parent: &OwnedStateDirectory, name: &str) -> Result<OwnedStateDirectory> {
    let directory = openat(
        parent.descriptor(),
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|source| {
        io_error(
            "open checkpoint directory",
            parent.configured_path().join(name),
            std::io::Error::from(source),
        )
    })?;
    Ok(OwnedStateDirectory::new(
        parent.configured_path().join(name),
        directory,
    ))
}

fn optional_child_directory(
    parent: &OwnedStateDirectory,
    name: &str,
) -> Result<Option<OwnedStateDirectory>> {
    match openat(
        parent.descriptor(),
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(directory) => Ok(Some(OwnedStateDirectory::new(
            parent.configured_path().join(name),
            directory,
        ))),
        Err(Errno::NOENT) => Ok(None),
        Err(source) => Err(io_error(
            "open checkpoint directory",
            parent.configured_path().join(name),
            std::io::Error::from(source),
        )),
    }
}

fn required_child_directory(
    parent: &OwnedStateDirectory,
    name: &str,
    operation: &'static str,
) -> Result<OwnedStateDirectory> {
    optional_child_directory(parent, name)?.ok_or_else(|| {
        io_error(
            operation,
            parent.configured_path().join(name),
            std::io::Error::from(std::io::ErrorKind::NotFound),
        )
    })
}

/// Open a child directory by a name that need not be valid UTF-8.
///
/// Only removal needs this: publication rejects non-UTF-8 names, so no other
/// path can be holding one.
fn required_child_directory_os(
    parent: &OwnedStateDirectory,
    name: &std::ffi::OsStr,
    operation: &'static str,
) -> Result<OwnedStateDirectory> {
    let path = parent.configured_path().join(name);
    let descriptor = openat(
        parent.descriptor(),
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|source| io_error(operation, &path, std::io::Error::from(source)))?;
    Ok(OwnedStateDirectory::new(path, descriptor))
}

fn create_new_file(
    directory: &OwnedStateDirectory,
    name: &str,
    operation: &'static str,
) -> Result<OwnedArtifact> {
    let path = directory.configured_path().join(name);
    let descriptor = openat(
        directory.descriptor(),
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        CHECKPOINT_FILE_MODE,
    )
    .map_err(|source| io_error(operation, &path, std::io::Error::from(source)))?;
    Ok(OwnedArtifact {
        path,
        file: File::from(descriptor),
    })
}

fn open_required_file(
    directory: &OwnedStateDirectory,
    name: &str,
    operation: &'static str,
) -> Result<OwnedArtifact> {
    optional_file(directory, name, operation)?.ok_or_else(|| {
        io_error(
            operation,
            directory.configured_path().join(name),
            std::io::Error::from(std::io::ErrorKind::NotFound),
        )
    })
}

fn optional_file(
    directory: &OwnedStateDirectory,
    name: &str,
    operation: &'static str,
) -> Result<Option<OwnedArtifact>> {
    let path = directory.configured_path().join(name);
    let descriptor = match openat(
        directory.descriptor(),
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(descriptor) => descriptor,
        Err(Errno::NOENT) => return Ok(None),
        Err(source) => return Err(io_error(operation, &path, std::io::Error::from(source))),
    };
    let file = File::from(descriptor);
    let metadata = file
        .metadata()
        .map_err(|source| io_error("inspect checkpoint file", &path, source))?;
    if !metadata.is_file() {
        return Err(invariant(format!(
            "checkpoint file {} is not a regular file",
            path.display()
        )));
    }
    Ok(Some(OwnedArtifact { path, file }))
}

fn validate_checkpoint_artifact_owner(artifact: &OwnedArtifact) -> Result<()> {
    let metadata = fstat(&artifact.file).map_err(|source| {
        io_error(
            "inspect checkpoint file",
            &artifact.path,
            std::io::Error::from(source),
        )
    })?;
    let expected_uid = unsafe { libc::geteuid() };
    if metadata.st_uid != expected_uid {
        return Err(invariant(format!(
            "checkpoint artifact {} is not owned by the daemon user",
            artifact.path.display()
        )));
    }
    if metadata.st_nlink != 1 {
        return Err(invariant(format!(
            "checkpoint artifact {} must have exactly one hard link",
            artifact.path.display()
        )));
    }
    Ok(())
}

fn validate_exact_entries(directory: &OwnedStateDirectory, expected: &[&str]) -> Result<()> {
    let actual = directory_names(directory, "scan checkpoint directory")?;
    let expected: HashSet<String> = expected.iter().map(|name| (*name).to_string()).collect();
    if actual != expected {
        let mut unexpected: Vec<_> = actual.difference(&expected).cloned().collect();
        let mut missing: Vec<_> = expected.difference(&actual).cloned().collect();
        unexpected.sort();
        missing.sort();
        return Err(invariant(format!(
            "checkpoint directory {} has unexpected entries {:?} and missing entries {:?}",
            directory.configured_path().display(),
            unexpected,
            missing
        )));
    }
    Ok(())
}

fn directory_names(
    directory: &OwnedStateDirectory,
    operation: &'static str,
) -> Result<HashSet<String>> {
    // Open a fresh description so directory offsets from an earlier scan are
    // never reused through the retained owner.
    let scan = openat(
        directory.descriptor(),
        ".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|source| {
        io_error(
            operation,
            directory.configured_path(),
            std::io::Error::from(source),
        )
    })?;
    let entries = Dir::read_from(&scan).map_err(|source| {
        io_error(
            operation,
            directory.configured_path(),
            std::io::Error::from(source),
        )
    })?;
    let mut names = HashSet::new();
    for entry in entries {
        let entry = entry.map_err(|source| {
            io_error(
                operation,
                directory.configured_path(),
                std::io::Error::from(source),
            )
        })?;
        let Some(name) = entry.file_name().to_str().ok() else {
            return Err(invariant(format!(
                "checkpoint directory {} contains a non-UTF-8 name",
                directory.configured_path().display()
            )));
        };
        if name != "." && name != ".." {
            names.insert(name.to_string());
        }
    }
    Ok(names)
}

/// Enumerate every directory entry by raw `OsString`, including names that
/// are not valid UTF-8.
///
/// Publication rejects non-UTF-8 names, but removal must be able to clean
/// up a directory a backend populated with arbitrary names: rejecting the
/// scan would strand the sandbox in `RecoveryRequired` with destroy as the
/// only exit, and destroy itself calls this removal path.
fn directory_names_os(
    directory: &OwnedStateDirectory,
    operation: &'static str,
) -> Result<Vec<std::ffi::OsString>> {
    let scan = openat(
        directory.descriptor(),
        ".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|source| {
        io_error(
            operation,
            directory.configured_path(),
            std::io::Error::from(source),
        )
    })?;
    let entries = Dir::read_from(&scan).map_err(|source| {
        io_error(
            operation,
            directory.configured_path(),
            std::io::Error::from(source),
        )
    })?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| {
            io_error(
                operation,
                directory.configured_path(),
                std::io::Error::from(source),
            )
        })?;
        let name = entry.file_name();
        let name_bytes = name.to_bytes();
        if name_bytes != b"." && name_bytes != b".." {
            names.push(std::ffi::OsStr::from_bytes(name_bytes).to_os_string());
        }
    }
    Ok(names)
}

fn hash_artifact(artifact: &mut OwnedArtifact, name: &str) -> Result<CheckpointArtifact> {
    artifact
        .file
        .seek(SeekFrom::Start(0))
        .map_err(|source| io_error("rewind checkpoint artifact", &artifact.path, source))?;
    let mut hasher = Sha256::new();
    let mut size_bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = artifact
            .file
            .read(&mut buffer)
            .map_err(|source| io_error("read checkpoint artifact", &artifact.path, source))?;
        if read == 0 {
            break;
        }
        size_bytes = size_bytes
            .checked_add(read as u64)
            .ok_or_else(|| invariant(format!("checkpoint artifact {name} size overflow")))?;
        hasher.update(&buffer[..read]);
    }
    Ok(CheckpointArtifact {
        name: name.to_string(),
        size_bytes,
        sha256: format!("{:x}", hasher.finalize()),
    })
}

fn write_json_new<T: serde::Serialize>(
    directory: &OwnedStateDirectory,
    name: &str,
    value: &T,
) -> Result<OwnedArtifact> {
    let path = directory.configured_path().join(name);
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|source| CheckpointStoreError::Json { path, source })?;
    let mut file = create_new_file(directory, name, "create checkpoint metadata")?;
    write_all(&mut file.file, &file.path, &bytes)?;
    write_all(&mut file.file, &file.path, b"\n")?;
    file.file
        .sync_all()
        .map_err(|source| io_error("sync checkpoint metadata", &file.path, source))?;
    Ok(file)
}

fn read_file(file: &mut OwnedArtifact, operation: &'static str) -> Result<Vec<u8>> {
    file.file
        .seek(SeekFrom::Start(0))
        .map_err(|source| io_error("rewind checkpoint file", &file.path, source))?;
    let mut bytes = Vec::new();
    file.file
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(operation, &file.path, source))?;
    Ok(bytes)
}

fn write_all(file: &mut File, path: &Path, bytes: &[u8]) -> Result<()> {
    file.write_all(bytes)
        .map_err(|source| io_error("write checkpoint file", path, source))
}

fn require_linked_directory(
    parent: &OwnedStateDirectory,
    name: &str,
    expected: &OwnedStateDirectory,
) -> Result<()> {
    let linked = optional_child_directory(parent, name)?.ok_or_else(|| {
        invariant(format!(
            "checkpoint directory {} disappeared",
            parent.configured_path().join(name).display()
        ))
    })?;
    if !same_directory(&linked, expected)? {
        return Err(invariant(format!(
            "checkpoint directory {} changed identity",
            parent.configured_path().join(name).display()
        )));
    }
    Ok(())
}

fn require_linked_directory_os(
    parent: &OwnedStateDirectory,
    name: &std::ffi::OsStr,
    expected: &OwnedStateDirectory,
) -> Result<()> {
    let linked = required_child_directory_os(parent, name, "revalidate checkpoint directory")?;
    if !same_directory(&linked, expected)? {
        return Err(invariant(format!(
            "checkpoint directory {} changed identity",
            parent.configured_path().join(name).display()
        )));
    }
    Ok(())
}

fn require_linked_file(
    parent: &OwnedStateDirectory,
    name: &str,
    expected: &OwnedArtifact,
) -> Result<()> {
    let linked = open_required_file(parent, name, "revalidate checkpoint file")?;
    let expected_stat = fstat(&expected.file).map_err(|source| {
        io_error(
            "inspect checkpoint file",
            &expected.path,
            std::io::Error::from(source),
        )
    })?;
    let linked_stat = fstat(&linked.file).map_err(|source| {
        io_error(
            "inspect checkpoint file",
            &linked.path,
            std::io::Error::from(source),
        )
    })?;
    if expected_stat.st_dev != linked_stat.st_dev || expected_stat.st_ino != linked_stat.st_ino {
        return Err(invariant(format!(
            "checkpoint file {} changed identity",
            parent.configured_path().join(name).display()
        )));
    }
    Ok(())
}

fn same_directory(left: &OwnedStateDirectory, right: &OwnedStateDirectory) -> Result<bool> {
    let left = fstat(left.descriptor()).map_err(|source| {
        io_error(
            "inspect checkpoint directory",
            left.configured_path(),
            std::io::Error::from(source),
        )
    })?;
    let right = fstat(right.descriptor()).map_err(|source| {
        io_error(
            "inspect checkpoint directory",
            right.configured_path(),
            std::io::Error::from(source),
        )
    })?;
    Ok(left.st_dev == right.st_dev && left.st_ino == right.st_ino)
}

fn sync_directory(directory: &OwnedStateDirectory) -> Result<()> {
    fsync(directory.descriptor()).map_err(|source| {
        io_error(
            "sync checkpoint directory",
            directory.configured_path(),
            std::io::Error::from(source),
        )
    })
}

fn remove_owned_file(parent: &OwnedStateDirectory, name: &str, file: OwnedArtifact) -> Result<()> {
    require_linked_file(parent, name, &file)?;
    unlinkat(parent.descriptor(), name, AtFlags::empty()).map_err(|source| {
        io_error(
            "remove checkpoint scratch file",
            parent.configured_path().join(name),
            std::io::Error::from(source),
        )
    })
}

fn remove_file_if_exists(parent: &OwnedStateDirectory, name: &str) -> Result<()> {
    match unlinkat(parent.descriptor(), name, AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(source) => Err(io_error(
            "remove checkpoint temporary file",
            parent.configured_path().join(name),
            std::io::Error::from(source),
        )),
    }
}

fn remove_owned_directory(
    parent: &OwnedStateDirectory,
    name: &str,
    directory: OwnedStateDirectory,
) -> Result<()> {
    remove_owned_directory_bounded(parent, std::ffi::OsStr::new(name), directory, 0)
}

/// Remove one owned directory and everything below it, fail-closed.
///
/// Committed checkpoints carry backend-private subtrees, so removal must
/// recurse; the depth bound keeps a corrupted or hostile tree from walking
/// the daemon into unbounded work.
///
/// Entries are enumerated as raw `OsString`, so a name a backend created that
/// is not valid UTF-8 is still removable. Publication rejects such a name, but
/// refusing to remove one would strand the sandbox in `RecoveryRequired` with
/// destroy as its only exit, and destroy runs this same path.
///
fn remove_owned_directory_bounded(
    parent: &OwnedStateDirectory,
    name: &std::ffi::OsStr,
    directory: OwnedStateDirectory,
    depth: usize,
) -> Result<()> {
    if depth > MAX_REMOVAL_DEPTH {
        return Err(invariant(format!(
            "checkpoint directory {} is nested too deeply to remove",
            directory.configured_path().display()
        )));
    }
    require_linked_directory_os(parent, name, &directory)?;
    let mut entries = directory_names_os(&directory, "scan checkpoint scratch directory")?;
    entries.sort();
    for entry in entries {
        // Classification is deliberately laxer than payload validation:
        // publication rejects symlinks, FIFOs, sockets, and devices, and
        // removal must be able to delete exactly those rejected entries,
        // so anything that is not a directory is simply unlinked.
        let stat = statat(directory.descriptor(), &entry, AtFlags::SYMLINK_NOFOLLOW).map_err(
            |source| {
                io_error(
                    "inspect checkpoint scratch entry",
                    directory.configured_path().join(&entry),
                    std::io::Error::from(source),
                )
            },
        )?;
        if FileType::from_raw_mode(stat.st_mode as _) == FileType::Directory {
            let child = required_child_directory_os(
                &directory,
                &entry,
                "open checkpoint scratch directory",
            )?;
            remove_owned_directory_bounded(&directory, &entry, child, depth + 1)?;
        } else {
            unlinkat(directory.descriptor(), &entry, AtFlags::empty()).map_err(|source| {
                io_error(
                    "remove checkpoint scratch file",
                    directory.configured_path().join(&entry),
                    std::io::Error::from(source),
                )
            })?;
        }
    }
    sync_directory(&directory)?;
    require_linked_directory_os(parent, name, &directory)?;
    unlinkat(parent.descriptor(), name, AtFlags::REMOVEDIR).map_err(|source| {
        io_error(
            "remove checkpoint scratch directory",
            parent.configured_path().join(name),
            std::io::Error::from(source),
        )
    })
}

/// One directory entry pinned by an open descriptor.
enum PayloadEntry {
    Directory(OwnedStateDirectory),
    File(OwnedArtifact),
}

/// Open one payload entry without following symbolic links.
///
/// Payloads may only contain regular files and directories: a symbolic link
/// could redirect hashing or restore outside the checkpoint, and any other
/// object kind has no defined capture semantics, so both fail closed.
fn open_payload_entry(parent: &OwnedStateDirectory, name: &str) -> Result<PayloadEntry> {
    let path = parent.configured_path().join(name);
    let descriptor = openat(
        parent.descriptor(),
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|source| {
        io_error(
            "open checkpoint payload entry",
            &path,
            std::io::Error::from(source),
        )
    })?;
    let stat = fstat(&descriptor).map_err(|source| {
        io_error(
            "inspect checkpoint payload entry",
            &path,
            std::io::Error::from(source),
        )
    })?;
    match FileType::from_raw_mode(stat.st_mode as _) {
        FileType::Directory => Ok(PayloadEntry::Directory(OwnedStateDirectory::new(
            path, descriptor,
        ))),
        FileType::RegularFile => Ok(PayloadEntry::File(OwnedArtifact {
            path,
            file: File::from(descriptor),
        })),
        _ => Err(invariant(format!(
            "checkpoint payload entry {} is neither a regular file nor a directory",
            path.display()
        ))),
    }
}

/// Walk one payload subtree, collecting every regular file with its
/// slash-separated path relative to the checkpoint root, plus every
/// directory visited so callers can sync or revalidate them. Returns the
/// number of files found below `directory`.
///
/// Empty directories are rejected: the manifest inventories files only, so
/// an empty directory would be invisible to integrity validation while the
/// producing backend might rely on it.
fn collect_payload_files(
    directory: &OwnedStateDirectory,
    prefix: &str,
    depth: usize,
    files: &mut Vec<(String, OwnedArtifact)>,
    dirs: &mut Vec<OwnedStateDirectory>,
) -> Result<usize> {
    if depth > MAX_PAYLOAD_DEPTH {
        return Err(invariant(format!(
            "checkpoint payload at {} is nested too deeply",
            directory.configured_path().display()
        )));
    }
    let mut names: Vec<_> = directory_names(directory, "scan checkpoint payload")?
        .into_iter()
        .collect();
    names.sort();
    let mut file_count = 0_usize;
    for name in names {
        let rel_path = format!("{prefix}/{name}");
        // Reject a path the manifest format cannot carry while the producer
        // is still compensable, instead of committing an inventory that
        // every later load would refuse.
        validate_artifact_path(&rel_path).map_err(|error| invariant(error.to_string()))?;
        match open_payload_entry(directory, &name)? {
            PayloadEntry::Directory(child) => {
                let child_files = collect_payload_files(&child, &rel_path, depth + 1, files, dirs)?;
                if child_files == 0 {
                    return Err(invariant(format!(
                        "checkpoint payload directory {rel_path} is empty and cannot be \
                         carried by the manifest inventory"
                    )));
                }
                file_count += child_files;
                dirs.push(child);
            }
            PayloadEntry::File(file) => {
                files.push((rel_path, file));
                file_count += 1;
            }
        }
    }
    Ok(file_count)
}

/// Revalidate that the file at `rel_path` below `root` is still the retained
/// artifact, walking each path segment through directory descriptors.
fn require_linked_path(
    root: &OwnedStateDirectory,
    rel_path: &str,
    expected: &OwnedArtifact,
) -> Result<()> {
    let mut current = root.clone();
    let mut segments = rel_path.split('/').peekable();
    while let Some(segment) = segments.next() {
        if segments.peek().is_none() {
            return require_linked_file(&current, segment, expected);
        }
        current =
            required_child_directory(&current, segment, "revalidate checkpoint payload directory")?;
    }
    Err(invariant(format!(
        "checkpoint artifact path {rel_path:?} is empty"
    )))
}

/// Reject a stray entry a capture may have created directly in the sandbox
/// checkpoint namespace by escaping its configured payload path.
///
/// Only three kinds of name may legitimately appear there: `HEAD`, a scratch
/// name (recognised by [`classify_scratch_name`]), and a committed checkpoint
/// id. A capture that escaped through a parent component such as
/// `payload_dir.join("../../capture.log")` can instead leave an unrecognised
/// entry beside the checkpoints. Publication would keep it, and a later
/// `remove_sandbox` would fail `validate_checkpoint_id` on it and strand the
/// sandbox in `RecoveryRequired`. Rejecting it before the capture commits
/// leaves the checkpoint uncommitted and the sandbox untouched. The active
/// staging directory is expected and skipped.
fn require_publishable_sandbox_namespace(
    sandbox: &OwnedStateDirectory,
    staging_name: &str,
) -> Result<()> {
    for name in directory_names(sandbox, "scan sandbox checkpoint namespace")? {
        if name == staging_name || name == HEAD_FILE {
            continue;
        }
        if classify_scratch_name(&name)?.is_some() {
            continue;
        }
        if validate_checkpoint_id(&name).is_err() {
            return Err(invariant(format!(
                "sandbox checkpoint namespace entry {} is neither a checkpoint, \
                 HEAD, nor recognised scratch",
                sandbox.configured_path().join(&name).display()
            )));
        }
    }
    Ok(())
}

/// Require the complete sandbox checkpoint namespace to be stable before a
/// destructive prune is planned.
///
/// The lifecycle manager serialises checkpoint operations, so a prunable
/// running sandbox can contain only committed checkpoints and the optional
/// HEAD file. A staging name, cleanup tombstone, or any other entry means the
/// namespace does not match that lifecycle state. Rejecting it before loading
/// the catalog prevents a reduced view from selecting otherwise valid
/// checkpoints for deletion. Read-only catalog listing deliberately keeps its
/// existing behavior and may ignore a stage that a failed capture still owns.
fn require_prunable_sandbox_namespace(sandbox: &OwnedStateDirectory) -> Result<()> {
    for name in directory_names(sandbox, "scan prunable checkpoint namespace")? {
        if name == HEAD_FILE || validate_checkpoint_id(&name).is_ok() {
            continue;
        }
        return Err(invariant(format!(
            "sandbox checkpoint namespace entry {} is neither HEAD nor a committed checkpoint",
            sandbox.configured_path().join(&name).display()
        )));
    }
    Ok(())
}

fn classify_scratch_name(name: &str) -> Result<Option<ScratchKind>> {
    if let Some(checkpoint_id) = name
        .strip_prefix('.')
        .and_then(|name| name.strip_suffix(STAGING_SUFFIX))
        .filter(|name| name.starts_with("ckpt-"))
    {
        validate_checkpoint_id(checkpoint_id)?;
        return Ok(Some(ScratchKind::Directory));
    }
    if let Some(nonce) = name
        .strip_prefix(".HEAD.")
        .and_then(|name| name.strip_suffix(STAGING_SUFFIX))
    {
        parse_uuid_component(nonce, "checkpoint HEAD staging")?;
        return Ok(Some(ScratchKind::File));
    }
    if let Some(body) = name
        .strip_prefix(ABORT_TOMBSTONE_PREFIX)
        .and_then(|name| name.strip_suffix(TOMBSTONE_SUFFIX))
    {
        let (checkpoint_id, nonce) = body
            .rsplit_once('.')
            .ok_or_else(|| invariant(format!("invalid checkpoint tombstone name {name:?}")))?;
        validate_checkpoint_id(checkpoint_id)?;
        parse_uuid_component(nonce, "checkpoint tombstone")?;
        return Ok(Some(ScratchKind::Directory));
    }
    if let Some(body) = name
        .strip_prefix(PRUNE_TOMBSTONE_PREFIX)
        .and_then(|name| name.strip_suffix(TOMBSTONE_SUFFIX))
    {
        parse_prune_scratch_key(body, name)?;
        return Ok(Some(ScratchKind::Directory));
    }
    Ok(None)
}

fn prune_tombstone_name(key: &PruneScratchKey) -> String {
    format!(
        "{PRUNE_TOMBSTONE_PREFIX}{}.{}{TOMBSTONE_SUFFIX}",
        key.checkpoint_id, key.nonce
    )
}

fn parse_prune_scratch_key(body: &str, name: &str) -> Result<PruneScratchKey> {
    let (checkpoint_id, nonce) = body
        .rsplit_once('.')
        .ok_or_else(|| invariant(format!("invalid checkpoint prune tombstone name {name:?}")))?;
    validate_checkpoint_id(checkpoint_id)?;
    Ok(PruneScratchKey {
        checkpoint_id: checkpoint_id.to_string(),
        nonce: parse_uuid_component(nonce, "checkpoint prune tombstone")?,
    })
}

fn parse_uuid_component(value: &str, label: &str) -> Result<Uuid> {
    let uuid = Uuid::parse_str(value)
        .map_err(|error| invariant(format!("invalid {label} identifier {value:?}: {error}")))?;
    if value != uuid.to_string() {
        return Err(invariant(format!(
            "{label} identifier {value:?} is not canonical"
        )));
    }
    Ok(uuid)
}

fn io_error(
    operation: &'static str,
    path: impl AsRef<Path>,
    source: std::io::Error,
) -> CheckpointStoreError {
    CheckpointStoreError::Io {
        operation,
        path: path.as_ref().to_path_buf(),
        source,
    }
}

fn invariant(message: impl Into<String>) -> CheckpointStoreError {
    CheckpointStoreError::Invariant(message.into())
}

fn checkpoint_store_failpoint(name: &'static str, path: &Path) -> Result<()> {
    crate::failpoint::storage(name).map_err(|error| {
        io_error(
            "run checkpoint store failpoint",
            path,
            std::io::Error::other(error.to_string()),
        )
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use blaze_core::backend::{BackendKind, SnapshotKind};

    use super::*;

    fn store(temp: &tempfile::TempDir) -> CheckpointStore {
        let state_root = temp.path().join("state");
        fs::create_dir(&state_root).expect("state root");
        CheckpointStore::new(StateStore::new(state_root))
    }

    fn commit_input(parent: Option<String>) -> CommitCheckpoint {
        CommitCheckpoint {
            parent,
            policy_name: "default".to_string(),
            image_digest: "sha256:test".to_string(),
            backend: BackendKind::Mock,
            backend_version: Some("mock-v1".to_string()),
            snapshot_kind: SnapshotKind::Full,
        }
    }

    /// Relative payload layout used by store tests: the classic VM pair in
    /// the backend subtree and the rootfs in the storage subtree.
    const TEST_ARTIFACTS: [(&str, &str); 3] = [
        ("backend", "memory.snap"),
        ("backend", "vmstate.snap"),
        ("storage", "rootfs.snap"),
    ];

    fn stage_subtree<'stage>(
        stage: &'stage CheckpointStage,
        subtree: &str,
    ) -> &'stage OwnedStateDirectory {
        match subtree {
            "backend" => &stage.backend_dir,
            "storage" => &stage.storage_dir,
            other => panic!("unknown payload subtree {other}"),
        }
    }

    fn populate(stage: &CheckpointStage, suffix: &str) {
        for (subtree, name) in TEST_ARTIFACTS {
            let mut artifact =
                create_new_file(stage_subtree(stage, subtree), name, "create test artifact")
                    .expect("create artifact");
            artifact
                .file
                .write_all(format!("{name}-{suffix}").as_bytes())
                .expect("write artifact");
        }
    }

    fn publish(
        store: &CheckpointStore,
        sandbox_id: Uuid,
        parent: Option<String>,
        move_head: bool,
    ) -> String {
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        let id = stage.id().to_string();
        populate(&stage, &id);
        store
            .publish(&stage, commit_input(parent))
            .expect("publish checkpoint");
        if move_head {
            store.set_head(sandbox_id, &id).expect("move HEAD");
        }
        id
    }

    #[test]
    fn new_captures_commit_the_subtree_format() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let checkpoint_id = publish(&store, sandbox_id, None, true);

        let metadata = store.verify(sandbox_id, &checkpoint_id).expect("verify");
        assert_eq!(metadata.format_version, CHECKPOINT_FORMAT_VERSION);
        let names: Vec<&str> = metadata
            .artifacts
            .iter()
            .map(|artifact| artifact.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "backend/memory.snap",
                "backend/vmstate.snap",
                "storage/rootfs.snap"
            ],
            "the manifest inventories producer subtrees in sorted order"
        );
    }

    #[test]
    fn publish_rejects_an_empty_payload() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let stage = store.begin(sandbox_id).expect("begin checkpoint");

        let error = store
            .publish(&stage, commit_input(None))
            .expect_err("a checkpoint without any artifact is meaningless");

        assert_eq!(error.outcome(), CheckpointPublishOutcome::KnownUnpublished);
        assert!(error.to_string().contains("empty payload"));
        assert!(store.list(sandbox_id).expect("list").is_empty());
    }

    #[test]
    fn publish_rejects_a_payload_without_the_rootfs_capture() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        // Only the backend produced output; the storage provider captured
        // nothing. Restore consumes the rootfs unconditionally, so this
        // checkpoint must never publish as valid.
        let mut artifact =
            create_new_file(&stage.backend_dir, "vmstate.snap", "create test artifact")
                .expect("create artifact");
        artifact
            .file
            .write_all(b"vm state only")
            .expect("write artifact");

        let error = store
            .publish(&stage, commit_input(None))
            .expect_err("a checkpoint without a rootfs capture is unrestorable");

        assert_eq!(error.outcome(), CheckpointPublishOutcome::KnownUnpublished);
        assert!(error.to_string().contains("storage/rootfs.snap"));
        assert!(store.list(sandbox_id).expect("list").is_empty());
    }

    #[test]
    fn publish_rejects_an_empty_payload_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        populate(&stage, "empty-dir");
        fs::create_dir(stage.backend_payload_dir().join("hollow"))
            .expect("empty payload directory");

        let error = store
            .publish(&stage, commit_input(None))
            .expect_err("an empty payload directory is invisible to the manifest");

        assert_eq!(error.outcome(), CheckpointPublishOutcome::KnownUnpublished);
        assert!(error.to_string().contains("is empty"));
        store
            .abort(stage)
            .expect("a rejected payload must remain removable");
    }

    #[test]
    fn publish_rejects_an_empty_backend_subtree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        // Write only the storage rootfs capture; leave the backend subtree empty.
        std::fs::write(stage.storage_payload_dir().join("rootfs.snap"), b"rootfs")
            .expect("rootfs capture");

        let error = store
            .publish(&stage, commit_input(None))
            .expect_err("an empty backend subtree must fail even with rootfs present");

        assert_eq!(error.outcome(), CheckpointPublishOutcome::KnownUnpublished);
        assert!(error.to_string().contains("empty backend payload subtree"));
        store
            .abort(stage)
            .expect("a rejected payload must remain removable");
    }

    #[test]
    fn publish_rejects_a_stray_entry_in_the_sandbox_namespace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        let id = stage.id().to_string();
        populate(&stage, &id);
        // A capture that escaped its payload path through `../../capture.log`
        // lands an entry directly in the sandbox checkpoint namespace. It is
        // neither a checkpoint id, HEAD, nor recognised scratch, so a later
        // destroy would fail on it. Publication must reject it now.
        std::fs::write(
            stage.sandbox.configured_path().join("capture.log"),
            b"escaped",
        )
        .expect("stray namespace entry");

        let error = store
            .publish(&stage, commit_input(None))
            .expect_err("a stray namespace entry must block publication");
        assert_eq!(error.outcome(), CheckpointPublishOutcome::KnownUnpublished);
        assert!(error.to_string().contains("neither a checkpoint"));
        store
            .abort(stage)
            .expect("a rejected capture must remain removable");
    }

    #[test]
    fn remove_sandbox_reclaims_a_stray_namespace_entry() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        // Publish a real checkpoint so the sandbox namespace exists, then plant
        // a stray file beside it as an escaped capture would. Destroy must
        // reclaim the whole namespace instead of failing `validate_checkpoint_id`
        // and stranding the sandbox.
        let id = publish(&store, sandbox_id, None, true);
        let catalog = store.root().expect("catalog");
        let sandbox = optional_child_directory(&catalog, &sandbox_id.to_string())
            .expect("open sandbox")
            .expect("sandbox namespace exists");
        std::fs::write(sandbox.configured_path().join("capture.log"), b"escaped")
            .expect("stray namespace entry");

        store
            .remove_sandbox(sandbox_id)
            .expect("destroy must reclaim a stray namespace entry");
        assert!(
            optional_child_directory(&catalog, &sandbox_id.to_string())
                .expect("probe sandbox namespace")
                .is_none(),
            "the sandbox namespace should be gone"
        );
        let _ = id;
    }

    #[test]
    fn remove_sandbox_reclaims_malformed_scratch_like_entries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        publish(&store, sandbox_id, None, true);
        let catalog = store.root().expect("catalog");
        let sandbox = optional_child_directory(&catalog, &sandbox_id.to_string())
            .expect("open sandbox")
            .expect("sandbox namespace exists");

        std::fs::create_dir(sandbox.configured_path().join(".ckpt-invalid.tmp"))
            .expect("malformed checkpoint scratch directory");
        std::fs::write(
            sandbox.configured_path().join(".HEAD.invalid.tmp"),
            b"stale",
        )
        .expect("malformed HEAD scratch file");

        store
            .remove_sandbox(sandbox_id)
            .expect("destroy must classify malformed scratch entries by filesystem type");
        assert!(
            optional_child_directory(&catalog, &sandbox_id.to_string())
                .expect("probe sandbox namespace")
                .is_none(),
            "the sandbox namespace should be gone"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn remove_sandbox_reclaims_non_utf8_namespace_entries() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        publish(&store, sandbox_id, None, true);
        let catalog = store.root().expect("catalog");
        let sandbox = optional_child_directory(&catalog, &sandbox_id.to_string())
            .expect("open sandbox")
            .expect("sandbox namespace exists");

        let stray_file = OsStr::from_bytes(b"capture-\xff.log");
        let stray_directory = OsStr::from_bytes(b".ckpt-\xfe.tmp");
        std::fs::write(sandbox.configured_path().join(stray_file), b"escaped")
            .expect("non-UTF-8 namespace file");
        let stray_directory_path = sandbox.configured_path().join(stray_directory);
        std::fs::create_dir(&stray_directory_path).expect("non-UTF-8 namespace directory");
        std::fs::write(stray_directory_path.join("artifact"), b"escaped")
            .expect("nested escaped artifact");

        store
            .remove_sandbox(sandbox_id)
            .expect("destroy must reclaim non-UTF-8 namespace entries");
        assert!(
            optional_child_directory(&catalog, &sandbox_id.to_string())
                .expect("probe sandbox namespace")
                .is_none(),
            "the sandbox namespace should be gone"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_payload_with_a_symlink_stays_removable() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        populate(&stage, "symlinked");
        let outside = temp.path().join("outside");
        fs::write(&outside, b"outside").expect("outside file");
        symlink(&outside, stage.backend_payload_dir().join("escape"))
            .expect("plant payload symlink");

        let error = store
            .publish(&stage, commit_input(None))
            .expect_err("a payload containing a symlink must not publish");
        assert_eq!(error.outcome(), CheckpointPublishOutcome::KnownUnpublished);

        store
            .abort(stage)
            .expect("a rejected payload must remain removable");
        assert!(store.list(sandbox_id).expect("list").is_empty());
        assert!(outside.is_file(), "the symlink target must not be followed");
    }

    #[test]
    fn a_payload_too_deep_to_publish_stays_removable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        populate(&stage, "too-deep");
        // Nest one level past the payload bound: publication must reject the
        // stage, and the compensation path must still be able to remove it.
        let mut deep = stage.backend_payload_dir();
        for _ in 0..MAX_PAYLOAD_DEPTH {
            deep.push("d");
        }
        fs::create_dir_all(&deep).expect("deep payload tree");
        fs::write(deep.join("stranded.bin"), b"too deep").expect("deep artifact");

        let error = store
            .publish(&stage, commit_input(None))
            .expect_err("a payload past the depth bound must not publish");
        assert_eq!(error.outcome(), CheckpointPublishOutcome::KnownUnpublished);

        store
            .abort(stage)
            .expect("a rejected payload must remain removable");
        assert!(store.list(sandbox_id).expect("list").is_empty());
    }

    #[test]
    fn verify_rejects_unregistered_payload_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let checkpoint_id = publish(&store, sandbox_id, None, true);
        let committed = store
            .configured_root()
            .join(sandbox_id.to_string())
            .join(&checkpoint_id);

        fs::write(committed.join("backend/unregistered.bin"), b"smuggled")
            .expect("write unregistered payload file");

        let error = store
            .verify(sandbox_id, &checkpoint_id)
            .expect_err("files outside the manifest must fail closed");
        assert!(error.to_string().contains("does not match its manifest"));
    }

    #[test]
    fn nested_payload_directories_publish_verify_and_pin() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        let checkpoint_id = stage.id().to_string();
        // A container-shaped payload: an image directory plus the spec
        // beside it, exactly what a runsc-style backend writes.
        let image = stage.backend_payload_dir().join("image");
        let bundle = stage.backend_payload_dir().join("bundle");
        fs::create_dir_all(&image).expect("image directory");
        fs::create_dir_all(&bundle).expect("bundle directory");
        fs::write(image.join("checkpoint.img"), b"sentry state").expect("image state");
        fs::write(image.join("pages.bin"), b"guest pages").expect("image pages");
        fs::write(bundle.join("config.json"), b"{}").expect("spec copy");
        fs::write(
            stage.storage_payload_dir().join("rootfs.snap"),
            b"rootfs capture",
        )
        .expect("rootfs artifact");

        let metadata = store
            .publish(&stage, commit_input(None))
            .expect("publish nested payload");
        let names: Vec<&str> = metadata
            .artifacts
            .iter()
            .map(|artifact| artifact.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "backend/bundle/config.json",
                "backend/image/checkpoint.img",
                "backend/image/pages.bin",
                "storage/rootfs.snap"
            ]
        );
        store.set_head(sandbox_id, &checkpoint_id).expect("HEAD");
        store
            .verify(sandbox_id, &checkpoint_id)
            .expect("verify nested payload");

        let target = store
            .verify_restore_target(sandbox_id, &checkpoint_id)
            .expect("restore target");
        assert!(
            target
                .backend_payload_dir()
                .join("image/pages.bin")
                .is_file(),
            "the pinned backend subtree must expose the nested layout"
        );
        assert!(
            target.storage_payload_dir().join("rootfs.snap").is_file(),
            "the pinned storage subtree must expose the rootfs capture"
        );
    }

    #[test]
    fn v1_checkpoints_stay_verifiable_and_map_payloads_to_the_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        // Force catalog creation, then hand-write a pre-split checkpoint the
        // way version 1 committed it: flat artifacts beside metadata.json.
        let _ = publish(&store, sandbox_id, None, false);
        let checkpoint_id = format!("ckpt-{}", Uuid::new_v4());
        let directory = store
            .configured_root()
            .join(sandbox_id.to_string())
            .join(&checkpoint_id);
        fs::create_dir(&directory).expect("v1 checkpoint directory");
        let contents: [(&str, &[u8]); 3] = [
            ("vmstate.snap", b"vm"),
            ("memory.snap", b"mem"),
            ("rootfs.snap", b"root"),
        ];
        let mut artifacts = Vec::new();
        for (name, bytes) in contents {
            fs::write(directory.join(name), bytes).expect("v1 artifact");
            artifacts.push(CheckpointArtifact {
                name: name.to_string(),
                size_bytes: bytes.len() as u64,
                sha256: format!("{:x}", Sha256::digest(bytes)),
            });
        }
        let metadata = CheckpointMetadata {
            format_version: CHECKPOINT_FORMAT_V1,
            id: checkpoint_id.clone(),
            parent: None,
            sandbox_id,
            policy_name: "default".to_string(),
            image_digest: "sha256:test".to_string(),
            backend: BackendKind::Mock,
            backend_version: Some("mock-v1".to_string()),
            created_at: Utc::now(),
            snapshot_kind: SnapshotKind::Full,
            artifacts,
        };
        fs::write(
            directory.join(METADATA_FILE),
            serde_json::to_vec_pretty(&metadata).expect("encode v1 metadata"),
        )
        .expect("write v1 metadata");

        let verified = store
            .verify(sandbox_id, &checkpoint_id)
            .expect("v1 checkpoints stay readable");
        assert_eq!(verified.format_version, CHECKPOINT_FORMAT_V1);

        // Both producer subtrees map onto the checkpoint root, so a backend
        // that wrote flat files before the split finds them unchanged.
        let target = store
            .verify_restore_target(sandbox_id, &checkpoint_id)
            .expect("v1 restore target");
        assert!(target.backend_payload_dir().join("vmstate.snap").is_file());
        assert!(target.backend_payload_dir().join("memory.snap").is_file());
        assert!(target.storage_payload_dir().join("rootfs.snap").is_file());
    }

    #[test]
    fn publish_verify_and_list_preserve_the_head_boundary() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let root = publish(&store, sandbox_id, None, true);
        let unreachable = publish(&store, sandbox_id, Some(root.clone()), false);

        assert_eq!(store.read_head(sandbox_id).expect("HEAD"), Some(root));
        store
            .verify(sandbox_id, &unreachable)
            .expect("published checkpoint");
        let listed = store.list(sandbox_id).expect("list checkpoints");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed.iter().filter(|info| info.is_head).count(), 1);
        assert!(
            listed
                .iter()
                .any(|info| info.id == unreachable && !info.on_head_chain)
        );
    }

    #[test]
    fn prune_preserves_head_lineage_and_removes_nested_payloads() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let root = publish(&store, sandbox_id, None, true);
        let head = publish(&store, sandbox_id, Some(root.clone()), true);
        let stage = store.begin(sandbox_id).expect("begin nested checkpoint");
        let unreachable = stage.id().to_string();
        populate(&stage, "nested-unreachable");
        let nested = stage.backend_payload_dir().join("image/layer");
        fs::create_dir_all(&nested).expect("nested backend payload");
        fs::write(nested.join("pages.bin"), b"nested-pages").expect("nested payload file");
        store
            .publish(&stage, commit_input(Some(root.clone())))
            .expect("publish nested checkpoint");

        let outcome = store
            .prune_unreachable(sandbox_id, true)
            .expect("prune unreachable branch");
        match outcome {
            PruneOutcome::Complete { removed } => {
                assert_eq!(removed, vec![unreachable.clone()]);
            }
            other => panic!("expected complete prune, got {other:?}"),
        }

        let sandbox = store.configured_root().join(sandbox_id.to_string());
        assert!(sandbox.join(&root).is_dir());
        assert!(sandbox.join(&head).is_dir());
        assert!(!sandbox.join(&unreachable).exists());
        assert!(
            fs::read_dir(&sandbox)
                .expect("scan checkpoint namespace")
                .all(|entry| !entry
                    .expect("checkpoint entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(PRUNE_TOMBSTONE_PREFIX)),
            "successful prune must leave no tombstone"
        );
    }

    #[test]
    fn prune_distinguishes_no_history_from_a_vanished_namespace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();

        let outcome = store
            .prune_unreachable(sandbox_id, false)
            .expect("a sandbox without checkpoint history has nothing to prune");
        assert!(matches!(
            outcome,
            PruneOutcome::Complete { removed } if removed.is_empty()
        ));

        let error = store
            .prune_unreachable(sandbox_id, true)
            .expect_err("a missing namespace must not hide recorded checkpoint history");
        assert!(
            error
                .to_string()
                .contains("checkpoint namespace is missing")
        );
    }

    #[test]
    fn prune_rejects_unknown_namespace_entries_before_deletion() {
        for entry_is_directory in [false, true] {
            let temp = tempfile::tempdir().expect("tempdir");
            let store = store(&temp);
            let sandbox_id = Uuid::new_v4();
            let root = publish(&store, sandbox_id, None, true);
            let unreachable = publish(&store, sandbox_id, Some(root.clone()), false);
            let sandbox = store.configured_root().join(sandbox_id.to_string());
            let stray = sandbox.join(if entry_is_directory {
                "unknown-directory"
            } else {
                "unknown-file"
            });
            if entry_is_directory {
                fs::create_dir(&stray).expect("create unknown directory");
            } else {
                fs::write(&stray, b"unexpected").expect("create unknown file");
            }

            let error = store
                .prune_unreachable(sandbox_id, true)
                .expect_err("an unknown namespace entry must stop prune before deletion");

            assert!(
                error
                    .to_string()
                    .contains("is neither HEAD nor a committed checkpoint")
            );
            assert_eq!(
                store.read_head(sandbox_id).expect("HEAD after rejection"),
                Some(root.clone())
            );
            assert!(sandbox.join(&root).is_dir());
            assert!(sandbox.join(&unreachable).is_dir());
            assert!(stray.exists());
            assert!(
                fs::read_dir(&sandbox)
                    .expect("scan checkpoint namespace")
                    .all(|entry| !entry
                        .expect("checkpoint entry")
                        .file_name()
                        .to_string_lossy()
                        .starts_with(PRUNE_TOMBSTONE_PREFIX))
            );
        }
    }

    #[test]
    fn prune_verifies_every_checkpoint_digest_before_deletion() {
        for corrupt_unreachable in [false, true] {
            let temp = tempfile::tempdir().expect("tempdir");
            let store = store(&temp);
            let sandbox_id = Uuid::new_v4();
            let head = publish(&store, sandbox_id, None, true);
            let unreachable = publish(&store, sandbox_id, Some(head.clone()), false);
            let sandbox = store.configured_root().join(sandbox_id.to_string());
            let corrupt_id = if corrupt_unreachable {
                &unreachable
            } else {
                &head
            };
            let artifact = sandbox.join(corrupt_id).join("backend/memory.snap");
            let mut bytes = fs::read(&artifact).expect("read artifact before corruption");
            bytes[0] ^= 1;
            fs::write(&artifact, bytes).expect("replace artifact without changing its size");

            let error = store
                .prune_unreachable(sandbox_id, true)
                .expect_err("any corrupted checkpoint must stop prune before deletion");

            assert!(error.to_string().contains("failed integrity validation"));
            assert_eq!(
                store
                    .read_head_id(sandbox_id)
                    .expect("HEAD identifier after rejection"),
                Some(head.clone())
            );
            assert!(sandbox.join(&head).is_dir());
            assert!(sandbox.join(&unreachable).is_dir());
            assert!(
                fs::read_dir(&sandbox)
                    .expect("scan checkpoint namespace")
                    .all(|entry| !entry
                        .expect("checkpoint entry")
                        .file_name()
                        .to_string_lossy()
                        .starts_with(PRUNE_TOMBSTONE_PREFIX))
            );
        }
    }

    #[test]
    fn prune_rejects_an_unreachable_branch_with_a_missing_parent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let root = publish(&store, sandbox_id, None, true);
        let head = publish(&store, sandbox_id, Some(root.clone()), true);
        let unreachable = publish(&store, sandbox_id, Some(root.clone()), false);
        let sandbox = store.configured_root().join(sandbox_id.to_string());
        let metadata_path = sandbox.join(&unreachable).join(METADATA_FILE);
        let mut metadata: CheckpointMetadata =
            serde_json::from_slice(&fs::read(&metadata_path).expect("read metadata"))
                .expect("decode metadata");
        metadata.parent = Some(format!("ckpt-{}", Uuid::new_v4()));
        fs::write(
            &metadata_path,
            serde_json::to_vec(&metadata).expect("encode metadata with missing parent"),
        )
        .expect("write metadata with missing parent");

        let error = store
            .prune_unreachable(sandbox_id, true)
            .expect_err("a missing parent must stop prune before deletion");

        assert!(
            error
                .to_string()
                .contains("checkpoint lineage references missing parent")
        );
        assert_eq!(
            store.read_head(sandbox_id).expect("HEAD after rejection"),
            Some(head.clone())
        );
        for checkpoint_id in [&root, &head, &unreachable] {
            assert!(sandbox.join(checkpoint_id).is_dir());
        }
        assert!(
            fs::read_dir(&sandbox)
                .expect("scan checkpoint namespace")
                .all(|entry| !entry
                    .expect("checkpoint entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(PRUNE_TOMBSTONE_PREFIX))
        );
    }

    #[test]
    fn prune_rejects_a_cycle_outside_the_head_lineage() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let root = publish(&store, sandbox_id, None, true);
        let head = publish(&store, sandbox_id, Some(root.clone()), true);
        let first = publish(&store, sandbox_id, Some(root.clone()), false);
        let second = publish(&store, sandbox_id, Some(first.clone()), false);
        let sandbox = store.configured_root().join(sandbox_id.to_string());
        let metadata_path = sandbox.join(&first).join(METADATA_FILE);
        let mut metadata: CheckpointMetadata =
            serde_json::from_slice(&fs::read(&metadata_path).expect("read metadata"))
                .expect("decode metadata");
        metadata.parent = Some(second.clone());
        fs::write(
            &metadata_path,
            serde_json::to_vec(&metadata).expect("encode cyclic metadata"),
        )
        .expect("write cyclic metadata");

        let error = store
            .prune_unreachable(sandbox_id, true)
            .expect_err("an unreachable cycle must stop prune before deletion");

        assert!(
            error
                .to_string()
                .contains("checkpoint parent cycle reaches")
        );
        assert_eq!(
            store.read_head(sandbox_id).expect("HEAD after rejection"),
            Some(head.clone())
        );
        for checkpoint_id in [&root, &head, &first, &second] {
            assert!(sandbox.join(checkpoint_id).is_dir());
        }
        assert!(
            fs::read_dir(&sandbox)
                .expect("scan checkpoint namespace")
                .all(|entry| !entry
                    .expect("checkpoint entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(PRUNE_TOMBSTONE_PREFIX))
        );
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn prune_reports_incomplete_cleanup_after_publishing_a_tombstone() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let root = publish(&store, sandbox_id, None, true);
        let unreachable = publish(&store, sandbox_id, Some(root), false);
        let hook = crate::failpoint::TestFailpoint::new(&["checkpoint-prune-after-tombstone"]);

        let outcome = hook
            .run(async { store.prune_unreachable(sandbox_id, true) })
            .await
            .expect("post-rename failure must retain its outcome");
        match outcome {
            PruneOutcome::Incomplete {
                removed,
                uncertain,
                source,
            } => {
                assert_eq!(removed, vec![unreachable.clone()]);
                assert!(uncertain.is_none());
                assert!(!source.to_string().is_empty());
            }
            other => panic!("expected incomplete prune, got {other:?}"),
        }

        let sandbox = store.configured_root().join(sandbox_id.to_string());
        assert!(!sandbox.join(&unreachable).exists());
        assert!(
            fs::read_dir(&sandbox)
                .expect("scan checkpoint namespace")
                .any(|entry| entry
                    .expect("checkpoint entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(PRUNE_TOMBSTONE_PREFIX)),
            "interrupted cleanup must retain a recognised tombstone"
        );
    }

    #[test]
    fn checkpoint_tree_uses_owner_only_permissions() {
        if std::env::var_os("BLAZE_CHECKPOINT_MODE_CHILD").is_none() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        populate(&stage, "private");
        let root = store.configured_root();
        let sandbox = root.join(sandbox_id.to_string());
        let staging = sandbox.join(&stage.staging_name);

        assert_mode(&root, 0o700);
        assert_mode(&sandbox, 0o700);
        assert_mode(&staging, 0o700);
        assert_mode(&staging.join(PAYLOAD_BACKEND_DIR), 0o700);
        assert_mode(&staging.join(PAYLOAD_STORAGE_DIR), 0o700);

        for (subtree, name) in TEST_ARTIFACTS {
            fs::set_permissions(
                staging.join(subtree).join(name),
                fs::Permissions::from_mode(0o666),
            )
            .expect("make backend artifact permissive");
        }
        let checkpoint_id = stage.id().to_string();
        store
            .publish(&stage, commit_input(None))
            .expect("publish checkpoint");
        store
            .set_head(sandbox_id, &checkpoint_id)
            .expect("set HEAD");

        let committed = sandbox.join(&checkpoint_id);
        assert_mode(&committed, 0o700);
        for (subtree, name) in TEST_ARTIFACTS {
            assert_mode(&committed.join(subtree).join(name), 0o600);
        }
        assert_mode(&committed.join(METADATA_FILE), 0o600);
        assert_mode(&sandbox.join(HEAD_FILE), 0o600);
    }

    #[test]
    fn publish_rejects_multiply_linked_artifacts_without_changing_permissions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        populate(&stage, "hard-linked");
        let vmstate = stage.backend_payload_dir().join("vmstate.snap");
        let rootfs = stage.storage_payload_dir().join("rootfs.snap");
        let external = temp.path().join("external-rootfs");
        fs::set_permissions(&vmstate, fs::Permissions::from_mode(0o666))
            .expect("make preceding artifact permissive");
        fs::set_permissions(&rootfs, fs::Permissions::from_mode(0o640))
            .expect("set observable mode");
        fs::hard_link(&rootfs, &external).expect("link artifact outside checkpoint tree");

        let error = store
            .publish(&stage, commit_input(None))
            .expect_err("multiply linked artifact must fail closed");

        assert!(error.to_string().contains("exactly one hard link"));
        assert_eq!(
            fs::metadata(&vmstate).expect("VM state metadata").mode() & 0o777,
            0o666,
            "validation must finish before any artifact permissions change"
        );
        assert_eq!(
            fs::metadata(&external).expect("external metadata").mode() & 0o777,
            0o640
        );
        assert!(store.list(sandbox_id).expect("list").is_empty());
    }

    #[test]
    fn sandbox_removal_accepts_an_interrupted_internal_rootfs_link() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        let rootfs = stage.storage_payload_dir().join("rootfs.snap");
        let temporary = stage
            .directory
            .configured_path()
            .join(PAYLOAD_STORAGE_DIR)
            .join(".rootfs.snap.capture-interrupted.tmp");
        fs::write(&temporary, b"partial rootfs").expect("write temporary rootfs");
        fs::hard_link(&temporary, &rootfs).expect("link captured rootfs");

        store.remove_sandbox(sandbox_id).expect("remove sandbox");

        assert!(!stage.directory.configured_path().exists());
    }

    #[test]
    fn owner_only_modes_ignore_a_permissive_umask() {
        let temp = tempfile::tempdir().expect("tempdir");
        let script = "umask 000; \"$1\" --exact checkpoint_store::tests::checkpoint_tree_uses_owner_only_permissions --nocapture";
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .arg("sh")
            .arg(std::env::current_exe().expect("test binary"))
            .env("BLAZE_CHECKPOINT_MODE_CHILD", "1")
            .env("TMPDIR", temp.path())
            .output()
            .expect("run child test with a permissive umask");
        assert!(
            output.status.success(),
            "child test failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn assert_mode(path: &Path, expected: u32) {
        assert_eq!(
            fs::symlink_metadata(path)
                .unwrap_or_else(|error| panic!("inspect {}: {error}", path.display()))
                .mode()
                & 0o777,
            expected,
            "unexpected permissions for {}",
            path.display()
        );
    }

    #[test]
    fn list_uses_committed_metadata_without_rehashing_artifacts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let root = publish(&store, sandbox_id, None, true);
        let head = publish(&store, sandbox_id, Some(root.clone()), true);
        let expected = store.list(sandbox_id).expect("list intact checkpoints");
        let sandbox = store.configured_root().join(sandbox_id.to_string());

        fs::write(
            sandbox.join(&root).join("storage/rootfs.snap"),
            b"corrupted root",
        )
        .expect("corrupt historical checkpoint artifact");
        fs::write(
            sandbox.join(&head).join("backend/memory.snap"),
            b"corrupted head",
        )
        .expect("corrupt HEAD checkpoint artifact");

        assert_eq!(
            store.list(sandbox_id).expect("list from metadata"),
            expected
        );
        let verify_error = store
            .verify(sandbox_id, &root)
            .expect_err("verification must hash historical artifacts");
        assert!(
            verify_error
                .to_string()
                .contains("failed integrity validation")
        );
        let set_head_error = store
            .set_head(sandbox_id, &root)
            .expect_err("setting HEAD must hash the target artifacts");
        assert_eq!(
            set_head_error.outcome(),
            CheckpointHeadOutcome::KnownUnchanged
        );
        assert!(
            set_head_error
                .to_string()
                .contains("failed integrity validation")
        );
        assert!(
            store.read_head(sandbox_id).is_err(),
            "reading HEAD must retain full artifact verification"
        );
        assert_eq!(
            store
                .read_head_id(sandbox_id)
                .expect("observing the recorded HEAD must not hash artifacts"),
            Some(head),
            "an unreadable artifact must not hide which checkpoint HEAD names"
        );
    }

    #[test]
    fn publish_does_not_rehash_non_head_ancestor_artifacts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let root = publish(&store, sandbox_id, None, true);
        let head = publish(&store, sandbox_id, Some(root.clone()), true);
        let sandbox = store.configured_root().join(sandbox_id.to_string());

        fs::write(
            sandbox.join(&root).join("storage/rootfs.snap"),
            b"corrupted root",
        )
        .expect("corrupt non-HEAD ancestor artifact");

        assert_eq!(
            store.read_head(sandbox_id).expect("read intact HEAD"),
            Some(head.clone())
        );
        let next = publish(&store, sandbox_id, Some(head), true);
        assert_eq!(
            store
                .read_head(sandbox_id)
                .expect("read newly published HEAD"),
            Some(next)
        );
        let verify_error = store
            .verify(sandbox_id, &root)
            .expect_err("explicit verification must hash ancestor artifacts");
        assert!(
            verify_error
                .to_string()
                .contains("failed integrity validation")
        );
    }

    #[test]
    fn publish_metadata_only_lineage_validation_rejects_a_missing_parent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let missing_parent = format!("ckpt-{}", Uuid::new_v4());
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        populate(&stage, "missing-parent");

        let error = store
            .publish(&stage, commit_input(Some(missing_parent)))
            .expect_err("missing parent must prevent publication");

        assert_eq!(error.outcome(), CheckpointPublishOutcome::KnownUnpublished);
        assert!(
            error
                .to_string()
                .contains("open committed checkpoint directory")
        );
    }

    #[test]
    fn publish_metadata_only_lineage_validation_rejects_a_parent_cycle() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let root = publish(&store, sandbox_id, None, true);
        let head = publish(&store, sandbox_id, Some(root.clone()), true);
        let sandbox = store.configured_root().join(sandbox_id.to_string());
        let root_metadata_path = sandbox.join(&root).join(METADATA_FILE);
        let mut root_metadata: CheckpointMetadata =
            serde_json::from_slice(&fs::read(&root_metadata_path).expect("read root metadata"))
                .expect("decode root metadata");
        root_metadata.parent = Some(head.clone());
        fs::write(
            &root_metadata_path,
            serde_json::to_vec(&root_metadata).expect("encode cyclic root metadata"),
        )
        .expect("write cyclic root metadata");
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        populate(&stage, "parent-cycle");

        let error = store
            .publish(&stage, commit_input(Some(head)))
            .expect_err("parent cycle must prevent publication");

        assert_eq!(error.outcome(), CheckpointPublishOutcome::KnownUnpublished);
        assert!(
            error
                .to_string()
                .contains("checkpoint parent cycle reaches")
        );
    }

    #[test]
    fn sandbox_removal_clears_scratch_and_committed_history() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let committed = publish(&store, sandbox_id, None, true);
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        let sandbox_dir = store.configured_root().join(sandbox_id.to_string());
        let stage_path = sandbox_dir.join(&stage.staging_name);
        let temporary_head = sandbox_dir.join(format!(".HEAD.{}{STAGING_SUFFIX}", Uuid::new_v4()));
        fs::write(&temporary_head, b"temporary").expect("write temporary HEAD");

        store.remove_sandbox(sandbox_id).expect("remove sandbox");

        assert!(!stage_path.exists());
        assert!(!temporary_head.exists());
        assert!(!sandbox_dir.join(committed).exists());
        assert_eq!(store.read_head(sandbox_id).expect("HEAD"), None);
        assert!(!sandbox_dir.exists());
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn sandbox_parent_sync_is_retried_for_an_existing_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let hook = crate::failpoint::TestFailpoint::new(&["checkpoint-store-sandbox-parent-sync"]);

        let first_error = hook
            .run(async { store.begin(sandbox_id) })
            .await
            .expect_err("initial parent sync must fail");
        assert!(
            first_error
                .to_string()
                .contains("checkpoint-store-sandbox-parent-sync")
        );
        assert!(
            store
                .configured_root()
                .join(sandbox_id.to_string())
                .is_dir(),
            "the failed parent sync leaves the newly created directory"
        );

        let retry_error = hook
            .run(async { store.begin(sandbox_id) })
            .await
            .expect_err("retry must synchronize the catalog again");
        assert!(
            retry_error
                .to_string()
                .contains("checkpoint-store-sandbox-parent-sync")
        );

        let stage = store.begin(sandbox_id).expect("unarmed retry");
        store.abort(stage).expect("discard retry stage");
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn stage_parent_sync_failure_removes_the_owned_stage() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let hook = crate::failpoint::TestFailpoint::new(&["checkpoint-store-stage-parent-sync"]);

        let error = hook
            .run(async { store.begin(sandbox_id) })
            .await
            .expect_err("stage parent sync must fail");
        assert!(
            error
                .to_string()
                .contains("checkpoint-store-stage-parent-sync")
        );

        let sandbox = store.configured_root().join(sandbox_id.to_string());
        assert!(
            fs::read_dir(&sandbox)
                .expect("checkpoint sandbox")
                .next()
                .is_none(),
            "failed stage creation must not leave scratch entries"
        );

        let stage = store.begin(sandbox_id).expect("unarmed retry");
        store.abort(stage).expect("discard retry stage");
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn sandbox_removal_retries_parent_sync_when_the_namespace_is_absent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        store.abort(stage).expect("discard stage");
        let sandbox = store.configured_root().join(sandbox_id.to_string());
        let hook =
            crate::failpoint::TestFailpoint::new(&["checkpoint-store-sandbox-remove-parent-sync"]);

        let first_error = hook
            .run(async { store.remove_sandbox(sandbox_id) })
            .await
            .expect_err("initial parent sync must fail");
        assert!(
            first_error
                .to_string()
                .contains("checkpoint-store-sandbox-remove-parent-sync")
        );
        assert!(!sandbox.exists(), "the namespace was already unlinked");

        let retry_error = hook
            .run(async { store.remove_sandbox(sandbox_id) })
            .await
            .expect_err("retry must synchronize the catalog again");
        assert!(
            retry_error
                .to_string()
                .contains("checkpoint-store-sandbox-remove-parent-sync")
        );

        store
            .remove_sandbox(sandbox_id)
            .expect("unarmed retry synchronizes the catalog");
    }

    #[test]
    fn state_root_replacement_does_not_redirect_catalog_creation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let configured_state = temp.path().join("state");
        let retained_state = temp.path().join("retained-state");
        fs::rename(&configured_state, &retained_state).expect("move retained state root");
        fs::create_dir(&configured_state).expect("replacement state root");

        let sandbox_id = Uuid::new_v4();
        let stage = store
            .begin(sandbox_id)
            .expect("begin through retained root");
        populate(&stage, "retained-state");

        assert!(
            retained_state
                .join("checkpoints")
                .join(sandbox_id.to_string())
                .is_dir()
        );
        assert!(!configured_state.join("checkpoints").exists());
    }

    #[test]
    fn catalog_replacement_does_not_redirect_later_operations() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let first = Uuid::new_v4();
        let first_stage = store.begin(first).expect("open catalog");
        store.abort(first_stage).expect("discard first stage");
        let configured_catalog = temp.path().join("state/checkpoints");
        let retained_catalog = temp.path().join("retained-checkpoints");
        fs::rename(&configured_catalog, &retained_catalog).expect("move retained catalog");
        fs::create_dir(&configured_catalog).expect("replacement catalog");

        let second = Uuid::new_v4();
        let stage = store.begin(second).expect("begin through retained catalog");
        populate(&stage, "retained-catalog");

        assert!(retained_catalog.join(second.to_string()).is_dir());
        assert!(!configured_catalog.join(second.to_string()).exists());
    }

    #[test]
    fn sandbox_replacement_is_detected_before_publication() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        populate(&stage, "sandbox-owner");
        let configured = store.configured_root().join(sandbox_id.to_string());
        let retained = store.configured_root().join("retained-sandbox");
        fs::rename(&configured, &retained).expect("move retained sandbox");
        fs::create_dir(&configured).expect("replacement sandbox");

        let error = store
            .publish(&stage, commit_input(None))
            .expect_err("sandbox replacement must fail closed");

        assert!(error.to_string().contains("changed identity"));
        assert!(!configured.join(stage.id()).exists());
        assert!(retained.join(&stage.staging_name).is_dir());
    }

    #[test]
    fn stage_replacement_is_detected_before_publication() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        populate(&stage, "stage-owner");
        let sandbox = store.configured_root().join(sandbox_id.to_string());
        let configured_stage = sandbox.join(&stage.staging_name);
        let retained_stage = sandbox.join("retained-stage");
        fs::rename(&configured_stage, &retained_stage).expect("move retained stage");
        fs::create_dir(&configured_stage).expect("replacement stage");
        fs::write(configured_stage.join("sentinel"), b"replacement")
            .expect("write replacement sentinel");

        let error = store
            .publish(&stage, commit_input(None))
            .expect_err("stage replacement must fail closed");

        assert_eq!(error.outcome(), CheckpointPublishOutcome::KnownUnpublished);
        assert!(error.to_string().contains("changed identity"));
        assert!(!sandbox.join(stage.id()).exists());
        let error = store
            .abort(stage)
            .expect_err("replacement must prevent retained-stage cleanup");
        assert!(error.to_string().contains("changed identity"));
        assert_eq!(
            fs::read(configured_stage.join("sentinel")).expect("read replacement sentinel"),
            b"replacement"
        );
        assert!(retained_stage.is_dir());
    }

    #[test]
    fn artifact_replacement_during_publication_is_detected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        populate(&stage, "artifact-owner");
        let rootfs = store
            .configured_root()
            .join(sandbox_id.to_string())
            .join(&stage.staging_name)
            .join("storage/rootfs.snap");
        let retained = temp.path().join("retained-rootfs.snap");
        store.set_before_publish_revalidation(move || {
            fs::rename(&rootfs, &retained).expect("move retained artifact");
            fs::write(&rootfs, b"replacement").expect("replacement artifact");
        });

        let error = store
            .publish(&stage, commit_input(None))
            .expect_err("artifact replacement must fail closed");

        assert!(error.to_string().contains("changed identity"));
        assert!(store.list(sandbox_id).expect("list").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn verify_rejects_artifact_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let checkpoint_id = publish(&store, sandbox_id, None, true);
        let artifact = store
            .configured_root()
            .join(sandbox_id.to_string())
            .join(&checkpoint_id)
            .join("storage/rootfs.snap");
        fs::remove_file(&artifact).expect("remove artifact");
        let outside = temp.path().join("outside");
        fs::write(&outside, b"outside").expect("write outside file");
        symlink(&outside, &artifact).expect("link artifact");

        assert!(store.verify(sandbox_id, &checkpoint_id).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn begin_rejects_a_symlinked_catalog_root() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let actual = temp.path().join("actual");
        fs::create_dir(&state).expect("state root");
        fs::create_dir(&actual).expect("actual root");
        symlink(&actual, state.join("checkpoints")).expect("link root");
        let store = CheckpointStore::new(StateStore::new(state));

        assert!(store.begin(Uuid::new_v4()).is_err());
    }

    #[cfg(not(feature = "test-failpoints"))]
    #[test]
    fn production_checkpoint_store_boundary_hooks_are_inert() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let root = publish(&store, sandbox_id, None, true);

        assert_eq!(store.read_head(sandbox_id).expect("HEAD"), Some(root));
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn publish_boundary_error_leaves_a_committed_unreachable_checkpoint() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        let checkpoint_id = stage.id().to_string();
        populate(&stage, "publish-boundary");
        let hook = crate::failpoint::TestFailpoint::new(&["checkpoint-store-publish-after-rename"]);

        let error = hook
            .run(async { store.publish(&stage, commit_input(None)) })
            .await
            .expect_err("publish boundary must return a store error");

        assert!(
            error
                .to_string()
                .contains("checkpoint-store-publish-after-rename")
        );
        assert_eq!(error.outcome(), CheckpointPublishOutcome::Unknown);
        store
            .verify(sandbox_id, &checkpoint_id)
            .expect("renamed checkpoint remains committed");
        assert_eq!(store.read_head(sandbox_id).expect("HEAD"), None);
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn publish_pre_rename_error_reports_a_known_unpublished_stage() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        let checkpoint_id = stage.id().to_string();
        let staging_name = stage.staging_name.clone();
        populate(&stage, "pre-rename-boundary");
        let hook =
            crate::failpoint::TestFailpoint::new(&["checkpoint-store-publish-before-rename"]);

        let error = hook
            .run(async { store.publish(&stage, commit_input(None)) })
            .await
            .expect_err("pre-rename boundary must return a store error");

        assert_eq!(error.outcome(), CheckpointPublishOutcome::KnownUnpublished);
        assert!(
            error
                .to_string()
                .contains("checkpoint-store-publish-before-rename")
        );
        let sandbox = store.configured_root().join(sandbox_id.to_string());
        assert!(sandbox.join(&staging_name).is_dir());
        assert!(sandbox.join(&staging_name).join(METADATA_FILE).is_file());
        assert!(!sandbox.join(checkpoint_id).exists());
        store
            .abort(stage)
            .expect("abort retained unpublished stage");
        assert!(!sandbox.join(staging_name).exists());
    }

    #[test]
    fn publish_rename_error_reports_an_unknown_outcome() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        let checkpoint_id = stage.id().to_string();
        let staging_name = stage.staging_name.clone();
        populate(&stage, "rename-collision");
        let sandbox = store.configured_root().join(sandbox_id.to_string());
        let target = sandbox.join(&checkpoint_id);
        let collision = target.clone();
        store.set_before_publish_revalidation(move || {
            fs::create_dir(&collision).expect("create publication collision");
            fs::write(collision.join("sentinel"), b"collision").expect("write collision sentinel");
        });

        let error = store
            .publish(&stage, commit_input(None))
            .expect_err("rename collision must fail publication");

        assert_eq!(error.outcome(), CheckpointPublishOutcome::Unknown);
        assert!(sandbox.join(staging_name).is_dir());
        assert_eq!(
            fs::read(target.join("sentinel")).expect("read collision sentinel"),
            b"collision"
        );
    }

    #[test]
    fn published_witness_sets_head_without_full_verification() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        let checkpoint_id = stage.id().to_string();
        populate(&stage, "retained-publication");

        let published = store
            .publish_retained(&stage, commit_input(None))
            .expect("publish checkpoint with retained owners");
        assert_eq!(store.verified_checkpoint_count(), 0);

        let metadata = store
            .set_head_published(published)
            .expect("advance HEAD from published witness");
        assert_eq!(metadata.id, checkpoint_id);
        assert_eq!(
            store.verified_checkpoint_count(),
            0,
            "the publication witness must avoid a second payload scan"
        );
        let head = store
            .configured_root()
            .join(sandbox_id.to_string())
            .join(HEAD_FILE);
        assert_eq!(
            fs::read_to_string(head).expect("read HEAD").trim(),
            checkpoint_id
        );

        store
            .set_head(sandbox_id, &checkpoint_id)
            .expect("public HEAD update performs full verification");
        assert_eq!(store.verified_checkpoint_count(), 1);
    }

    #[test]
    fn published_witness_rejects_replaced_artifact_identity() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let existing_head = publish(&store, sandbox_id, None, true);
        let stage = store.begin(sandbox_id).expect("begin checkpoint");
        let checkpoint_id = stage.id().to_string();
        populate(&stage, "replaced-after-publication");
        let published = store
            .publish_retained(&stage, commit_input(Some(existing_head.clone())))
            .expect("publish checkpoint with retained owners");

        let sandbox = store.configured_root().join(sandbox_id.to_string());
        let artifact = sandbox.join(&checkpoint_id).join("storage/rootfs.snap");
        let displaced = sandbox.join("displaced-rootfs.snap");
        let bytes = fs::read(&artifact).expect("read published rootfs");
        fs::rename(&artifact, &displaced).expect("move retained rootfs");
        fs::write(&artifact, &bytes).expect("write same-content replacement");

        let error = store
            .set_head_published(published)
            .expect_err("replacement must invalidate the publication witness");
        assert_eq!(error.outcome(), CheckpointHeadOutcome::KnownUnchanged);
        assert!(error.to_string().contains("changed identity"));
        assert_eq!(
            fs::read_to_string(sandbox.join(HEAD_FILE))
                .expect("read unchanged HEAD")
                .trim(),
            existing_head
        );
        assert_eq!(fs::read(&artifact).expect("read replacement rootfs"), bytes);
        assert!(displaced.is_file());
        assert!(
            fs::read_dir(&sandbox)
                .expect("checkpoint sandbox")
                .filter_map(std::result::Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().starts_with(".HEAD.")),
            "identity rejection must not leave temporary HEAD state"
        );
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn head_pre_rename_error_removes_scratch_and_reports_known_unchanged() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let existing_head = publish(&store, sandbox_id, None, true);
        let checkpoint_id = publish(&store, sandbox_id, Some(existing_head.clone()), false);
        let hook = crate::failpoint::TestFailpoint::new(&["checkpoint-store-head-before-rename"]);

        let error = hook
            .run(async { store.set_head(sandbox_id, &checkpoint_id) })
            .await
            .expect_err("pre-rename HEAD boundary must fail");

        assert_eq!(error.outcome(), CheckpointHeadOutcome::KnownUnchanged);
        assert!(
            error
                .to_string()
                .contains("checkpoint-store-head-before-rename")
        );
        assert_eq!(
            store.read_head(sandbox_id).expect("HEAD"),
            Some(existing_head.clone())
        );
        let checkpoints = store.list(sandbox_id).expect("checkpoint catalog");
        assert_eq!(checkpoints.len(), 2);
        assert!(
            checkpoints
                .iter()
                .any(|checkpoint| checkpoint.id == existing_head && checkpoint.is_head)
        );
        assert!(
            checkpoints
                .iter()
                .any(|checkpoint| checkpoint.id == checkpoint_id && !checkpoint.is_head)
        );
        let sandbox = store.configured_root().join(sandbox_id.to_string());
        assert!(
            fs::read_dir(sandbox)
                .expect("checkpoint sandbox")
                .filter_map(std::result::Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().starts_with(".HEAD.")),
            "known-unchanged failure must remove its temporary HEAD"
        );
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn head_pre_rename_cleanup_error_reports_an_unknown_outcome() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let checkpoint_id = publish(&store, sandbox_id, None, false);
        let hook = crate::failpoint::TestFailpoint::new(&[
            "checkpoint-store-head-before-rename",
            "checkpoint-store-head-cleanup",
        ]);

        let error = hook
            .run(async { store.set_head(sandbox_id, &checkpoint_id) })
            .await
            .expect_err("failed pre-rename cleanup must be uncertain");

        assert_eq!(error.outcome(), CheckpointHeadOutcome::Unknown);
        assert!(error.to_string().contains("temporary HEAD cleanup failed"));
        assert_eq!(store.read_head(sandbox_id).expect("HEAD"), None);
        let sandbox = store.configured_root().join(sandbox_id.to_string());
        assert_eq!(
            fs::read_dir(&sandbox)
                .expect("checkpoint sandbox")
                .filter_map(std::result::Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with(".HEAD."))
                .count(),
            1,
            "failed cleanup must remain observable for recovery"
        );
        store.remove_sandbox(sandbox_id).expect("remove sandbox");
        assert!(!sandbox.exists());
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn head_boundary_error_leaves_the_new_head_visible() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = store(&temp);
        let sandbox_id = Uuid::new_v4();
        let checkpoint_id = publish(&store, sandbox_id, None, false);
        let hook = crate::failpoint::TestFailpoint::new(&["checkpoint-store-head-after-rename"]);

        let error = hook
            .run(async { store.set_head(sandbox_id, &checkpoint_id) })
            .await
            .expect_err("HEAD boundary must return a store error");

        assert_eq!(error.outcome(), CheckpointHeadOutcome::Unknown);
        assert!(
            error
                .to_string()
                .contains("checkpoint-store-head-after-rename")
        );
        assert_eq!(
            store.read_head(sandbox_id).expect("HEAD"),
            Some(checkpoint_id)
        );
    }
}

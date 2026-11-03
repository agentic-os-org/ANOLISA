//! Re-resolvable identity manifests for static file-protection topology.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::sync::Arc;

use super::file_protection_mountinfo::{MountInfo, read_mountinfo};
use super::file_protection_resolver::{FileIdentity, HeldPath, open_held, open_held_directory};
use super::read_process_start_time;
use crate::BackendError;

#[derive(Clone, Debug)]
pub(super) struct TopologySnapshot {
    pub(super) namespace: (u64, u64),
    pub(super) start_time: u64,
    pub(super) mountinfo_digest: u64,
    pub(super) mounts: Vec<MountInfo>,
}

impl TopologySnapshot {
    pub(super) fn validate_process(
        &self,
        pid: i32,
        expected_start: u64,
    ) -> Result<(), BackendError> {
        if self.start_time == expected_start {
            Ok(())
        } else {
            Err(BackendError::StaleProcess { pid })
        }
    }

    pub(super) fn same_topology(&self, other: &Self) -> bool {
        self.namespace == other.namespace
            && self.mountinfo_digest == other.mountinfo_digest
            && self.mounts == other.mounts
    }

    pub(super) fn change_message(&self, other: &Self) -> String {
        format!(
            "target mount topology changed (namespace {:?}->{:?}, digest {}->{})",
            self.namespace, other.namespace, self.mountinfo_digest, other.mountinfo_digest
        )
    }
}

pub(super) trait TopologySource: Send + Sync {
    fn open_root(&self) -> io::Result<File>;
    fn snapshot(&self) -> Result<TopologySnapshot, BackendError>;
}

#[derive(Debug)]
pub(super) struct ProcTopologySource {
    pid: i32,
}

impl ProcTopologySource {
    pub(super) fn new(pid: i32) -> Self {
        Self { pid }
    }
}

impl TopologySource for ProcTopologySource {
    fn open_root(&self) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_CLOEXEC | libc::O_DIRECTORY)
            .open(format!("/proc/{}/root", self.pid))
    }

    fn snapshot(&self) -> Result<TopologySnapshot, BackendError> {
        let namespace = fs::metadata(format!("/proc/{}/ns/mnt", self.pid))
            .map(|metadata| (metadata.dev(), metadata.ino()))
            .map_err(|error| kernel(format!("read target mount namespace: {error}")))?;
        let start_time = read_process_start_time(self.pid)?;
        let (mounts, mountinfo_digest) = read_mountinfo(self.pid)?;
        Ok(TopologySnapshot {
            namespace,
            start_time,
            mountinfo_digest,
            mounts,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ManifestExpectation {
    Existing {
        identity: FileIdentity,
        follow_final: bool,
    },
    MissingFinal {
        parent_path: String,
        parent_identity: FileIdentity,
    },
}

#[derive(Clone, Debug, Default)]
pub(super) struct IdentityManifest {
    paths: BTreeMap<(String, bool), ManifestExpectation>,
}

impl IdentityManifest {
    pub(super) fn add_existing(&mut self, path: &HeldPath) -> Result<(), BackendError> {
        self.add_existing_with_mode(path, false)
    }

    pub(super) fn add_directory(&mut self, path: &HeldPath) -> Result<(), BackendError> {
        self.add_existing_with_mode(path, true)
    }

    fn add_existing_with_mode(
        &mut self,
        path: &HeldPath,
        follow_final: bool,
    ) -> Result<(), BackendError> {
        self.insert(
            path.authored_path.clone(),
            ManifestExpectation::Existing {
                identity: path.identity,
                follow_final,
            },
        )
    }

    pub(super) fn add_missing_final(
        &mut self,
        path: &str,
        parent: &HeldPath,
    ) -> Result<(), BackendError> {
        self.insert(
            path.into(),
            ManifestExpectation::MissingFinal {
                parent_path: parent.authored_path.clone(),
                parent_identity: parent.identity,
            },
        )
    }

    fn insert(
        &mut self,
        path: String,
        expectation: ManifestExpectation,
    ) -> Result<(), BackendError> {
        let follow_final = matches!(
            expectation,
            ManifestExpectation::Existing {
                follow_final: true,
                ..
            }
        );
        let key = (path, follow_final);
        if let Some(existing) = self.paths.get(&key) {
            return if existing == &expectation {
                Ok(())
            } else {
                Err(BackendError::CompileFailure(format!(
                    "conflicting topology identity requirements for {:?}",
                    key.0
                )))
            };
        }
        self.paths.insert(key, expectation);
        Ok(())
    }

    pub(super) fn validate(&self, root: &File) -> Result<(), BackendError> {
        for ((path, _), expectation) in &self.paths {
            match expectation {
                ManifestExpectation::Existing {
                    identity: expected,
                    follow_final,
                } => {
                    let current = if *follow_final {
                        open_held_directory(root, path)
                    } else {
                        open_held(root, path)
                    }
                    .map_err(|error| changed(format!("re-resolve {path:?}: {error}")))?;
                    if current.identity != *expected {
                        return Err(changed(format!(
                            "authored path identity changed for {path:?}: expected {expected:?}, found {:?}",
                            current.identity
                        )));
                    }
                }
                ManifestExpectation::MissingFinal {
                    parent_path,
                    parent_identity,
                } => {
                    let parent = open_held_directory(root, parent_path).map_err(|error| {
                        changed(format!(
                            "re-resolve future exclusion parent {parent_path:?}: {error}"
                        ))
                    })?;
                    if parent.identity != *parent_identity {
                        return Err(changed(format!(
                            "future exclusion parent identity changed for {parent_path:?}: expected {parent_identity:?}, found {:?}",
                            parent.identity
                        )));
                    }
                    match open_held(root, path) {
                        Err(error) if error.is_missing_final() => {}
                        Ok(_) => {
                            return Err(changed(format!(
                                "future exclusion final component {path:?} now exists"
                            )));
                        }
                        Err(error) => {
                            return Err(changed(format!(
                                "future exclusion final component {path:?} is no longer ENOENT: {error}"
                            )));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(super) struct TopologyValidation {
    pid: i32,
    expected_start: u64,
    expected: TopologySnapshot,
    manifest: IdentityManifest,
    source: Arc<dyn TopologySource>,
}

impl fmt::Debug for TopologyValidation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TopologyValidation")
            .field("pid", &self.pid)
            .field("expected_start", &self.expected_start)
            .field("expected", &self.expected)
            .field("manifest", &self.manifest)
            .finish_non_exhaustive()
    }
}

impl TopologyValidation {
    pub(super) fn new(
        pid: i32,
        expected_start: u64,
        expected: TopologySnapshot,
        manifest: IdentityManifest,
        source: Arc<dyn TopologySource>,
    ) -> Self {
        Self {
            pid,
            expected_start,
            expected,
            manifest,
            source,
        }
    }

    pub(super) fn validate(&self) -> Result<(), BackendError> {
        let before = self.source.snapshot()?;
        self.validate_snapshot(&before)?;
        let root = open_fresh_root(self.source.as_ref(), self.pid)?;
        self.manifest.validate(&root)?;
        let after = self.source.snapshot()?;
        self.validate_snapshot(&after)
    }

    fn validate_snapshot(&self, snapshot: &TopologySnapshot) -> Result<(), BackendError> {
        snapshot.validate_process(self.pid, self.expected_start)?;
        if self.expected.same_topology(snapshot) {
            Ok(())
        } else {
            Err(changed(self.expected.change_message(snapshot)))
        }
    }
}

pub(super) fn open_fresh_root(source: &dyn TopologySource, pid: i32) -> Result<File, BackendError> {
    source
        .open_root()
        .map_err(|error| kernel(format!("open /proc/{pid}/root with O_PATH: {error}")))
}

fn changed(message: impl Into<String>) -> BackendError {
    BackendError::KernelFailure(format!(
        "file-protection topology validation failed: {}",
        message.into()
    ))
}

fn kernel(message: impl Into<String>) -> BackendError {
    BackendError::KernelFailure(message.into())
}

#[cfg(test)]
#[path = "file_protection_manifest_tests.rs"]
pub(super) mod tests;

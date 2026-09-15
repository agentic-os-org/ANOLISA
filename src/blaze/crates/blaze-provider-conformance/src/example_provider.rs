// SPDX-License-Identifier: Apache-2.0
//! Small file-backed provider used by the executable composition examples.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use blaze_core::storage::StorageSlot;
use blaze_provider_api::{
    AbortRequest, AbortResult, CommitRequest, CommittedLease, DataPlaneProvider, FinalizeRequest,
    FinalizedLease, InspectRequest, LeaseBinding, LeaseState, ObservedLease,
    PROVIDER_CONTRACT_VERSION, PrepareRequest, PrepareSource, PreparedLease, PreparedResources,
    ProviderCapabilities, ProviderDescriptor, ProviderError, ReleaseRequest, ReleaseResult,
    StopRequest, StoppedLease,
};
use uuid::Uuid;

#[derive(Clone)]
struct Lease {
    binding: LeaseBinding,
    storage: StorageSlot,
}

/// Minimal file-backed provider for examples and contract exercises.
///
/// This implementation supports ordinary image preparation only. It creates
/// sparse files under an isolated root and deliberately omits persistence,
/// concurrency control across processes, templates, restart reconciliation,
/// checkpoints, suspension, and reusable capacity. It is not a production
/// storage implementation.
pub struct ExampleFileProvider {
    descriptor: ProviderDescriptor,
    root: PathBuf,
    leases: Mutex<HashMap<Uuid, Lease>>,
}

impl ExampleFileProvider {
    /// Create an example provider rooted at one absolute directory.
    pub fn new(root: PathBuf) -> Self {
        Self {
            descriptor: ProviderDescriptor {
                contract_version: PROVIDER_CONTRACT_VERSION,
                provider_instance_id: Uuid::new_v4(),
            },
            root,
            leases: Mutex::new(HashMap::new()),
        }
    }

    fn current(&self, binding: LeaseBinding) -> Result<Lease, ProviderError> {
        let leases = self
            .leases
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?;
        let lease = leases
            .get(&binding.context.lease_id)
            .ok_or(ProviderError::Conflict)?;
        if lease.binding != binding {
            return Err(ProviderError::Conflict);
        }
        Ok(lease.clone())
    }

    fn advance(
        &self,
        binding: LeaseBinding,
        expected: LeaseState,
        next: LeaseState,
    ) -> Result<LeaseBinding, ProviderError> {
        let mut leases = self
            .leases
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?;
        let lease = leases
            .get_mut(&binding.context.lease_id)
            .ok_or(ProviderError::Conflict)?;
        if lease.binding != binding || binding.state != expected {
            return Err(ProviderError::Conflict);
        }
        lease.binding.generation = binding
            .generation
            .checked_add(1)
            .ok_or(ProviderError::OutcomeUnknown)?;
        lease.binding.state = next;
        Ok(lease.binding)
    }

    async fn remove(
        &self,
        binding: LeaseBinding,
        expected: &[LeaseState],
    ) -> Result<LeaseBinding, ProviderError> {
        let lease = self.current(binding)?;
        if !expected.contains(&binding.state) {
            return Err(ProviderError::Conflict);
        }
        tokio::fs::remove_dir_all(&lease.storage.instance_dir)
            .await
            .map_err(|_| ProviderError::OutcomeUnknown)?;
        self.leases
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?
            .remove(&binding.context.lease_id)
            .ok_or(ProviderError::OutcomeUnknown)?;
        Ok(LeaseBinding {
            generation: binding
                .generation
                .checked_add(1)
                .ok_or(ProviderError::OutcomeUnknown)?,
            state: LeaseState::Released,
            ..binding
        })
    }
}

#[async_trait]
impl DataPlaneProvider for ExampleFileProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        self.descriptor
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            images: true,
            ..ProviderCapabilities::default()
        }
    }

    async fn probe(&self) -> Result<(), ProviderError> {
        if self.root.is_absolute() && self.root.is_dir() {
            Ok(())
        } else {
            Err(ProviderError::Unavailable)
        }
    }

    async fn prepare(&self, request: PrepareRequest) -> Result<PreparedLease, ProviderError> {
        if !matches!(request.source, PrepareSource::Image { .. })
            || request.root_filesystem_bytes == 0
            || request.guest_memory_bytes == 0
        {
            return Err(ProviderError::Unsupported);
        }
        let instance_dir = self.root.join(request.context.instance_id.to_string());
        tokio::fs::create_dir(&instance_dir)
            .await
            .map_err(|_| ProviderError::Conflict)?;
        let storage = StorageSlot {
            id: request.context.instance_id.to_string(),
            rootfs_path: instance_dir.join("rootfs.img"),
            mem_path: instance_dir.join("memory.bin"),
            mem_diff_path: instance_dir.join("memory.diff"),
            rootfs_diff_path: instance_dir.join("rootfs.diff"),
            instance_dir,
        };
        for (path, size) in [
            (&storage.rootfs_path, request.root_filesystem_bytes),
            (&storage.mem_path, request.guest_memory_bytes),
            (&storage.mem_diff_path, 0),
            (&storage.rootfs_diff_path, 0),
        ] {
            let file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(path)
                .map_err(|_| ProviderError::OutcomeUnknown)?;
            file.set_len(size)
                .map_err(|_| ProviderError::OutcomeUnknown)?;
        }
        let binding = LeaseBinding {
            provider_instance_id: self.descriptor.provider_instance_id,
            context: request.context,
            generation: request.context.generation,
            state: LeaseState::Prepared,
        };
        let lease = Lease {
            binding,
            storage: storage.clone(),
        };
        self.leases
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?
            .insert(request.context.lease_id, lease);
        Ok(PreparedLease {
            binding,
            resources: PreparedResources::PathBacked {
                storage,
                restore_payload_dir: None,
            },
        })
    }

    async fn inspect(&self, request: InspectRequest) -> Result<ObservedLease, ProviderError> {
        let leases = self
            .leases
            .lock()
            .map_err(|_| ProviderError::OutcomeUnknown)?;
        let lease = leases
            .get(&request.context.lease_id)
            .ok_or(ProviderError::NotFound)?;
        if lease.binding.context != request.context {
            return Err(ProviderError::Conflict);
        }
        Ok(ObservedLease {
            binding: lease.binding,
        })
    }

    async fn commit(&self, request: CommitRequest) -> Result<CommittedLease, ProviderError> {
        Ok(CommittedLease {
            binding: self.advance(request.binding, LeaseState::Prepared, LeaseState::Committed)?,
        })
    }

    async fn finalize(&self, request: FinalizeRequest) -> Result<FinalizedLease, ProviderError> {
        if request.public_transition.instance_id != request.binding.context.instance_id
            || request.public_transition.operation_id != request.binding.context.operation_id
        {
            return Err(ProviderError::Conflict);
        }
        Ok(FinalizedLease {
            binding: self.advance(
                request.binding,
                LeaseState::Committed,
                LeaseState::Finalized,
            )?,
        })
    }

    async fn abort(&self, request: AbortRequest) -> Result<AbortResult, ProviderError> {
        Ok(AbortResult {
            binding: self
                .remove(
                    request.binding,
                    &[LeaseState::Prepared, LeaseState::Committed],
                )
                .await?,
        })
    }

    async fn stop(&self, request: StopRequest) -> Result<StoppedLease, ProviderError> {
        Ok(StoppedLease {
            binding: self.advance(request.binding, LeaseState::Finalized, LeaseState::Stopped)?,
        })
    }

    async fn release(&self, request: ReleaseRequest) -> Result<ReleaseResult, ProviderError> {
        Ok(ReleaseResult {
            binding: self.remove(request.binding, &[LeaseState::Stopped]).await?,
        })
    }
}

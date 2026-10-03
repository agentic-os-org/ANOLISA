//! Authenticated local control plane for durable Gateway Tasks.
//!
//! The Unix transport derives authority from kernel peer credentials and
//! delegates every mutation to the single-writer [`TaskCoordinator`].
//!
//! Runtime scheduling is isolated in the sibling `scheduler` module so the
//! private transport remains independent from provider-specific adapters.

mod checkpoint;
mod handler;
mod scheduler;
mod scheduler_attachment;

pub use checkpoint::{
    ApprovalCheckpointCreateResult, ApprovalCheckpointEvidence, ApprovalCheckpointPrepareResult,
    ApprovalCheckpointReconcileResult, ApprovalCheckpointRecord, ApprovalCheckpointRequest,
    ApprovalCheckpointState, PreRuntimeBaselineState, PreRuntimeBaselineView,
    PreRuntimeCheckpointBinding, PreRuntimeCheckpointCreateResult, PreRuntimeCheckpointDriver,
    PreRuntimeCheckpointEvidence, PreRuntimeCheckpointReconcileRequest,
    PreRuntimeCheckpointReconcileResult, PreRuntimeCheckpointRequest, TaskSnapshotChange,
    TaskSnapshotDriver, TaskSnapshotKind, TaskSnapshotProviderPreview, TaskSnapshotProviderRequest,
    TaskSnapshotProviderSwitch, TaskSnapshotProviderSwitchResult, TaskSnapshotView,
};
pub use scheduler::{
    BrokeredApprovalContext, BrokeredApprovalPlan, BrokeredExecutionDriver,
    BrokeredRecoveryContext, BrokeredResolution, BrokeredResolutionContext,
    BrokeredResolutionSource, RuntimeCancellationCause, RuntimeFactory, RuntimeHandle, RuntimePoll,
    ScheduledRun, SchedulerTick, StartedRuntime, TaskScheduler, TaskSchedulerConfig,
};

use std::fs::{self, FileType, Metadata};
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cosh_gateway_contracts::capability::ApprovalDecision;
use cosh_gateway_contracts::common::{
    ActorKind, ActorRef, AuthAssurance, BoundedName, BoundedOpaque, ContractHeader, ContractSchema,
    Correlation, Digest, IdempotencyKey, RuntimeSelector, TargetRef, WorkspaceRef,
};
use cosh_gateway_contracts::ids::{
    ActorId, ApprovalId, CheckpointId, DeliveryId, InputRequestId, InstallationId, MessageId,
    RequestId, RunId, TaskId,
};
use cosh_gateway_contracts::profile::GatewayCapabilityProfile;
use cosh_gateway_contracts::runtime::RuntimeInputResponse;
use cosh_gateway_contracts::task::{
    ApprovalPolicy, CancelReason, CancellationStage, CheckpointPolicy, TaskEvent,
    TaskEventEnvelope, TaskLaunchDescriptorV1, TaskLaunchSpecV1, TaskRuntime, TaskState,
    TASK_LAUNCH_SPEC_V1,
};
use nix::sys::socket::getsockopt;
use nix::unistd::Uid;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::runtime::VerifiedRuntimeContainment;
use crate::storage::{CommitOutcome, OutboxIntent, SqliteTaskStore, StoreError, TaskCommit};
use crate::task::TaskAggregate;

use handler::{TaskAdmission, TaskCommandPort, TaskProjectionPort};

/// Local Gateway API version, independent from ACP wire versions.
pub const GATEWAY_API_VERSION: &str = "cosh.gateway.v1";
/// Maximum bytes in one length-prefixed request or response.
pub const MAX_GATEWAY_FRAME_BYTES: usize = 1024 * 1024;
// A same-UID client may occupy the serial handler for at most one scheduler
// admission quantum. This remains far below the shortest supported Run lease.
const CONNECTION_ADMISSION_QUANTUM: Duration = Duration::from_millis(250);
const CLIENT_REQUEST_DEADLINE: Duration = Duration::from_secs(5);
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(25);

// These cohesive fragments share one private transport namespace; making them
// sibling modules would widen internal authority helpers solely for layout.
include!("daemon/protocol.rs");
include!("daemon/coordinator.rs");
include!("daemon/server.rs");
include!("daemon/client.rs");
include!("daemon/support.rs");

#[cfg(test)]
mod tests;

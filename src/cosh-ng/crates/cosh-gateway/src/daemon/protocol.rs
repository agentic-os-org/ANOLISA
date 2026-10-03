include!("protocol/launch.rs");

/// Validated fields used to create and queue one Task.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitTask {
    /// Correlates one transport request and response.
    pub request_id: RequestId,
    /// Caller-stable replay key within the authenticated actor namespace.
    pub idempotency_key: IdempotencyKey,
    /// Bounded user intent; Task history retains its digest while the private
    /// runtime-start Outbox retains the delivery payload.
    pub intent: cosh_gateway_contracts::common::BoundedText,
    /// Governed environment selected for the Task.
    pub target: TargetRef,
    /// Runtime selected for the first queued Run.
    pub runtime: RuntimeSelector,
}

/// Validated fields used to launch one catalog-selected durable Task.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitLaunch {
    /// Correlates one transport request and response.
    pub request_id: RequestId,
    /// Caller-stable replay key within the authenticated actor namespace.
    pub idempotency_key: IdempotencyKey,
    /// Strict versioned Task launch data.
    pub launch: TaskLaunchSpecV1,
}

/// Validated fields used to request Task cancellation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelTask {
    /// Correlates one transport request and response.
    pub request_id: RequestId,
    /// Caller-stable replay key within the authenticated actor namespace.
    pub idempotency_key: IdempotencyKey,
    /// Task owning the active Run.
    pub task_id: TaskId,
    /// Active Run whose cancellation is requested.
    pub run_id: RunId,
    /// Optional optimistic Task revision.
    pub expected_revision: Option<u64>,
}

/// Validated fields used to queue a replacement for one suspended Run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryTask {
    /// Correlates one transport request and response.
    pub request_id: RequestId,
    /// Caller-stable replay key within the authenticated actor namespace.
    pub idempotency_key: IdempotencyKey,
    /// Task owning the suspended Run.
    pub task_id: TaskId,
    /// Exact active attempt from which immutable start intent is recovered.
    pub previous_run_id: RunId,
    /// Optional optimistic Task revision.
    pub expected_revision: Option<u64>,
}

/// Validated fields used to append one exact pending Runtime input response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppendTaskInput {
    /// Correlates one transport request and response.
    pub request_id: RequestId,
    /// Caller-stable replay key within the authenticated actor namespace.
    pub idempotency_key: IdempotencyKey,
    /// Task owning the pending Runtime question.
    pub task_id: TaskId,
    /// Exact durable Runtime input request being resolved.
    pub input_request_id: InputRequestId,
    /// Typed bounded response stored only in the private dispatch ledger.
    pub response: RuntimeInputResponse,
    /// Optional optimistic Task revision.
    pub expected_revision: Option<u64>,
}

/// Validated fields used to resolve a provider-native approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveApproval {
    /// Correlates one transport request and response.
    pub request_id: RequestId,
    /// Caller-stable replay key within the authenticated actor namespace.
    pub idempotency_key: IdempotencyKey,
    /// Durable approval awaiting this decision.
    pub approval_id: ApprovalId,
    /// Human decision dispatched once to the bound provider callback.
    pub decision: ApprovalDecision,
}

/// Approval resolution whose Task binding is checked before mutation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveApprovalForTask {
    /// Correlates one transport request and response.
    pub request_id: RequestId,
    /// Caller-stable replay key within the authenticated actor namespace.
    pub idempotency_key: IdempotencyKey,
    /// Task that must own the approval.
    pub task_id: TaskId,
    /// Durable approval awaiting this decision.
    pub approval_id: ApprovalId,
    /// Human decision dispatched once to the bound operation.
    pub decision: ApprovalDecision,
}

/// Exact Task-owned snapshot selected for a read-only operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InspectTaskSnapshot {
    /// Owning managed Task.
    pub task_id: TaskId,
    /// Complete checkpoint identity; prefixes are rejected by parsing.
    pub snapshot_id: CheckpointId,
}

/// Recovery-protected switch to one exact Task-owned snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwitchTaskSnapshot {
    /// Correlates one transport request and response.
    pub request_id: RequestId,
    /// Caller-stable replay key within the authenticated actor namespace.
    pub idempotency_key: IdempotencyKey,
    /// Owning managed Task.
    pub task_id: TaskId,
    /// Complete Task-owned target checkpoint.
    pub snapshot_id: CheckpointId,
    /// Preview digest displayed during the caller's confirmation step.
    pub preview_digest: Digest,
    /// Rejects a switch if the Task projection changed after preview.
    pub expected_revision: u64,
}

/// Task-scoped list of proven-created checkpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSnapshotList {
    /// Owning managed Task.
    pub task_id: TaskId,
    /// Current Task lifecycle state.
    pub state: TaskState,
    /// Current Task revision.
    pub revision: u64,
    /// Canonical admitted workspace.
    pub workspace: WorkspaceRef,
    /// Checkpoints in durable creation order.
    pub snapshots: Vec<TaskSnapshotView>,
}

/// Read-only preview of one exact Task-owned checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSnapshotPreview {
    /// Owning managed Task.
    pub task_id: TaskId,
    /// Current Task lifecycle state.
    pub state: TaskState,
    /// Current Task revision.
    pub revision: u64,
    /// Canonical admitted workspace.
    pub workspace: WorkspaceRef,
    /// Exact target checkpoint.
    pub snapshot_id: CheckpointId,
    /// Ordered prefix of provider changes, bounded for the local transport.
    pub changes: Vec<TaskSnapshotChange>,
    /// Number of provider changes omitted from this response.
    #[serde(default)]
    pub changes_omitted: usize,
    /// Digest over the complete provider preview that must be confirmed before switching.
    pub preview_digest: Digest,
}

/// Durable result of one recovery-protected Task snapshot switch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSnapshotSwitchView {
    /// Owning managed Task.
    pub task_id: TaskId,
    /// Exact selected target.
    pub snapshot_id: CheckpointId,
    /// Recovery point created immediately before the switch.
    pub recovery_snapshot_id: CheckpointId,
    /// Provider head replaced by the switch.
    pub from: BoundedOpaque,
    /// Exact provider target returned after the switch.
    pub to: CheckpointId,
}

/// Safe Task projection returned to an authorized local client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskView {
    /// Durable Task identity.
    pub task_id: TaskId,
    /// Latest event revision.
    pub revision: u64,
    /// Current durable lifecycle state.
    pub state: TaskState,
    /// Current Run when one has been allocated.
    pub active_run_id: Option<RunId>,
    /// Immutable governed target.
    pub target: TargetRef,
    /// Safe launch choices for Tasks submitted through the launch API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<TaskLaunchDescriptorV1>,
    /// Honest pre-Runtime baseline state, when checkpointing was requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<PreRuntimeBaselineView>,
}

impl From<&TaskAggregate> for TaskView {
    fn from(task: &TaskAggregate) -> Self {
        Self {
            task_id: task.task_id().clone(),
            revision: task.revision(),
            state: task.state(),
            active_run_id: task.active_run_id().cloned(),
            target: task.target().clone(),
            launch: None,
            baseline: None,
        }
    }
}

/// Bounded page of immutable Task events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEventPage {
    /// Task owning the stream.
    pub task_id: TaskId,
    /// Events ordered by increasing revision.
    pub events: Vec<TaskEventEnvelope>,
    /// Last revision in this page, or the supplied cursor for an empty page.
    pub next_revision: u64,
    /// Whether a later revision exists in the current projection.
    pub has_more: bool,
}

/// Bounded newest-first page of Tasks owned by the authenticated local actor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskListPage {
    /// Authorized Task projections ordered by durable update time, then ID.
    pub tasks: Vec<TaskView>,
}

/// Successful local Gateway response payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", content = "data", rename_all = "snake_case")]
pub enum GatewayResult {
    /// Daemon accepted an authenticated ping.
    Pong,
    /// Safe Runtime, workspace, checkpoint, and authority capabilities.
    Capabilities(GatewayCapabilities),
    /// Current authorized Task projection.
    Task(TaskView),
    /// Bounded authorized Task projections.
    Tasks(TaskListPage),
    /// Bounded immutable event page.
    Events(TaskEventPage),
    /// Projection after a cancellation commit or replay.
    Cancelled(TaskView),
    /// Projection after a provider-native approval resolution.
    ApprovalResolved(TaskView),
    /// Projection after a retry was queued or replayed.
    Retried(TaskView),
    /// Projection after an input response was durably appended and dispatched.
    InputAppended(TaskView),
    /// Proven-created checkpoints owned by one authorized Task.
    TaskSnapshots(TaskSnapshotList),
    /// Read-only preview or diff of one Task-owned checkpoint.
    TaskSnapshotPreview(TaskSnapshotPreview),
    /// Recovery-protected Task snapshot switch result.
    TaskSnapshotSwitched(TaskSnapshotSwitchView),
}

/// Local daemon or client failure.
#[derive(Debug, Error)]
pub enum GatewayDaemonError {
    /// A configured socket or state path is unsafe.
    #[error("unsafe Gateway path {path}: {message}")]
    UnsafePath {
        /// Rejected path.
        path: PathBuf,
        /// Bounded reason.
        message: String,
    },
    /// Another daemon owns the configured socket.
    #[error("a Gateway daemon is already listening at {0}")]
    AlreadyRunning(PathBuf),
    /// Kernel peer credentials do not authorize this local client.
    #[error("local Gateway peer is not authorized")]
    Unauthorized,
    /// The local framing or API contract is invalid.
    #[error("invalid Gateway protocol: {0}")]
    Protocol(String),
    /// A remote daemon returned a stable domain failure.
    #[error("Gateway request failed [{code}]: {message}")]
    Remote {
        /// Stable machine-readable error code.
        code: String,
        /// Bounded diagnostic safe for the local client.
        message: String,
        /// Whether refreshing state and retrying may succeed.
        recoverable: bool,
    },
    /// Local I/O failed.
    #[error("Gateway I/O failed: {0}")]
    Io(#[from] io::Error),
    /// Durable Task storage failed.
    #[error("Gateway storage failed: {0}")]
    Store(#[from] StoreError),
    /// JSON encoding or decoding failed.
    #[error("Gateway serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum GatewayRequest {
    Ping {
        api_version: String,
        request_id: RequestId,
    },
    Submit {
        api_version: String,
        #[serde(flatten)]
        request: SubmitTask,
    },
    SubmitLaunch {
        api_version: String,
        #[serde(flatten)]
        request: SubmitLaunch,
    },
    Capabilities {
        api_version: String,
        request_id: RequestId,
    },
    Get {
        api_version: String,
        request_id: RequestId,
        task_id: TaskId,
    },
    List {
        api_version: String,
        request_id: RequestId,
        limit: u16,
    },
    Events {
        api_version: String,
        request_id: RequestId,
        task_id: TaskId,
        after_revision: Option<u64>,
        limit: u16,
    },
    Cancel {
        api_version: String,
        #[serde(flatten)]
        request: CancelTask,
    },
    ResolveApproval {
        api_version: String,
        #[serde(flatten)]
        request: ResolveApproval,
    },
    ResolveApprovalForTask {
        api_version: String,
        #[serde(flatten)]
        request: ResolveApprovalForTask,
    },
    Retry {
        api_version: String,
        #[serde(flatten)]
        request: RetryTask,
    },
    AppendInput {
        api_version: String,
        #[serde(flatten)]
        request: AppendTaskInput,
    },
    ListTaskSnapshots {
        api_version: String,
        request_id: RequestId,
        task_id: TaskId,
    },
    PreviewTaskSnapshot {
        api_version: String,
        request_id: RequestId,
        #[serde(flatten)]
        request: InspectTaskSnapshot,
    },
    DiffTaskSnapshot {
        api_version: String,
        request_id: RequestId,
        #[serde(flatten)]
        request: InspectTaskSnapshot,
    },
    SwitchTaskSnapshot {
        api_version: String,
        #[serde(flatten)]
        request: SwitchTaskSnapshot,
    },
}

impl GatewayRequest {
    fn request_id(&self) -> &RequestId {
        match self {
            Self::Ping { request_id, .. }
            | Self::Capabilities { request_id, .. }
            | Self::Get { request_id, .. }
            | Self::List { request_id, .. }
            | Self::Events { request_id, .. } => request_id,
            Self::Submit { request, .. } => &request.request_id,
            Self::SubmitLaunch { request, .. } => &request.request_id,
            Self::Cancel { request, .. } => &request.request_id,
            Self::Retry { request, .. } => &request.request_id,
            Self::ResolveApproval { request, .. } => &request.request_id,
            Self::ResolveApprovalForTask { request, .. } => &request.request_id,
            Self::AppendInput { request, .. } => &request.request_id,
            Self::ListTaskSnapshots { request_id, .. }
            | Self::PreviewTaskSnapshot { request_id, .. }
            | Self::DiffTaskSnapshot { request_id, .. } => request_id,
            Self::SwitchTaskSnapshot { request, .. } => &request.request_id,
        }
    }

    fn api_version(&self) -> &str {
        match self {
            Self::Ping { api_version, .. }
            | Self::Submit { api_version, .. }
            | Self::SubmitLaunch { api_version, .. }
            | Self::Capabilities { api_version, .. }
            | Self::Get { api_version, .. }
            | Self::List { api_version, .. }
            | Self::Events { api_version, .. }
            | Self::Cancel { api_version, .. }
            | Self::Retry { api_version, .. }
            | Self::AppendInput { api_version, .. }
            | Self::ResolveApproval { api_version, .. } => api_version,
            Self::ResolveApprovalForTask { api_version, .. }
            | Self::ListTaskSnapshots { api_version, .. }
            | Self::PreviewTaskSnapshot { api_version, .. }
            | Self::DiffTaskSnapshot { api_version, .. }
            | Self::SwitchTaskSnapshot { api_version, .. } => api_version,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct GatewayResponse {
    api_version: String,
    request_id: Option<RequestId>,
    #[serde(flatten)]
    outcome: GatewayResponseOutcome,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum GatewayResponseOutcome {
    Ok { result: Box<GatewayResult> },
    Error { error: GatewayErrorBody },
}

#[derive(Debug, Serialize, Deserialize)]
struct GatewayErrorBody {
    code: String,
    message: String,
    recoverable: bool,
}

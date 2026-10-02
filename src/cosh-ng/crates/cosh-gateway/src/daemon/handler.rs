//! Transport-neutral Task request admission and dispatch.

use cosh_gateway_contracts::common::{ActorRef, WorkspaceRef};
use cosh_gateway_contracts::ids::{ActorId, TaskId};
use cosh_gateway_contracts::profile::GatewayCapabilityProfile;

use super::{
    AppendTaskInput, CancelTask, GatewayDaemonError, GatewayRequest, GatewayResult,
    ResolveApproval, ResolveApprovalForTask, RetryTask, SubmitLaunch, SubmitTask,
    SwitchTaskSnapshot, TaskEventPage, TaskLaunchCatalog, TaskListPage, TaskSnapshotList,
    TaskSnapshotPreview, TaskSnapshotSwitchView, TaskView, GATEWAY_API_VERSION,
};

/// Mutating Task operations available to the transport handler.
pub(super) trait TaskCommandPort {
    fn submit(
        &mut self,
        actor: &ActorRef,
        workspace: &WorkspaceRef,
        request: SubmitTask,
    ) -> Result<TaskView, GatewayDaemonError>;

    fn submit_launch(
        &mut self,
        actor: &ActorRef,
        catalog: &TaskLaunchCatalog,
        request: SubmitLaunch,
    ) -> Result<TaskView, GatewayDaemonError>;

    fn cancel(
        &mut self,
        actor_id: &ActorId,
        request: CancelTask,
    ) -> Result<TaskView, GatewayDaemonError>;

    fn retry(
        &mut self,
        actor: &ActorRef,
        catalog: &TaskLaunchCatalog,
        request: RetryTask,
    ) -> Result<TaskView, GatewayDaemonError>;

    fn resolve_approval(
        &mut self,
        actor_id: &ActorId,
        request: ResolveApproval,
    ) -> Result<TaskView, GatewayDaemonError>;

    fn resolve_approval_for_task(
        &mut self,
        actor_id: &ActorId,
        request: ResolveApprovalForTask,
    ) -> Result<TaskView, GatewayDaemonError>;

    fn append_input(
        &mut self,
        actor_id: &ActorId,
        request: AppendTaskInput,
    ) -> Result<TaskView, GatewayDaemonError>;

    fn switch_snapshot(
        &mut self,
        actor_id: &ActorId,
        request: SwitchTaskSnapshot,
    ) -> Result<TaskSnapshotSwitchView, GatewayDaemonError>;
}

/// Read-only Task projections available to the transport handler.
pub(super) trait TaskProjectionPort {
    fn list(&self, actor_id: &ActorId, limit: u16) -> Result<TaskListPage, GatewayDaemonError>;

    fn get(&self, actor_id: &ActorId, task_id: &TaskId) -> Result<TaskView, GatewayDaemonError>;

    fn events(
        &self,
        actor_id: &ActorId,
        task_id: &TaskId,
        after_revision: Option<u64>,
        limit: u16,
    ) -> Result<TaskEventPage, GatewayDaemonError>;

    fn snapshots(
        &mut self,
        actor_id: &ActorId,
        task_id: &TaskId,
    ) -> Result<TaskSnapshotList, GatewayDaemonError>;

    fn snapshot_preview(
        &mut self,
        actor_id: &ActorId,
        request: &super::InspectTaskSnapshot,
    ) -> Result<TaskSnapshotPreview, GatewayDaemonError>;
}

/// Trusted admission values selected before request dispatch.
pub(super) struct TaskAdmission<'a> {
    pub(super) catalog: &'a TaskLaunchCatalog,
}

/// Dispatches one authenticated request through Task command and projection ports.
pub(super) fn dispatch<P>(
    actor: &ActorRef,
    request: GatewayRequest,
    admission: TaskAdmission<'_>,
    ports: &mut P,
) -> Result<GatewayResult, GatewayDaemonError>
where
    P: TaskCommandPort + TaskProjectionPort,
{
    if request.api_version() != GATEWAY_API_VERSION {
        return Err(GatewayDaemonError::Protocol(
            "unsupported Gateway API version".to_owned(),
        ));
    }
    match request {
        GatewayRequest::Ping { .. } => Ok(GatewayResult::Pong),
        GatewayRequest::Capabilities { .. } => Ok(GatewayResult::Capabilities(
            admission.catalog.capabilities(),
        )),
        GatewayRequest::SubmitLaunch { request, .. } => ports
            .submit_launch(actor, admission.catalog, request)
            .map(GatewayResult::Task),
        GatewayRequest::Submit { request, .. } => {
            validate_submission_admission(&request, admission.catalog)?;
            ports
                .submit(actor, &admission.catalog.default_workspace, request)
                .map(GatewayResult::Task)
        }
        GatewayRequest::Get { task_id, .. } => ports
            .get(&actor.actor_id, &task_id)
            .map(GatewayResult::Task),
        GatewayRequest::List { limit, .. } => {
            ports.list(&actor.actor_id, limit).map(GatewayResult::Tasks)
        }
        GatewayRequest::Events {
            task_id,
            after_revision,
            limit,
            ..
        } => ports
            .events(&actor.actor_id, &task_id, after_revision, limit)
            .map(GatewayResult::Events),
        GatewayRequest::Cancel { request, .. } => ports
            .cancel(&actor.actor_id, request)
            .map(GatewayResult::Cancelled),
        GatewayRequest::Retry { request, .. } => ports
            .retry(actor, admission.catalog, request)
            .map(GatewayResult::Retried),
        GatewayRequest::ResolveApproval { request, .. } => ports
            .resolve_approval(&actor.actor_id, request)
            .map(GatewayResult::ApprovalResolved),
        GatewayRequest::ResolveApprovalForTask { request, .. } => ports
            .resolve_approval_for_task(&actor.actor_id, request)
            .map(GatewayResult::ApprovalResolved),
        GatewayRequest::AppendInput { request, .. } => ports
            .append_input(&actor.actor_id, request)
            .map(GatewayResult::InputAppended),
        GatewayRequest::ListTaskSnapshots { task_id, .. } => ports
            .snapshots(&actor.actor_id, &task_id)
            .map(GatewayResult::TaskSnapshots),
        GatewayRequest::PreviewTaskSnapshot { request, .. }
        | GatewayRequest::DiffTaskSnapshot { request, .. } => ports
            .snapshot_preview(&actor.actor_id, &request)
            .map(GatewayResult::TaskSnapshotPreview),
        GatewayRequest::SwitchTaskSnapshot { request, .. } => ports
            .switch_snapshot(&actor.actor_id, request)
            .map(GatewayResult::TaskSnapshotSwitched),
    }
}

pub(super) fn validate_submission_admission(
    request: &SubmitTask,
    catalog: &TaskLaunchCatalog,
) -> Result<GatewayCapabilityProfile, GatewayDaemonError> {
    catalog
        .legacy_admission(&request.target, &request.runtime)
        .ok_or_else(|| {
            GatewayDaemonError::Protocol(
                "Task target or Runtime is not admitted by this daemon".to_owned(),
            )
        })
}

//! Local Task and administration command handlers.

use super::*;

pub(super) fn admin(args: AdminArgs, reporter: &Reporter) -> Result<u8, CliError> {
    match args.command {
        AdminCommand::Inspect(command) => {
            let report = inspect_task_store(command.database)
                .map_err(|error| CliError::StoreInspection(error.to_string()))?;
            let exit = if report.outcome == StoreInspectionOutcome::Healthy {
                0
            } else {
                EXIT_STORE_INSPECTION
            };
            reporter.event(
                "store_inspection",
                serde_json::to_value(report)
                    .map_err(|error| CliError::StoreInspection(error.to_string()))?,
            )?;
            Ok(exit)
        }
    }
}

pub(super) fn task(args: TaskArgs, reporter: &Reporter) -> Result<u8, CliError> {
    let socket = daemon_socket_path(args.socket.as_ref())?;
    let client = LocalGatewayClient::new(socket);
    let result = match args.command {
        TaskCommand::Capabilities => client.capabilities(RequestId::new()),
        TaskCommand::Submit(command) => {
            let capabilities = match client
                .capabilities(RequestId::new())
                .map_err(|error| CliError::Daemon(error.to_string()))?
            {
                GatewayResult::Capabilities(capabilities) => capabilities,
                _ => {
                    return Err(CliError::Daemon(
                        "Gateway returned an invalid capabilities response".to_owned(),
                    ));
                }
            };
            verify_expected_workspace(
                command.expected_workspace_digest.as_deref(),
                &capabilities.default_workspace,
            )?;
            let request = SubmitLaunch {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new(command.idempotency_key)
                    .map_err(|error| CliError::InvalidInput(error.to_string()))?,
                launch: TaskLaunchSpecV1::new(
                    BoundedText::new(read_intent(command.intent_file.as_ref())?)
                        .map_err(|error| CliError::InvalidInput(error.to_string()))?,
                    command.runtime.into(),
                    capabilities.default_workspace,
                    command.checkpoint.into(),
                    command.approval_policy.into(),
                ),
            };
            client.submit_launch(request)
        }
        TaskCommand::List(command) => client.list(RequestId::new(), command.limit),
        TaskCommand::Get(command) => client.get(RequestId::new(), parse_task(&command.task_id)?),
        TaskCommand::Events(command) => client.events(
            RequestId::new(),
            parse_task(&command.task_id)?,
            (command.after != 0).then_some(command.after),
            command.limit,
        ),
        TaskCommand::Cancel(command) => client.cancel(CancelTask {
            request_id: RequestId::new(),
            idempotency_key: IdempotencyKey::new(command.idempotency_key)
                .map_err(|error| CliError::InvalidInput(error.to_string()))?,
            task_id: parse_task(&command.task_id)?,
            run_id: RunId::parse(&command.run_id)
                .map_err(|error| CliError::InvalidInput(error.to_string()))?,
            expected_revision: command.expected_revision,
        }),
        TaskCommand::Retry(command) => client.retry(RetryTask {
            request_id: RequestId::new(),
            idempotency_key: IdempotencyKey::new(command.idempotency_key)
                .map_err(|error| CliError::InvalidInput(error.to_string()))?,
            task_id: parse_task(&command.task_id)?,
            previous_run_id: RunId::parse(&command.previous_run_id)
                .map_err(|error| CliError::InvalidInput(error.to_string()))?,
            expected_revision: command.expected_revision,
        }),
        TaskCommand::ResolveApproval(command) => client.resolve_approval(ResolveApproval {
            request_id: RequestId::new(),
            idempotency_key: IdempotencyKey::new(command.idempotency_key)
                .map_err(|error| CliError::InvalidInput(error.to_string()))?,
            approval_id: ApprovalId::parse(&command.approval_id)
                .map_err(|error| CliError::InvalidInput(error.to_string()))?,
            decision: command.decision.into(),
        }),
        TaskCommand::Append(command) => {
            let response = if command.selections.is_empty() {
                RuntimeInputResponse::Text {
                    text: BoundedText::new(read_intent(command.input_file.as_ref())?)
                        .map_err(|error| CliError::InvalidInput(error.to_string()))?,
                }
            } else {
                RuntimeInputResponse::Options {
                    selections: RuntimeInputSelections::new(command.selections)
                        .map_err(|error| CliError::InvalidInput(error.to_string()))?,
                }
            };
            client.append_input(AppendTaskInput {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new(command.idempotency_key)
                    .map_err(|error| CliError::InvalidInput(error.to_string()))?,
                task_id: parse_task(&command.task_id)?,
                input_request_id: InputRequestId::parse(&command.input_request_id)
                    .map_err(|error| CliError::InvalidInput(error.to_string()))?,
                response,
                expected_revision: command.expected_revision,
            })
        }
        TaskCommand::Snapshot(command) => match command.command {
            TaskSnapshotCommand::List(command) => {
                client.task_snapshots(RequestId::new(), parse_task(&command.task_id)?)
            }
            TaskSnapshotCommand::Preview(command) => client.preview_task_snapshot(
                RequestId::new(),
                InspectTaskSnapshot {
                    task_id: parse_task(&command.task_id)?,
                    snapshot_id: CheckpointId::parse(&command.snapshot_id)
                        .map_err(|error| CliError::InvalidInput(error.to_string()))?,
                },
            ),
            TaskSnapshotCommand::Diff(command) => client.diff_task_snapshot(
                RequestId::new(),
                InspectTaskSnapshot {
                    task_id: parse_task(&command.task_id)?,
                    snapshot_id: CheckpointId::parse(&command.snapshot_id)
                        .map_err(|error| CliError::InvalidInput(error.to_string()))?,
                },
            ),
            TaskSnapshotCommand::Switch(command) => {
                client.switch_task_snapshot(SwitchTaskSnapshot {
                    request_id: RequestId::new(),
                    idempotency_key: IdempotencyKey::new(command.idempotency_key)
                        .map_err(|error| CliError::InvalidInput(error.to_string()))?,
                    task_id: parse_task(&command.task_id)?,
                    snapshot_id: CheckpointId::parse(&command.snapshot_id)
                        .map_err(|error| CliError::InvalidInput(error.to_string()))?,
                    preview_digest: Digest::parse(command.preview_digest)
                        .map_err(|error| CliError::InvalidInput(error.to_string()))?,
                    expected_revision: command.expected_revision,
                })
            }
        },
    }
    .map_err(|error| CliError::Daemon(error.to_string()))?;
    report_gateway_result(reporter, result)?;
    Ok(0)
}

pub(super) fn verify_expected_workspace(
    expected: Option<&str>,
    actual: &cosh_gateway_contracts::common::WorkspaceRef,
) -> Result<(), CliError> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let expected = cosh_gateway_contracts::common::Digest::parse(expected)
        .map_err(|error| CliError::InvalidInput(error.to_string()))?;
    if actual.scope_digest != expected {
        return Err(CliError::Profile(
            "Gateway canonical workspace changed after Task confirmation".to_owned(),
        ));
    }
    Ok(())
}

fn report_gateway_result(reporter: &Reporter, result: GatewayResult) -> Result<(), CliError> {
    match result {
        GatewayResult::Pong => reporter.event("daemon_pong", json!({})),
        GatewayResult::Capabilities(capabilities) => reporter.event(
            "task_capabilities",
            serde_json::to_value(capabilities)
                .map_err(|error| CliError::Daemon(error.to_string()))?,
        ),
        GatewayResult::Task(task) => reporter.event(
            "task",
            serde_json::to_value(task).map_err(|error| CliError::Daemon(error.to_string()))?,
        ),
        GatewayResult::Tasks(tasks) => reporter.event(
            "tasks",
            serde_json::to_value(tasks).map_err(|error| CliError::Daemon(error.to_string()))?,
        ),
        GatewayResult::Events(events) => reporter.event(
            "task_events",
            serde_json::to_value(events).map_err(|error| CliError::Daemon(error.to_string()))?,
        ),
        GatewayResult::Cancelled(task) => reporter.event(
            "task_cancelled",
            serde_json::to_value(task).map_err(|error| CliError::Daemon(error.to_string()))?,
        ),
        GatewayResult::Retried(task) => reporter.event(
            "task_retried",
            serde_json::to_value(task).map_err(|error| CliError::Daemon(error.to_string()))?,
        ),
        GatewayResult::InputAppended(task) => reporter.event(
            "task_input_appended",
            serde_json::to_value(task).map_err(|error| CliError::Daemon(error.to_string()))?,
        ),
        GatewayResult::ApprovalResolved(task) => reporter.event(
            "approval_resolved",
            serde_json::to_value(task).map_err(|error| CliError::Daemon(error.to_string()))?,
        ),
        GatewayResult::TaskSnapshots(snapshots) => reporter.event(
            "task_snapshots",
            serde_json::to_value(snapshots).map_err(|error| CliError::Daemon(error.to_string()))?,
        ),
        GatewayResult::TaskSnapshotPreview(preview) => reporter.event(
            "task_snapshot_preview",
            serde_json::to_value(preview).map_err(|error| CliError::Daemon(error.to_string()))?,
        ),
        GatewayResult::TaskSnapshotSwitched(switched) => reporter.event(
            "task_snapshot_switched",
            serde_json::to_value(switched).map_err(|error| CliError::Daemon(error.to_string()))?,
        ),
    }
}

fn parse_task(value: &str) -> Result<TaskId, CliError> {
    TaskId::parse(value).map_err(|error| CliError::InvalidInput(error.to_string()))
}

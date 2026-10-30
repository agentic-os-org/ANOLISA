//! Dispatch the versioned service control operations.

use super::{Job, Runtime};
use crate::{rejected, Operation, Result};
use aw_host::ProcessContext;
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::Instant,
};

impl Runtime {
    pub(crate) fn handle(
        self: &Arc<Self>,
        operation: Operation,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Value> {
        if Instant::now() >= deadline {
            return Err(rejected("deadline_exceeded"));
        }
        if self.stopped.load(Ordering::Acquire) {
            return Err(rejected("service_stopping"));
        }
        match operation {
            Operation::RecentExecutions => self.audit.recent(),
            Operation::Status => {
                let observations = self.observation_status()?;
                let policies = self.policy_status()?;
                let resources = self.resource_status()?;
                Ok(
                    json!({"pid": std::process::id(), "bindings": self.instances.lock().map_err(|_| rejected("state_poisoned"))?.len(),
                "events": self.events.lock().map_err(|_| rejected("state_poisoned"))?.len(), "audit_healthy": self.audit.healthy(),
                "resources": resources, "observations": observations, "instances": policies, "guarantee": "native_hook", "adoption": "unverified"}),
                )
            }
            Operation::EnableInvocations { instance_id } => self.enable_invocations(&instance_id),
            Operation::RegisterInvocation {
                instance_id,
                sequence,
                call_id,
                tool_name,
                input,
            } => self.register_invocation(&instance_id, sequence, &call_id, &tool_name, input),
            Operation::FinishInvocation {
                instance_id,
                sequence,
                call_id,
            } => self.finish_invocation(&instance_id, sequence, &call_id, deadline),
            Operation::ScopePolicy { instance_id } => self.scope_policy(&instance_id),
            Operation::AcknowledgeScope { instance_id } => self.acknowledge_scope(&instance_id),
            Operation::OwnInstance { instance_id, pid } => self.own_instance(&instance_id, pid),
            Operation::OpenScope {
                parent_id,
                pid,
                cwd,
                environment,
            } => self.open_scope(
                &parent_id,
                pid,
                ProcessContext {
                    cwd: cwd.into(),
                    environment,
                    stderr_bytes: 64 * 1024,
                },
                deadline,
                cancelled,
            ),
            Operation::EnableReload { instance_id } => self.enable_reload(&instance_id),
            Operation::AssociateProcess { instance_id, pid } => {
                self.associate_process(&instance_id, pid)
            }
            Operation::BridgeLifecycle {
                instance_id,
                pid,
                phase,
                key,
            } => self.lifecycle(&instance_id, pid, phase, &key),
            Operation::HookPlan { instance_id, event } => self.hook_plan(&instance_id, &event),
            Operation::UpdatePolicy { .. } => Err(rejected("policy_update_requires_peer")),
            Operation::StartObservation { instance_id } => {
                self.start_observation(&instance_id, deadline)
            }
            Operation::ReportRun {
                instance_id,
                outcome,
            } => self.report_run(&instance_id, outcome),
            Operation::ObserveHookEvent {
                instance_id,
                event,
                native_input,
            } => self.observe_hook(&instance_id, event, native_input, deadline),
            Operation::Bind {
                target,
                capabilities,
                cwd,
                environment,
            } => self.bind(
                target,
                capabilities,
                ProcessContext {
                    cwd: cwd.into(),
                    environment,
                    stderr_bytes: 64 * 1024,
                },
                deadline,
                cancelled,
            ),
            Operation::OpenEvent { instance_id, event } => self.open(
                instance_id,
                event,
                None,
                deadline,
                Arc::new(AtomicBool::new(false)),
            ),
            Operation::OpenHookEvent {
                instance_id,
                event,
                native_input,
            } => self.open_hook(instance_id, event, native_input, deadline),
            Operation::CloseHookEvent {
                instance_id,
                event,
                session_id,
                call_id,
            } => self.close_hook(&instance_id, &event, &session_id, &call_id, deadline),
            Operation::InvokeStep {
                event_id,
                instance_id,
                step_id,
                native_environment,
            } => {
                self.check_scope_owner(&instance_id)?;
                let (sender, cancelled) = self.event_sender(&event_id, &instance_id)?;
                let (reply, result) = mpsc::sync_channel(1);
                sender
                    .try_send(Job::Invoke {
                        step: step_id,
                        native_environment,
                        reply,
                    })
                    .map_err(|_| rejected("event_busy_or_closed"))?;
                result
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .map_err(|_| {
                        cancelled.store(true, Ordering::Release);
                        rejected("event_reply_unavailable")
                    })?
            }
            Operation::CloseEvent {
                event_id,
                instance_id,
            } => {
                let (sender, _) = self.event_sender(&event_id, &instance_id)?;
                let (reply, result) = mpsc::sync_channel(1);
                sender
                    .try_send(Job::Close { reply })
                    .map_err(|_| rejected("event_busy_or_closed"))?;
                result
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .map_err(|_| rejected("event_reply_unavailable"))?
            }
            Operation::Unbind { instance_id } => {
                let mut instances = self
                    .instances
                    .lock()
                    .map_err(|_| rejected("state_poisoned"))?;
                let instance = instances
                    .get_mut(&instance_id)
                    .and_then(Option::as_mut)
                    .ok_or_else(|| rejected("unknown_instance"))?;
                if instance.reload.is_some() && !instance.closing {
                    self.idle(instance, &instance_id)?;
                } else if self
                    .events
                    .lock()
                    .map_err(|_| rejected("state_poisoned"))?
                    .values()
                    .any(|entry| entry.instance_id == instance_id)
                {
                    return Err(rejected("instance_has_events"));
                }
                instance.closing = true;
                drop(instances);
                self.release_instance(&instance_id, deadline)
            }
            Operation::ReleaseInstance { instance_id } => {
                self.release_instance(&instance_id, deadline)
            }
            Operation::Audit { key } => self.audit.read(&key),
            Operation::FreezeContext {
                event_id,
                instance_id,
                candidate_revision,
            } => self.freeze_context(&event_id, &instance_id, &candidate_revision, deadline),
            Operation::Stop => {
                self.stopped.store(true, Ordering::Release);
                Ok(json!({"stopping": true}))
            }
        }
    }
}

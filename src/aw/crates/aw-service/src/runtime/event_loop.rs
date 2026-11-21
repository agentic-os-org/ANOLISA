//! Schedule native callbacks before spending any budget on shadow evaluation.

use super::{audit, input, policy, Job, Runtime};
use crate::{rejected, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Mutex,
    },
    thread,
    time::Instant,
};

use super::evidence;

impl Runtime {
    pub(super) fn event_loop(
        &self,
        identity: (&str, &str, &Value),
        event: &aw_host::Event<'_>,
        lease: (&AtomicBool, &evidence::Cell, Option<&policy::Execution>),
        receiver: Receiver<Job>,
        automatic: bool,
        ordered: bool,
    ) {
        let (key, instance_id, original) = identity;
        let rewrite = Mutex::new(input::State::default());
        let (cancelled, context, execution) = lease;
        let mut close = None;
        let mut claimed = BTreeSet::new();
        let mut panicked = false;
        let completed_steps = AtomicU64::new(0);
        let settled_steps = Mutex::new(0);
        let completed_mask = AtomicU64::new(0);
        let callbacks = event.callback_steps();
        let step_count = callbacks.len() as u64;
        let has_shadows = event.shadow_steps().next().is_some();
        let shadow_reserved = AtomicBool::new(false);
        let reservation = has_shadows.then(|| ShadowReservation { runtime: self, key });
        let deliver = |reply: SyncSender<Result<Value>>, result| {
            // Publish the native replies in the same order as settlement. The
            // final reply may immediately cause another event to be opened.
            let Ok(mut settled) = settled_steps.lock() else {
                let _ = reply.send(Err(rejected("state_poisoned")));
                return;
            };
            *settled += 1;
            if *settled == step_count && has_shadows {
                shadow_reserved.store(self.reserve_shadow(key, automatic), Ordering::Release);
            }
            let _ = reply.send(result);
            completed_steps.fetch_add(1, Ordering::Release);
        };
        thread::scope(|scope| {
            let mut children = Vec::new();
            let mut shadow_worker = None;
            while Instant::now() < event.deadline()
                && !self.stopped.load(Ordering::Acquire)
                && !cancelled.load(Ordering::Acquire)
            {
                let callbacks_done = completed_steps.load(Ordering::Acquire) == step_count;
                if !automatic
                    && callbacks_done
                    && shadow_reserved.load(Ordering::Acquire)
                    && shadow_worker.is_none()
                {
                    shadow_worker = Some(scope.spawn(|| {
                        self.evaluate_shadows(key, instance_id, event, execution, cancelled);
                    }));
                }
                if automatic && callbacks_done {
                    break;
                }
                let job = match receiver.recv_timeout(
                    event
                        .deadline()
                        .saturating_duration_since(Instant::now())
                        .min(std::time::Duration::from_millis(20)),
                ) {
                    Ok(job) => job,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                match job {
                    Job::Completion {
                        value,
                        input,
                        reply,
                    } => {
                        let result = event
                            .completion_view(value, input)
                            .map(|()| json!({}))
                            .map_err(|_| rejected("native_completion_invalid"));
                        let _ = reply.send(result);
                    }
                    Job::Close { reply } => {
                        close = Some(reply);
                        break;
                    }
                    Job::Invoke {
                        step,
                        native_environment,
                        reply,
                    } => {
                        if cancelled.load(Ordering::Acquire) || self.stopped.load(Ordering::Acquire)
                        {
                            let _ = reply.send(Err(rejected("event_cancelled")));
                            continue;
                        }
                        if let Err(error) = self.check_execution_admission(instance_id, execution) {
                            let _ = reply.send(Err(error));
                            continue;
                        }
                        let Some(index) = callbacks
                            .iter()
                            .position(|candidate| candidate.step_id == step)
                        else {
                            let _ = reply.send(Err(rejected("step_unknown_or_claimed")));
                            continue;
                        };
                        if !claimed.insert(step.clone()) {
                            let _ = reply.send(Err(rejected("step_unknown_or_claimed")));
                            continue;
                        }
                        if event.is_shadow(callbacks[index]) {
                            deliver(reply, Ok(audit::shadow_reply()));
                            continue;
                        }
                        // Admission caps events at 64 steps, so every step has a bit.
                        let bit = 1u64 << index;
                        if self
                            .calls
                            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                                (n < self.limits.calls as u64).then_some(n + 1)
                            })
                            .is_err()
                        {
                            completed_mask.fetch_or(bit, Ordering::Release);
                            deliver(reply, Err(self.overload("call_limit")));
                            continue;
                        }
                        let deliver = &deliver;
                        let completed_mask = &completed_mask;
                        let rewrite = &rewrite;
                        children.push(scope.spawn(move || {
                            let call = CallGuard(&self.calls);
                            let result = (|| {
                                while ordered
                                    && completed_mask.load(Ordering::Acquire) & (bit - 1) != bit - 1
                                {
                                    if Instant::now() >= event.deadline()
                                        || cancelled.load(Ordering::Acquire)
                                        || self.stopped.load(Ordering::Acquire)
                                    {
                                        return Err(rejected("event_cancelled_or_expired"));
                                    }
                                    thread::sleep(std::time::Duration::from_millis(2));
                                }
                                self.check_execution_admission(instance_id, execution)?;
                                self.audit
                                    .append(key, json!({"phase": "started", "step_id": step}))?;
                                // Queueing and durable audit can outlive the admitted owner.
                                self.check_execution_admission(instance_id, execution)?;
                                let mut invocation = match native_environment {
                                    Some(environment) => {
                                        event.invoke_with_native_environment(&step, &environment)
                                    }
                                    None => event.invoke(&step),
                                }
                                .map_err(|_| rejected("step_rejected"))?;
                                let replacement = match self.adopt_input(
                                    instance_id,
                                    event,
                                    original,
                                    &mut invocation,
                                    rewrite,
                                    index + 1 == step_count as usize,
                                ) {
                                    Ok(input) => input,
                                    Err(_) => {
                                        invocation.result = Err(aw_host::Failure::Protocol(
                                            aw_provider::Error::Invalid(
                                                "input replacement adoption failed",
                                            ),
                                        ));
                                        invocation.failure_action =
                                            Some(aw_host::FailureAction::Block);
                                        None
                                    }
                                };
                                let (record, mut result) = audit::invocation(invocation);
                                if result["effects"].as_array().is_some_and(|effects| {
                                    effects
                                        .iter()
                                        .any(|effect| effect["type"] == "replace_input")
                                }) {
                                    result["effects"] = serde_json::json!([]);
                                }
                                if let Some(input) = replacement {
                                    result["input_replacement"] = input;
                                }
                                self.audit.append(key, record)?;
                                Ok(result)
                            })();
                            completed_mask.fetch_or(bit, Ordering::Release);
                            drop(call);
                            deliver(reply, result);
                        }));
                    }
                }
            }
            if !automatic || !shadow_reserved.load(Ordering::Acquire) {
                cancelled.store(true, Ordering::Release);
            }
            for child in children {
                panicked |= child.join().is_err();
            }
            // Moving the event can disconnect the foreground queue while the final
            // callback is sending its reply. Check delivery after joining the senders.
            let callbacks_done = completed_steps.load(Ordering::Acquire) == step_count;
            context.close();
            let status = if panicked {
                "worker_failed"
            } else if close.is_some() {
                "closed"
            } else if automatic && callbacks_done {
                "completed"
            } else if self.stopped.load(Ordering::Acquire) {
                "stopped"
            } else if Instant::now() >= event.deadline() {
                "expired"
            } else {
                "cancelled"
            };
            let completed = self.retire_event(
                (key, instance_id),
                event,
                &claimed,
                status,
                automatic,
                execution,
            );
            if let Some(reply) = close {
                let _ = reply
                    .send(completed.map(|evidence| json!({"closed": true, "evidence": evidence})));
            }
            if panicked {
                self.stopped.store(true, Ordering::Release);
            }
            self.audit.release(key);

            if automatic && callbacks_done && shadow_reserved.load(Ordering::Acquire) {
                self.evaluate_shadows(key, instance_id, event, execution, cancelled);
            } else if let Some(worker) = shadow_worker {
                let _ = worker.join();
            } else if has_shadows {
                drop(reservation);
                let reason = if Instant::now() >= event.deadline() {
                    "event_budget"
                } else if callbacks_done && !shadow_reserved.load(Ordering::Acquire) {
                    "shadow_busy"
                } else if callbacks_done {
                    "event_closed"
                } else {
                    "callback_incomplete"
                };
                self.audit.shadow.write(
                    key,
                    audit::shadow::unassessed(event.shadow_steps(), reason)
                        .into_iter()
                        .collect(),
                );
            }
        });
    }

    fn reserve_shadow(&self, key: &str, automatic: bool) -> bool {
        // Use the same lock order as cancellation. A release sees either the
        // foreground event or the optional reservation, never a gap between.
        let Ok(mut events) = self.events.lock() else {
            return false;
        };
        let Ok(mut shadow) = self.shadow.lock() else {
            return false;
        };
        let reserved = shadow.is_none()
            && events.get(key).is_some_and(|entry| {
                if entry.cancelled.load(Ordering::Acquire) || self.stopped.load(Ordering::Acquire) {
                    return false;
                }
                *shadow = Some(ShadowEvent {
                    key: key.into(),
                    instance_id: entry.instance_id.clone(),
                    cancelled: Arc::clone(&entry.cancelled),
                });
                true
            });
        // Native callbacks have settled. A manual OpenEvent retains its
        // explicit CloseEvent/expiry contract, regardless of step mode.
        if automatic {
            events.remove(key);
        }
        reserved
    }

    pub(super) fn cancel_shadow(&self, applies: impl FnOnce(&str) -> bool) -> Result<bool> {
        let shadow = self.shadow.lock().map_err(|_| rejected("state_poisoned"))?;
        Ok(shadow.as_ref().is_some_and(|event| {
            if applies(&event.instance_id) {
                event.cancelled.store(true, Ordering::Release);
                true
            } else {
                false
            }
        }))
    }

    fn evaluate_shadows(
        &self,
        key: &str,
        instance_id: &str,
        event: &aw_host::Event<'_>,
        execution: Option<&policy::Execution>,
        cancelled: &AtomicBool,
    ) {
        let reservation = ShadowReservation { runtime: self, key };
        let mut records = Vec::new();
        let deadline = event
            .deadline()
            .checked_sub(aw_exec::COOPERATIVE_CHILD_CLEANUP_GRACE);
        let mut steps = event.shadow_steps();
        while let Some(step) = steps.next() {
            let admission = if cancelled.load(Ordering::Acquire)
                || self.stopped.load(Ordering::Acquire)
                || self
                    .check_provider_admission(instance_id, execution)
                    .is_err()
            {
                Err("event_closed")
            } else {
                deadline
                    .filter(|deadline| Instant::now() < *deadline)
                    .ok_or("event_budget")
            };
            let deadline = match admission {
                Ok(deadline) => deadline,
                Err(reason) => {
                    records.extend(audit::shadow::unassessed(
                        std::iter::once(step).chain(steps),
                        reason,
                    ));
                    break;
                }
            };
            if let Ok(invocation) = event.invoke_shadow(&step.step_id, deadline, |event| {
                aw_adapter::projection::add_result_projection(event)
                    .map_err(|_| aw_provider::Error::Invalid("result projection failed"))
            }) {
                records.push(audit::invocation(invocation).0);
            } else {
                records.extend(audit::shadow::unassessed(
                    std::iter::once(step),
                    "step_unavailable",
                ));
            }
        }
        // Cleanup only waits for the optional Provider, including child reaping.
        // Observation storage cannot delay the release of its execution slot.
        drop(reservation);
        self.audit.shadow.write(key, records);
    }
}

pub(super) struct ShadowEvent {
    pub(super) key: String,
    instance_id: String,
    cancelled: Arc<AtomicBool>,
}

struct ShadowReservation<'a> {
    runtime: &'a Runtime,
    key: &'a str,
}

impl Drop for ShadowReservation<'_> {
    fn drop(&mut self) {
        if let Ok(mut shadow) = self.runtime.shadow.lock() {
            if shadow.as_ref().is_some_and(|event| event.key == self.key) {
                shadow.take();
            }
        }
    }
}

struct CallGuard<'a>(&'a AtomicU64);
impl Drop for CallGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

//! Retain once-only expiry replies without keeping Hosts, payloads or worker threads alive.

use super::{audit, Job, Runtime};
use crate::{rejected, Result};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::Instant,
};

pub(super) struct ExpiredEvent {
    pub(super) instance_id: String,
    pub(super) cancelled: Arc<AtomicBool>,
    pub(super) replies: BTreeMap<String, Value>,
    pub(super) automatic: bool,
    pub(super) execution: Option<super::policy::Execution>,
    pub(super) evidence: Value,
}

pub(super) fn replies(
    event: &aw_host::Event<'_>,
    claimed: &BTreeSet<String>,
) -> Result<BTreeMap<String, Value>> {
    event
        .steps()
        .filter(|step| !claimed.contains(&step.step_id))
        .map(|step| {
            let mut invocation = event
                .invoke(&step.step_id)
                .map_err(|_| rejected("expired_event_unavailable"))?;
            // Worker cancellation must not obscure the terminal deadline failure.
            invocation.result = Err(aw_host::Failure::Transport(
                aw_exec::Error::DeadlineExceeded,
            ));
            Ok((step.step_id.clone(), audit::invocation(invocation).1))
        })
        .collect()
}

impl Runtime {
    pub(super) fn retire_event(
        &self,
        identity: (&str, &str),
        event: &aw_host::Event<'_>,
        claimed: &BTreeSet<String>,
        mut status: &str,
        automatic: bool,
        execution: Option<&super::policy::Execution>,
    ) -> Result<Value> {
        let (key, instance_id) = identity;
        let replies = if status == "expired" {
            match self::replies(event, claimed) {
                Ok(replies) => replies,
                Err(_) => {
                    self.stopped.store(true, Ordering::Release);
                    status = "worker_failed";
                    BTreeMap::new()
                }
            }
        } else {
            BTreeMap::new()
        };
        let actions: BTreeMap<_, _> = replies
            .iter()
            .map(|(step, reply)| (step.clone(), reply["failure_action"].clone()))
            .collect();
        let completed = self.audit.append(key, json!({"phase": "closed", "status": status, "attempted_steps": claimed.len(), "expired_failure_actions":actions}));
        // Admission and retirement share the event lock, keeping retained replies bounded.
        let mut events = self.events.lock().map_err(|_| rejected("state_poisoned"))?;
        if let Some(entry) = events.remove(key).filter(|_| {
            status == "expired"
                && (!automatic || !replies.is_empty())
                && !self.stopped.load(Ordering::Acquire)
        }) {
            if let Ok(evidence) = &completed {
                self.expired
                    .lock()
                    .map_err(|_| rejected("state_poisoned"))?
                    .insert(
                        key.into(),
                        ExpiredEvent {
                            instance_id: instance_id.into(),
                            cancelled: entry.cancelled,
                            replies,
                            automatic,
                            execution: execution.cloned(),
                            evidence: evidence.clone(),
                        },
                    );
            }
        }
        completed
    }

    fn expired_reply(&self, key: &str, instance: &str, step: &str) -> Result<Option<Value>> {
        let execution = {
            let expired = self
                .expired
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            let Some(entry) = expired
                .get(key)
                .filter(|entry| entry.instance_id == instance)
            else {
                return Ok(None);
            };
            entry.execution.clone()
        };
        // Keep the same owner/reservation fence as a live worker. Never acquire
        // the instance lock while holding the expiry lock (open uses the reverse order).
        self.check_execution_admission(instance, execution.as_ref())?;
        let mut expired = self
            .expired
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        let Some(entry) = expired
            .get_mut(key)
            .filter(|entry| entry.instance_id == instance)
        else {
            return Ok(None);
        };
        let reply = entry
            .replies
            .remove(step)
            .ok_or_else(|| rejected("step_unknown_or_claimed"))?;
        if entry.automatic && entry.replies.is_empty() {
            expired.remove(key);
        }
        Ok(Some(reply))
    }

    fn close_expired(&self, key: &str, instance: &str) -> Result<Option<Value>> {
        let mut expired = self
            .expired
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        if !expired
            .get(key)
            .is_some_and(|entry| entry.instance_id == instance)
        {
            return Ok(None);
        }
        Ok(expired
            .remove(key)
            .map(|entry| json!({"closed":true,"evidence":entry.evidence})))
    }

    pub(super) fn invoke_event_step(
        &self,
        key: &str,
        instance: &str,
        step: String,
        native_environment: Option<BTreeMap<OsString, OsString>>,
        deadline: Instant,
    ) -> Result<Value> {
        self.check_scope_owner(instance)?;
        let (sender, cancelled) = match self.event_sender(key, instance) {
            Ok(entry) => entry,
            Err(crate::Error::Rejected(code)) if code == "unknown_event" => {
                return self
                    .expired_reply(key, instance, &step)?
                    .ok_or_else(|| rejected("unknown_event"));
            }
            Err(error) => return Err(error),
        };
        let (reply, result) = mpsc::sync_channel(1);
        if sender
            .try_send(Job::Invoke {
                step: step.clone(),
                native_environment,
                reply,
            })
            .is_err()
        {
            return self
                .expired_reply(key, instance, &step)?
                .ok_or_else(|| rejected("event_busy_or_closed"));
        }
        match result.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Disconnected) => self
                .expired_reply(key, instance, &step)?
                .ok_or_else(|| rejected("event_reply_unavailable")),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                cancelled.store(true, Ordering::Release);
                Err(rejected("event_reply_unavailable"))
            }
        }
    }

    pub(super) fn close_event(
        &self,
        key: &str,
        instance: &str,
        deadline: Instant,
    ) -> Result<Value> {
        let (sender, _) = match self.event_sender(key, instance) {
            Ok(entry) => entry,
            Err(crate::Error::Rejected(code)) if code == "unknown_event" => {
                return self
                    .close_expired(key, instance)?
                    .ok_or_else(|| rejected("unknown_event"));
            }
            Err(error) => return Err(error),
        };
        let (reply, result) = mpsc::sync_channel(1);
        if sender.try_send(Job::Close { reply }).is_err() {
            return self
                .close_expired(key, instance)?
                .ok_or_else(|| rejected("event_busy_or_closed"));
        }
        match result.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Disconnected) => self
                .close_expired(key, instance)?
                .ok_or_else(|| rejected("event_reply_unavailable")),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(rejected("event_reply_unavailable")),
        }
    }
}

#[cfg(test)]
mod tests;

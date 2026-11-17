//! Source admission, completion, and bounded worker retirement.

use super::*;

impl RunningScan {
    pub(super) fn dispatch_next(&mut self) -> Option<HealthUnavailableReason> {
        if !self.configured || self.next_source >= self.plan.len() {
            return None;
        }
        let source = self.plan[self.next_source].source;
        let check_ids = self.plan[self.next_source].check_ids.clone();
        let budget = self.plan[self.next_source].budget;
        let finish = self.plan[self.next_source].finish;
        let remaining = self.request.deadline.remaining().unwrap_or_default();
        let spawn_cost = if self.child.is_none() {
            SPAWN_ADMIT_COST
        } else {
            Duration::ZERO
        };
        let effective = remaining.min(budget);
        if effective < spawn_cost + cleanup_reservation(finish, effective) + MIN_SOURCE_WORK {
            return Some(HealthUnavailableReason::Timeout);
        }
        let Ok(mut task_deadline) = HealthDeadline::after(effective) else {
            return Some(HealthUnavailableReason::ParseError);
        };
        task_deadline.expires_ns = task_deadline
            .expires_ns
            .min(self.request.deadline.expires_ns);
        let mut work_deadline = task_deadline;
        work_deadline.shorten_by(cleanup_reservation(finish, effective));
        self.task_id += 1;
        self.awaiting = Some(AwaitedTask {
            task_id: self.task_id,
            finish,
            deadline: task_deadline,
            work_deadline,
        });
        self.next_source += 1;
        if self.child.is_none() {
            if let Err(error) = self.launch() {
                return self.fail_source(error_reason(&error));
            }
        }
        let task = CollectTask {
            task_id: self.task_id,
            source,
            check_ids,
            deadline: task_deadline,
            work_deadline,
        };
        let bytes = match protocol::encode(&self.nonce, self.task_id, Payload::Collect(task)) {
            Ok(bytes) => bytes,
            Err(error) => return self.fail_source(error_reason(&error)),
        };
        let sent = self
            .stream
            .as_mut()
            .map(|stream| protocol::send(stream, &bytes, work_deadline))
            .is_some_and(|result| result.is_ok());
        if !sent {
            let reason = if work_deadline.remaining().unwrap_or_default().is_zero() {
                HealthUnavailableReason::Timeout
            } else {
                HealthUnavailableReason::ParseError
            };
            return self.fail_source(reason);
        }
        None
    }

    pub(super) fn expire_source(&mut self) -> Option<HealthUnavailableReason> {
        let received = self
            .stream
            .as_mut()
            .map(|stream| self.decoder.receive_buffered(stream));
        let applied = self.apply_frames();
        let invalid = (self.source_done && !self.decoder.is_empty())
            || received.is_some_and(|result| result.is_err())
            || applied
                .as_ref()
                .is_err_and(|error| error_reason(error) == HealthUnavailableReason::ParseError);
        if invalid {
            if let Some(source) = self
                .next_source
                .checked_sub(1)
                .and_then(|i| self.plan.get(i))
            {
                self.builder.discard_check_facts(&source.check_ids);
            }
        }
        self.fail_source(if invalid {
            HealthUnavailableReason::ParseError
        } else {
            HealthUnavailableReason::Timeout
        })
    }

    pub(super) fn complete_source(&mut self) -> Option<HealthUnavailableReason> {
        let awaited = self.awaiting.as_ref()?;
        let ids = self
            .plan
            .get(self.next_source.checked_sub(1)?)?
            .check_ids
            .clone();
        if !self.decoder.is_empty() || ids.iter().any(|id| self.builder.is_pending(id)) {
            self.builder.discard_check_facts(&ids);
            return self.fail_source(HealthUnavailableReason::ParseError);
        }
        let success = self
            .builder
            .report()
            .check_results
            .iter()
            .filter(|check| ids.contains(&check.id))
            .all(|check| check.status == HealthCheckStatus::Ok);
        if success && awaited.finish == FinishPolicy::ReuseOnSuccess && self.child.is_some() {
            self.awaiting.take();
            return self.dispatch_next().or_else(|| {
                (!self.builder.has_pending()).then_some(HealthUnavailableReason::Timeout)
            });
        }
        if !self.retire_child() {
            self.mark_source_gap(self.cleanup_failure_reason());
            return Some(self.cleanup_failure_reason());
        }
        self.awaiting.take();
        self.dispatch_next()
            .or_else(|| (!self.builder.has_pending()).then_some(HealthUnavailableReason::Timeout))
    }

    pub(super) fn fail_source(
        &mut self,
        reason: HealthUnavailableReason,
    ) -> Option<HealthUnavailableReason> {
        if self.awaiting.is_none() {
            return Some(reason);
        }
        self.mark_source_gap(reason);
        if !self.retire_child() {
            return Some(self.cleanup_failure_reason());
        }
        self.awaiting.take();
        self.dispatch_next()
            .or_else(|| (!self.builder.has_pending()).then_some(HealthUnavailableReason::Timeout))
    }

    pub(super) fn mark_source_gap(&mut self, reason: HealthUnavailableReason) {
        if self.awaiting.is_some() && self.next_source > 0 {
            let failed = &self.plan[self.next_source - 1];
            let elapsed = self
                .request
                .deadline
                .elapsed()
                .unwrap_or_else(|_| self.started.elapsed())
                .as_millis();
            for id in &failed.check_ids {
                if let Some(collector) = HealthCollector::from_check_id(id) {
                    self.builder.seal_check_gap(id, collector, reason, elapsed);
                }
            }
        }
    }

    pub(super) fn retire_child(&mut self) -> bool {
        let Some(mut child) = self.child.take() else {
            return true;
        };
        let cleanup_deadline = self
            .awaiting
            .as_ref()
            .map(|task| task.deadline)
            .unwrap_or(self.request.deadline);
        if let Ok(deadline) = cleanup_deadline.local_instant() {
            child.shorten_deadline(deadline);
        }
        let reaped = matches!(child.cleanup(), Ok(Some(_)));
        let confirmed = reaped
            && self.child_pgid.is_none_or(|pgid| {
                confirm_group_gone(
                    pgid,
                    cleanup_deadline
                        .local_instant()
                        .unwrap_or_else(|_| Instant::now())
                        .min(Instant::now() + GROUP_CONFIRM_BUDGET),
                )
            });
        self.child_pgid = None;
        if let Some(stream) = self.stream.take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
        self.decoder = Decoder::default();
        self.source_done = false;
        confirmed
    }
}

//! Serial dispatch and terminal decisions follow the existing plan validator.

use crate::{
    ports::{Cancellation, Clock, Journal, ProviderHost},
    CallRecord, Core, Error, Execution, PreparedPlan,
};
use aw_contracts::canonical;
use serde_json::{json, Value};

struct JournalClaim<'a, J: Journal> {
    journal: &'a mut J,
    event_key: &'a str,
}

impl<J: Journal> Drop for JournalClaim<'_, J> {
    fn drop(&mut self) {
        self.journal.release(self.event_key);
    }
}

impl Core {
    /// Executes a pinned plan once, acknowledging records before further dispatch.
    ///
    /// No retries or candidate adoption occur here. All selected providers in
    /// one step settle before its decision is reduced; terminal decisions skip
    /// later steps. Local journal ownership is released on return or unwinding;
    /// a failed storage/Host boundary leaves its durable reservation intact.
    ///
    /// # Errors
    /// Returns errors for duplicate events, journal failures, descriptor drift,
    /// expired calls or malformed Host evidence. Such errors never authorize
    /// the caller to dispatch a native tool or adopt a candidate.
    pub fn execute(
        &self,
        prepared: PreparedPlan,
        host: &mut impl ProviderHost,
        journal: &mut impl Journal,
        clock: &impl Clock,
        cancellation: &impl Cancellation,
    ) -> Result<Execution, Error> {
        let PreparedPlan {
            plan,
            boundary,
            runtime,
            event_key,
            steps,
        } = prepared;
        let claim = journal.claim(&event_key, &plan)?;
        // Own cleanup before validating the acknowledgement: even a malformed
        // successful claim must release its writer, while failed claims must not.
        let reservation = JournalClaim {
            journal,
            event_key: &event_key,
        };
        let claim = self.validate_journal_ack(claim)?;
        let mut entries = Vec::new();
        let mut calls = Vec::new();
        let mut decision = "proceed";
        let mut sequence = 0u64;
        for prepared_step in steps {
            let step = prepared_step.step;
            if decision != "proceed" {
                entries.push(json!({
                    "step_id": step["step_id"], "outcome": "skipped",
                    "invocations": [], "reason": "previous_step_stopped"
                }));
                continue;
            }
            sequence += 1;
            let started = sequence;
            self.validate_journal_ack(reservation.journal.append(
                &event_key,
                &json!({
                    "kind": "step_started", "step_id": step["step_id"], "sequence": started
                }),
            )?)?;
            let mut references = Vec::new();
            let mut gap = prepared_step.calls.is_empty();
            let mut rejected = false;
            let mut cancelled = cancellation.is_cancelled();
            for call in prepared_step.calls {
                if cancelled || cancellation.is_cancelled() {
                    cancelled = true;
                    break;
                }
                let invocation = call.invocation;
                let provider_id = invocation["provider_id"]
                    .as_str()
                    .ok_or(Error::Preparation("missing admitted provider ID"))?;
                if host.descriptor(provider_id) != Some(&call.provider) {
                    return Err(Error::ProviderChanged);
                }
                self.registry.validate_invocation(
                    &invocation,
                    &call.provider,
                    &boundary,
                    &runtime,
                    clock.now_ms(),
                )?;
                // A durable start marker precedes dispatch. If the process dies
                // or Host cannot return a receipt, this event stays reserved.
                self.validate_journal_ack(reservation.journal.append(
                    &event_key,
                    &json!({
                        "kind": "invocation_started",
                        "invocation_id": invocation["invocation_id"],
                        "invocation_digest": canonical::document_digest(&invocation)?,
                        "plan_ref": invocation["plan_ref"]
                    }),
                )?)?;
                // Durable storage can consume the remaining deadline or overlap
                // cancellation. Re-admit at actual dispatch, and keep fsync time
                // out of the Host's per-call wall-time measurement.
                if cancellation.is_cancelled() {
                    cancelled = true;
                    break;
                }
                let call_start = clock.now_ms();
                self.registry.validate_invocation(
                    &invocation,
                    &call.provider,
                    &boundary,
                    &runtime,
                    call_start,
                )?;
                if host.descriptor(provider_id) != Some(&call.provider) {
                    return Err(Error::ProviderChanged);
                }
                let result = host.invoke(&invocation)?;
                let call_end = clock.now_ms();
                self.registry.validate_result(
                    &invocation,
                    &result.receipt,
                    result.output.as_ref(),
                )?;
                let receipt_start = result.receipt["started_at_ms"].as_u64();
                let receipt_end = result.receipt["completed_at_ms"].as_u64();
                if call_end < call_start
                    || receipt_start.is_none_or(|t| t < call_start)
                    || receipt_end.is_none_or(|t| t > call_end)
                    || (result.receipt["disposition"] == "produced"
                        && (invocation["deadline_at_ms"]
                            .as_u64()
                            .is_none_or(|t| call_end > t)
                            || invocation["budget"]["wall_time_ms"]
                                .as_u64()
                                .is_none_or(|t| call_end - call_start > t)))
                {
                    return Err(Error::HostTime);
                }
                self.validate_journal_ack(reservation.journal.append(
                    &event_key,
                    &json!({
                        "kind": "invocation_settled",
                        "invocation_digest": canonical::document_digest(&invocation)?,
                        "receipt": result.receipt
                    }),
                )?)?;
                references.push(json!({
                    "invocation_id": invocation["invocation_id"],
                    "receipt_digest": canonical::document_digest(&result.receipt)?
                }));
                gap |= result.receipt["disposition"] != "produced";
                if step["capability"] == "security.command.inspect/v2" {
                    rejected |= result
                        .output
                        .as_ref()
                        .is_some_and(|o| o["decision"]["verdict"] != "allow");
                }
                calls.push(CallRecord { invocation, result });
                // Even the final Host call may have overlapped cancellation;
                // its receipt remains recorded but must not release a candidate.
                cancelled = cancellation.is_cancelled();
            }
            sequence += 1;
            let mut entry = json!({
                "step_id": step["step_id"],
                "outcome": if cancelled { "cancelled" } else if gap { "gap" } else { "completed" },
                "started_sequence": started, "settled_sequence": sequence,
                "invocations": references
            });
            if cancelled {
                entry["reason"] = json!("cancellation_requested");
                decision = "cancelled";
            } else {
                if gap {
                    entry["reason"] = json!("provider_result_unavailable");
                }
                if rejected {
                    decision = "deny";
                } else if gap {
                    decision = match step["on_failure"].as_str() {
                        Some("record_gap_and_continue") => "proceed",
                        Some("deny_dispatch") => "deny",
                        Some("reject_plan") if plan["boundary"] == "pre_tool" => "deny",
                        Some("reject_plan") => "preserve",
                        _ => return Err(Error::Preparation("unadmitted failure policy")),
                    };
                }
            }
            self.validate_journal_ack(
                reservation
                    .journal
                    .append(&event_key, &json!({"kind": "step_settled", "entry": entry}))?,
            )?;
            entries.push(entry);
        }
        let mut execution = json!({
            "plan_digest": canonical::document_digest(&plan)?,
            "steps": entries, "decision": decision, "evidence": [claim]
        });
        for field in [
            "plan_id",
            "revision",
            "event_id",
            "scope",
            "boundary_id",
            "boundary_revision",
        ] {
            execution[field] = plan[field].clone();
        }
        let evidence = calls.iter().map(CallRecord::evidence).collect::<Vec<_>>();
        self.registry
            .validate_plan_execution(&plan, &execution, &evidence, &boundary)?;
        let journal_ack = self.validate_journal_ack(reservation.journal.append(
            &event_key,
            &json!({
                "kind": "execution_settled", "execution": execution
            }),
        )?)?;
        Ok(Execution {
            plan,
            boundary,
            execution,
            calls,
            journal_ack,
        })
    }

    fn validate_journal_ack(&self, acknowledgement: Value) -> Result<Value, Error> {
        // Shape validation cannot establish storage durability; the Journal owns it.
        self.registry.validate_evidence(&acknowledgement)?;
        Ok(acknowledgement)
    }
}

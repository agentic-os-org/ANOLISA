//! Admit and dispatch only the obligation actions explicitly declared by local policy.

use crate::{check, CallRecord, Error, Event, Failure, Host, Method, ObligationUpdate, StepTrace};
use aw_config::requirements::{active_obligation_routes, configured_step};
use aw_core::admission::{AdmittedStep, StepExecution};
use aw_provider::{Outcome, ProviderEvidence, Reply, VERSION};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Authority supplied by the embedding runtime after its session and task admission.
pub struct BindingContext {
    /// Session identifier scoped to the authenticated runtime instance.
    pub session_id: String,
    /// Authenticated runtime instance identifier.
    pub instance_id: String,
    /// Administrator-assigned workspace identifier.
    pub workspace_id: String,
    /// Version of the admitted task scope or native request reservation.
    pub task_version: String,
}

impl BindingContext {
    pub(super) fn envelope(&self, event_id: &str, event: &Value, revision: &str) -> Value {
        // An adopted input rewrite changes the candidate, while identical calls
        // for this event share one candidate identity across Provider operations.
        let candidate = json!([
            event_id,
            event["tool"]["input"],
            event["tool"]["result"],
            event["input"]
        ]);
        json!({
            "session_id": self.session_id,
            "instance_id": self.instance_id,
            "workspace_id": self.workspace_id,
            "task_version": self.task_version,
            "config_revision": revision,
            "candidate_id": format!("{:x}", Sha256::digest(candidate.to_string().as_bytes())),
        })
    }
}

pub(super) fn step_configuration<'a>(
    document: &'a Value,
    step: &AdmittedStep,
) -> Result<&'a Value, Error> {
    configured_step(document, &step.event, &step.step_id)
        .ok_or(Error::Invalid("admitted step configuration is missing"))
}

fn operation<'a>(
    evidence: &'a BTreeMap<String, ProviderEvidence>,
    provider: &str,
    operation: &str,
) -> Option<&'a Value> {
    evidence.get(provider)?.description.as_value()["operations"]
        .as_array()?
        .iter()
        .find(|op| op["name"] == operation)
}

pub(super) fn route_providers(step: &Value) -> impl Iterator<Item = &str> {
    active_obligation_routes(step).filter_map(|(_, route)| route["provider"].as_str())
}

pub(super) fn admit_routes(
    host: &aw_core::admission::AdmittedPolicy,
    evidence: &BTreeMap<String, ProviderEvidence>,
) -> Result<(), Error> {
    let document = host.configuration().as_value();
    for step in host.steps() {
        let configured = step_configuration(document, step)?;
        for (kind, route) in active_obligation_routes(configured) {
            let action = route["provider"]
                .as_str()
                .zip(route["operation"].as_str())
                .and_then(|(provider, name)| operation(evidence, provider, name))
                .ok_or(Error::Invalid(
                    "obligation executor operation is unavailable",
                ))?;
            let contains = |value: &Value, expected: &str| {
                value
                    .as_array()
                    .is_some_and(|values| values.iter().any(|value| value == expected))
            };
            if action["decision"] == true
                || !contains(&action["obligation_kinds"], kind)
                || !contains(&action["events"], &step.event)
                || action["event_adapters"]
                    .get(&step.event)
                    .is_some_and(|adapters| !contains(adapters, &host.capabilities().adapter))
            {
                return Err(Error::Invalid(
                    "obligation executor does not support the configured route",
                ));
            }
        }
    }
    Ok(())
}

impl Host {
    pub(super) fn sends_binding(&self, step: &AdmittedStep) -> bool {
        let StepExecution::Provider {
            operation: name, ..
        } = &step.execution
        else {
            return false;
        };
        operation(self.policy().providers(), &step.provider, name)
            .is_some_and(|op| op["binding"] == true || op["decision"] == true)
    }
}

impl Event<'_> {
    pub(super) fn settle_decision(
        &self,
        step: &AdmittedStep,
        configured: &Value,
        outcome: &mut Outcome,
        binding: &Value,
        trace: &mut StepTrace,
    ) -> Result<(), Failure> {
        let shadow = configured["mode"] == "shadow";
        let reply = outcome.as_value();
        trace.decision = match reply["decision"].as_str() {
            Some("allow") => Some("allow"),
            Some("block") => Some("block"),
            Some("ask") => Some("ask"),
            Some("abstain") => Some("abstain"),
            _ => None,
        };
        let obligations = reply["obligations"]
            .as_array()
            .map_or(&[][..], Vec::as_slice);
        let routes = &configured["obligations"];
        for obligation in obligations {
            let (kind, _) = obligation_route(obligation, routes);
            trace.obligations.push(kind.to_owned());
        }
        if shadow {
            outcome.observe_only();
            return Ok(());
        }
        if trace.decision == Some("abstain") {
            return Err(Failure::Decision("decision_abstained"));
        }
        // Reject an unknown request before running any configured executor.
        if obligations
            .iter()
            .any(|value| obligation_route(value, routes).1.is_null())
        {
            return Err(Failure::Decision("obligation_route_unavailable"));
        }
        let mut scheduled: Vec<_> = obligations
            .iter()
            .map(|obligation| {
                let (kind, route) = obligation_route(obligation, routes);
                (kind, route, obligation)
            })
            .collect();
        // The arbiter chooses a set of requested protections. The admitted
        // configuration supplies their order, with a stable tie-break by kind.
        scheduled.sort_unstable_by_key(|(kind, route, _)| {
            (route["order"].as_f64().unwrap_or_default() as u16, *kind)
        });
        for (kind, route, obligation) in scheduled {
            self.check_binding()?;
            let result = self.execute_obligation(step, (kind, route), obligation, binding)?;
            let status = obligation_status(&result.1);
            trace.actions.push(crate::ObligationRecord {
                kind: kind.to_owned(),
                record: result.0,
                status,
            });
            result.1?;
        }
        self.check_binding()?;
        check(self.deadline, self.cancelled)?;
        outcome.apply_decision();
        Ok(())
    }

    fn execute_obligation(
        &self,
        step: &AdmittedStep,
        (kind, route): (&str, &Value),
        obligation: &Value,
        binding: &Value,
    ) -> Result<(CallRecord, Result<(), Failure>), Failure> {
        let (provider_id, operation) = route["provider"]
            .as_str()
            .zip(route["operation"].as_str())
            .ok_or(Failure::Decision("obligation_route_unavailable"))?;
        let provider = self
            .host
            .providers
            .get(provider_id)
            .ok_or(Failure::Decision("obligation_executor_unavailable"))?;
        let mut record = self
            .host
            .record(
                provider_id,
                Method::ExecuteObligation,
                Some(&self.id),
                Some(&step.step_id),
            )
            .map_err(|_| Failure::Decision("obligation_identity_unavailable"))?;
        let candidate_id = binding["candidate_id"]
            .as_str()
            .ok_or(Failure::Decision("obligation_identity_unavailable"))?;
        let identity = json!(["aw-obligation/v1", binding, step.step_id, kind]);
        record.request_id = format!("{:x}", Sha256::digest(identity.to_string().as_bytes()));
        let audit = self
            .obligation_audit
            .ok_or(Failure::Decision("obligation_audit_unavailable"))?;
        let persist = |record: &CallRecord, status: &'static str| {
            audit(ObligationUpdate {
                kind,
                operation,
                candidate_id,
                record,
                status,
            })
            .then_some(())
            .ok_or(Failure::Decision("obligation_audit_unavailable"))
        };
        persist(&record, "pending")?;
        // The synchronous journal write can consume the remaining authority or budget.
        let (record, result) = if let Err(error) = self.check_binding() {
            (record, Err(error))
        } else {
            let request_id = record.request_id.clone();
            let transport = self.host.transport(provider, self.deadline, self.cancelled);
            let exchange = transport.exchange(record, |budget_ms| {
                self.host.protocol.bind_obligation(json!({
                    "api_version": VERSION, "method": "execute_obligation",
                    "request_id": request_id, "operation": operation,
                    "config_revision": self.host.revision(), "budget_ms": budget_ms,
                    "config": provider.config, "event_name": self.gate.event(),
                    "binding": binding, "obligation": obligation,
                }))
            });
            let result = exchange.result.and_then(|reply| match reply {
                Reply::Invocation(outcome)
                    if outcome.as_value()["obligation_status"] == "completed" =>
                {
                    Ok(())
                }
                _ => Err(Failure::Decision("obligation_not_completed")),
            });
            (exchange.record, result)
        };
        persist(&record, obligation_status(&result))?;
        Ok((record, result))
    }
}

fn obligation_status(result: &Result<(), Failure>) -> &'static str {
    match result {
        Ok(()) => "completed",
        Err(Failure::Decision(code)) => code,
        Err(Failure::Transport(aw_exec::Error::DeadlineExceeded)) => "deadline_exceeded",
        Err(Failure::Transport(aw_exec::Error::Cancelled)) => "cancelled",
        Err(Failure::Transport(aw_exec::Error::Cleanup { .. })) => "cleanup",
        Err(Failure::Transport(_)) => "transport",
        Err(Failure::Exit) => "exit",
        Err(Failure::IncompleteInput) => "incomplete_input",
        Err(Failure::Provider { .. }) => "provider",
        Err(Failure::Protocol(_)) => "protocol",
        Err(Failure::NativeEnvironment(_)) => "native_environment",
    }
}

fn obligation_route<'a>(obligation: &Value, routes: &'a Value) -> (&'a str, &'a Value) {
    obligation["kind"]
        .as_str()
        .and_then(|kind| routes.as_object()?.get_key_value(kind))
        .map_or(("unmapped", &Value::Null), |(kind, route)| {
            (kind.as_str(), route)
        })
}

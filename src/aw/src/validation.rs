//! Cross-record checks for the bundled capability profiles.
//!
//! These checks consume trusted observations. They do not authenticate a
//! provider, inspect a process, run a decoder, dispatch a tool or commit a ledger.

use crate::{canonical, require, Error, Registry};
use serde_json::Value;
use std::collections::BTreeSet;

pub(crate) fn string(value: &Value) -> Result<&str, Error> {
    value.as_str().ok_or(Error::Invariant("expected string"))
}
pub(crate) fn number(value: &Value) -> Result<u64, Error> {
    value
        .as_u64()
        .ok_or(Error::Invariant("expected unsigned integer"))
}
pub(crate) fn array(value: &Value) -> Result<&Vec<Value>, Error> {
    value.as_array().ok_or(Error::Invariant("expected array"))
}
pub(crate) fn same(left: &Value, right: &Value, fields: &[&str]) -> Result<(), Error> {
    require(
        fields.iter().all(|field| left[*field] == right[*field]),
        "record identity mismatch",
    )
}
pub(crate) fn content_digest(value: &Value) -> Result<(), Error> {
    require(
        value["digest"] == canonical::digest(string(&value["content"])?.as_bytes()),
        "content digest mismatch",
    )
}
pub(crate) fn profile(capability: &Value) -> Result<&'static str, Error> {
    match string(capability)? {
        "context.projection.prepare/v2" => Ok("context-projection-prepare"),
        "security.content.inspect/v2" => Ok("security-content-inspect"),
        "security.code.inspect/v2" => Ok("security-code-inspect"),
        "security.command.inspect/v2" => Ok("security-command-inspect"),
        _ => Err(Error::Invariant("unsupported capability profile")),
    }
}

/// Evidence supplied by the adapter at one explicitly named adoption boundary.
///
/// The caller must independently obtain the effective and recovered text; a
/// provider echoing these fields does not constitute observation or recovery.
pub struct AdoptionCheck<'a> {
    /// Previously admitted invocation.
    pub invocation: &'a Value,
    /// Provider receipt, verified against its output by this check.
    pub receipt: &'a Value,
    /// Candidate envelope, absent when the provider produced no candidate.
    pub output: Option<&'a Value>,
    /// Environment-owned observation record.
    pub observation: &'a Value,
    /// Trusted descriptor for the native boundary.
    pub boundary: &'a Value,
    /// Exact text observed at the named boundary; absent when unverified.
    pub effective_text: Option<&'a str>,
    /// Exact text obtained independently from the declared decoder or resolver.
    pub recovered_text: Option<&'a str>,
    /// Current Unix epoch milliseconds from the caller's trusted clock.
    pub now_ms: u64,
}

impl Registry {
    /// Checks whether a boundary's declared powers are internally consistent.
    ///
    /// # Errors
    /// Rejects observation-only mutation, unsupported gating and impossible
    /// synchronous delivery claims. This does not test the native hook itself.
    pub fn validate_boundary(&self, boundary: &Value) -> Result<(), Error> {
        self.validate("boundary-descriptor-v1", boundary)?;
        let observe = boundary["invocation_mode"] == "observe_only";
        require(
            !observe
                || (boundary["can_replace_text"] == false
                    && boundary["can_deny_dispatch"] == false
                    && boundary["has_final_input_guard"] == false
                    && boundary["ledger_policy"] == "best_effort"),
            "observer cannot promise mutation or delivery ordering",
        )?;
        require(
            boundary["can_deny_dispatch"] != true
                || (boundary["boundary"] == "pre_tool"
                    && boundary["has_final_input_guard"] == true),
            "dispatch denial requires a final input guard",
        )?;
        require(
            boundary["has_final_input_guard"] != true || boundary["boundary"] == "pre_tool",
            "input guard requires pre-tool boundary",
        )?;
        let composition = &boundary["composition"];
        require(
            (boundary["has_final_input_guard"] == true)
                == (composition["gate"] == "required_final_guard"),
            "final guard declaration mismatch",
        )?;
        require(
            composition["gate"] != "required_final_guard"
                || composition["input_finality"] != "uncontrolled",
            "final guard cannot bind uncontrolled input",
        )?;
        require(
            !array(&boundary["proof_boundaries"])?
                .contains(&Value::String("final_tool_result".into()))
                || composition["result_finality"] == "final",
            "final tool proof requires final result boundary",
        )?;
        require(
            boundary["ledger_policy"] != "required_before_delivery"
                || !array(&boundary["proof_boundaries"])?.is_empty(),
            "required ledger needs an observable delivery boundary",
        )
    }

    /// Admits a supported invocation against trusted provider and runtime state.
    ///
    /// # Errors
    /// Rejects stale bindings, mismatched schema resources, unsupported powers,
    /// expired deadlines and inconsistent input bytes. Authentication, admission
    /// serialization and resource budget enforcement remain caller duties.
    pub fn validate_invocation(
        &self,
        invocation: &Value,
        provider: &Value,
        boundary: &Value,
        runtime: &Value,
        now_ms: u64,
    ) -> Result<(), Error> {
        self.validate("capability-invocation-v1", invocation)?;
        self.validate("provider-descriptor-v1", provider)?;
        self.validate("runtime-binding-v1", runtime)?;
        self.validate_boundary(boundary)?;
        self.validate_input(invocation)?;
        same(
            invocation,
            provider,
            &["provider_id", "provider_version", "manifest_digest"],
        )?;
        let scope = &invocation["scope"];
        same(
            scope,
            runtime,
            &["runtime_id", "binding_revision", "environment_id"],
        )?;
        if runtime.get("session_id").is_some() {
            same(scope, runtime, &["session_id"])?;
        }
        require(
            scope["runtime_generation"] == runtime["generation"] && runtime["state"] == "running",
            "runtime binding is not current and running",
        )?;
        require(
            invocation["boundary_id"] == boundary["boundary_id"]
                && invocation["boundary_revision"] == boundary["revision"],
            "boundary revision mismatch",
        )?;
        require(
            number(&invocation["deadline_at_ms"])? > now_ms,
            "invocation deadline expired",
        )?;
        let input = &invocation["input"];
        require(
            input["boundary"] == boundary["boundary"],
            "native boundary mismatch",
        )?;
        if matches!(string(&input["boundary"])?, "pre_tool" | "post_tool") {
            require(
                scope.get("tool_use_id").is_some() && scope.get("turn_id").is_some(),
                "tool boundary requires native call scope",
            )?;
        }
        let routes = array(&provider["capabilities"])?;
        let mut names = BTreeSet::new();
        require(
            routes
                .iter()
                .all(|r| names.insert(r["capability"].to_string())),
            "duplicate capability route",
        )?;
        let route = routes
            .iter()
            .find(|r| r["capability"] == invocation["capability"])
            .ok_or(Error::Invariant("provider does not advertise capability"))?;
        same(route, invocation, &["input_schema", "output_schema"])?;
        require(
            array(&route["boundaries"])?.contains(&boundary["boundary"]),
            "provider does not support boundary",
        )?;
        if invocation["capability"] == "context.projection.prepare/v2" {
            require(
                boundary["can_replace_text"] == true,
                "projection requires a replaceable text slot",
            )?;
            require(
                array(&boundary["media_types"])?.contains(&input["artifact"]["media_type"]),
                "unsupported input media type",
            )?;
            require(
                array(&input["constraints"]["accepted_reversibility"])?
                    .iter()
                    .all(|r| array(&boundary["reversibility"]).is_ok_and(|a| a.contains(r))),
                "unsupported recovery mode",
            )?;
        }
        if invocation["capability"] == "security.command.inspect/v2" {
            require(
                boundary["can_deny_dispatch"] == true && boundary["has_final_input_guard"] == true,
                "command inspection requires an enforceable dispatch boundary",
            )?;
        }
        Ok(())
    }

    pub(crate) fn validate_input(&self, invocation: &Value) -> Result<(), Error> {
        self.validate("capability-invocation-v1", invocation)?;
        let name = profile(&invocation["capability"])?;
        for direction in ["input", "output"] {
            require(
                invocation[format!("{direction}_schema")]
                    == self.reference(&format!("{name}-{direction}-v2"))?,
                "schema resource mismatch",
            )?;
        }
        let input = &invocation["input"];
        self.validate(&format!("{name}-input-v2"), input)?;
        require(
            invocation["input_digest"] == canonical::document_digest(input)?,
            "input document digest mismatch",
        )?;
        require(
            canonical::bytes(input)?.len() as u64 <= number(&invocation["budget"]["input_bytes"])?,
            "input exceeds budget",
        )?;
        content_digest(if name == "security-command-inspect" {
            &input["command"]
        } else {
            &input["artifact"]
        })
    }

    /// Verifies receipt correlation, output bytes and capability-specific claims.
    ///
    /// # Errors
    /// Rejects mismatched identities, deadlines, coverage, schema revisions or
    /// meters. Matching coverage is a scanner declaration, not proof that each
    /// byte was examined. This method never establishes environment adoption.
    pub fn validate_result(
        &self,
        invocation: &Value,
        receipt: &Value,
        output: Option<&Value>,
    ) -> Result<(), Error> {
        self.validate_input(invocation)?;
        self.validate("provider-receipt-v1", receipt)?;
        same(
            invocation,
            receipt,
            &[
                "invocation_id",
                "provider_id",
                "provider_version",
                "manifest_digest",
                "capability",
                "scope",
                "input_schema",
                "input_digest",
                "plan_ref",
            ],
        )?;
        require(
            receipt["disposition"] != "effect_applied",
            "bundled capabilities cannot perform effects",
        )?;
        let start = number(&receipt["started_at_ms"])?;
        let end = number(&receipt["completed_at_ms"])?;
        require(start <= end, "receipt time order mismatch")?;
        // Late failures remain useful evidence. Only successful results are usable.
        if receipt["disposition"] == "produced" {
            require(
                end <= number(&invocation["deadline_at_ms"])?
                    && end - start <= number(&invocation["budget"]["wall_time_ms"])?,
                "successful result exceeded time budget",
            )?;
        }
        let mut meters = BTreeSet::new();
        require(
            array(&receipt["meters"])?
                .iter()
                .all(|m| meters.insert(m["meter_id"].to_string())),
            "duplicate meter identity",
        )?;
        require(
            output.is_some() == receipt.get("output").is_some(),
            "output presence mismatch",
        )?;
        if let Some(output) = output {
            let name = profile(&invocation["capability"])?;
            self.validate(&format!("{name}-output-v2"), output)?;
            let bytes = canonical::bytes(output)?;
            require(
                receipt["output"]["schema"] == invocation["output_schema"]
                    && receipt["output"]["digest"] == canonical::digest(&bytes),
                "output schema or digest mismatch",
            )?;
            require(
                number(&receipt["output"]["bytes"])? == bytes.len() as u64
                    && bytes.len() as u64 <= number(&invocation["budget"]["output_bytes"])?,
                "output byte budget mismatch",
            )?;
            if name == "context-projection-prepare" {
                projection_identity(&invocation["input"], output)?;
            } else {
                inspect_result(&invocation["input"], output, name)?;
            }
        }
        Ok(())
    }

    /// Checks one observation and returns attributable byte savings, if known.
    ///
    /// Use `validate_plan_adoption` to also enforce complete plan execution; this
    /// lower-level method establishes only single-invocation consistency.
    ///
    /// `None` means unverified or unavailable ledger. Zero means a verified
    /// observation with no attributable savings. Deduplicate by invocation ID,
    /// proof boundary and representation revision before aggregating results.
    ///
    /// # Errors
    /// Rejects unsupported proof, mismatched text, invalid recovery or false
    /// adoption. Ledger durability and evidence authenticity must be established
    /// by the caller; a record claiming `committed` is not a storage receipt.
    pub fn validate_adoption(&self, check: AdoptionCheck<'_>) -> Result<Option<u64>, Error> {
        let AdoptionCheck {
            invocation,
            receipt,
            output,
            observation: obs,
            boundary,
            effective_text,
            recovered_text,
            now_ms,
        } = check;
        self.validate_result(invocation, receipt, output)?;
        require(
            invocation["capability"] == "context.projection.prepare/v2",
            "adoption requires projection capability",
        )?;
        self.validate_boundary(boundary)?;
        self.validate("context-adoption-v1", obs)?;
        same(
            invocation,
            obs,
            &["invocation_id", "scope", "boundary_id", "boundary_revision"],
        )?;
        require(
            obs["boundary_id"] == boundary["boundary_id"]
                && obs["boundary_revision"] == boundary["revision"]
                && boundary["can_replace_text"] == true,
            "observation boundary mismatch",
        )?;
        require(
            obs["receipt_digest"] == canonical::document_digest(receipt)?,
            "receipt digest mismatch",
        )?;
        let source = &invocation["input"]["artifact"];
        require(
            obs["source_digest"] == source["digest"],
            "source digest mismatch",
        )?;
        require(
            obs.get("candidate_digest").is_some() == output.is_some(),
            "candidate observation presence mismatch",
        )?;
        if output.is_some() {
            require(
                obs["candidate_digest"] == receipt["output"]["digest"],
                "candidate envelope digest mismatch",
            )?;
        }
        if obs["decision"] == "unverified" {
            require(
                effective_text.is_none(),
                "unverified observation cannot supply effective text",
            )?;
            require(
                obs["reason"] != "ledger_unavailable" || obs["ledger_status"] == "unavailable",
                "ledger failure reason mismatch",
            )?;
            return Ok(None);
        }
        require(
            array(&boundary["proof_boundaries"])?.contains(&obs["proof"]["boundary"]),
            "unsupported proof boundary",
        )?;
        let observed = number(&obs["proof"]["observed_at_ms"])?;
        require(
            number(&receipt["completed_at_ms"])? <= observed && observed <= now_ms,
            "observation time mismatch",
        )?;
        let effective =
            effective_text.ok_or(Error::Invariant("missing independently observed text"))?;
        require(
            obs["effective_digest"] == canonical::digest(effective.as_bytes())
                && number(&obs["effective_bytes"])? == effective.len() as u64,
            "effective text mismatch",
        )?;
        let savings = match string(&obs["decision"])? {
            "adopted" => {
                let output = output.ok_or(Error::Invariant("adoption without candidate"))?;
                let candidate = &output["candidate"];
                require(
                    effective == string(&candidate["content"])? && !effective.is_empty(),
                    "adopted text differs from nonempty candidate",
                )?;
                require(
                    array(&boundary["media_types"])?.contains(&candidate["media_type"]),
                    "unsupported candidate media type",
                )?;
                recovery(&invocation["input"], output, recovered_text, observed)?;
                require(
                    boundary["ledger_policy"] != "required_before_delivery"
                        || obs["ledger_status"] == "committed",
                    "required ledger unavailable at delivery",
                )?;
                string(&source["content"])?
                    .len()
                    .saturating_sub(effective.len()) as u64
            }
            "preserved" => {
                require(
                    effective == string(&source["content"])?,
                    "preserved text differs from source",
                )?;
                let reason = string(&obs["reason"])?;
                require(
                    matches!(
                        reason,
                        "no_candidate"
                            | "no_savings"
                            | "recovery_unavailable"
                            | "environment_rejected"
                            | "cancelled"
                            | "ledger_unavailable"
                    ),
                    "invalid preservation reason",
                )?;
                require(
                    (reason == "no_candidate") == output.is_none()
                        || reason == "cancelled"
                        || reason == "ledger_unavailable",
                    "preservation reason does not match candidate",
                )?;
                if reason == "no_savings" {
                    require(
                        output.is_some_and(|o| {
                            o["candidate"]["content"]
                                .as_str()
                                .is_some_and(|s| s.len() >= effective.len())
                        }),
                        "no-savings reason contradicts byte lengths",
                    )?;
                }
                require(
                    reason != "ledger_unavailable" || obs["ledger_status"] == "unavailable",
                    "ledger failure reason mismatch",
                )?;
                0
            }
            "overridden" => {
                require(
                    obs["reason"] == "later_transform",
                    "override requires later transformation reason",
                )?;
                0
            }
            _ => return Err(Error::Invariant("unsupported observation decision")),
        };
        Ok((obs["ledger_status"] == "committed").then_some(savings))
    }

    /// Checks one inspection against an unchanged execution intent.
    ///
    /// This is a consistency check, not complete dispatch admission. Use
    /// `validate_dispatch` for the required plan and OS protection checks.
    ///
    /// # Errors
    /// Rejects expired, altered or non-allow decisions. The adapter must capture
    /// the final intent and extracted command atomically with dispatch; it must
    /// bind the intent scope to the admitted invocation. This performs no action.
    pub fn validate_execution_gate(
        &self,
        invocation: &Value,
        output: &Value,
        intent: &Value,
        current_intent: &Value,
        now_ms: u64,
    ) -> Result<(), Error> {
        self.validate_input(invocation)?;
        require(
            invocation["capability"] == "security.command.inspect/v2",
            "gate requires command capability",
        )?;
        let input = &invocation["input"];
        self.validate("security-command-inspect-output-v2", output)?;
        self.validate("execution-intent-v1", intent)?;
        self.validate("execution-intent-v1", current_intent)?;
        content_digest(&input["command"])?;
        content_digest(&intent["arguments"])?;
        same(intent, invocation, &["scope"])?;
        require(
            intent == current_intent && number(&intent["expires_at_ms"])? > now_ms,
            "execution intent changed or expired",
        )?;
        require(
            input["execution_intent_digest"] == canonical::document_digest(intent)?,
            "execution intent digest mismatch",
        )?;
        inspect_result(input, output, "security-command-inspect")?;
        require(
            output["decision"]["verdict"] == "allow",
            "inspection does not allow dispatch",
        )
    }

    /// Checks a control grant against a trusted, current runtime binding.
    ///
    /// # Errors
    /// Rejects wrong owners, holders, generations, actions or expiry. The caller
    /// must authenticate the issuer and serialize this check with the action.
    pub fn validate_control(
        &self,
        grant: &Value,
        runtime: &Value,
        holder: &str,
        action: &str,
        now_ms: u64,
    ) -> Result<(), Error> {
        self.validate("control-grant-v1", grant)?;
        self.validate("runtime-binding-v1", runtime)?;
        same(grant, runtime, &["runtime_id", "binding_revision"])?;
        require(
            grant["runtime_generation"] == runtime["generation"]
                && grant["issuer_id"] == runtime["owner_id"]
                && grant["holder_id"] == holder,
            "control authority mismatch",
        )?;
        require(
            number(&grant["expires_at_ms"])? > now_ms
                && array(&grant["actions"])?.contains(&Value::String(action.into())),
            "control grant expired or action absent",
        )?;
        require(
            runtime["state"] != "detached"
                && ((action == "stop" && runtime["state"] == "running")
                    || (action == "resume" && runtime["state"] == "suspended")),
            "control action incompatible with runtime state",
        )
    }

    /// Validates a durable operation journal transition without executing it.
    ///
    /// # Errors
    /// Rejects identity mutation, skipped approval, invalid sequence and retries
    /// from uncertainty. Identical records are valid idempotent reads, never
    /// authorization to repeat an effect. Storage must enforce compare-and-swap.
    pub fn validate_operation_transition(
        &self,
        previous: &Value,
        next: &Value,
    ) -> Result<(), Error> {
        self.validate("operation-record-v1", previous)?;
        self.validate("operation-record-v1", next)?;
        same(
            previous,
            next,
            &[
                "operation_id",
                "capability",
                "scope",
                "resource_id",
                "resource_generation",
                "expected_revision",
                "input_digest",
                "idempotency_key",
            ],
        )?;
        if previous == next {
            return Ok(());
        }
        require(
            number(&previous["sequence"])? + 1 == number(&next["sequence"])?,
            "operation sequence must advance once",
        )?;
        if previous.get("approval_ref").is_some() {
            same(previous, next, &["approval_ref"])?;
        }
        let from = string(&previous["state"])?;
        let to = string(&next["state"])?;
        require(
            matches!(
                (from, to),
                ("prepared", "approved" | "no_effect")
                    | ("approved", "started" | "no_effect")
                    | ("started", "succeeded" | "no_effect" | "uncertain")
                    | ("uncertain", "succeeded" | "no_effect" | "uncertain")
            ),
            "invalid operation transition",
        )
    }
}

fn projection_identity(input: &Value, output: &Value) -> Result<(), Error> {
    let source = &input["artifact"];
    let candidate = &output["candidate"];
    require(
        candidate["source_artifact_id"] == source["id"]
            && candidate["source_digest"] == source["digest"],
        "candidate source mismatch",
    )?;
    require(
        input["constraints"]["allow_text_reencoding"] == true
            || candidate["media_type"] == source["media_type"],
        "text reencoding not permitted",
    )?;
    require(
        array(&input["constraints"]["accepted_reversibility"])?
            .contains(&candidate["reversibility"]),
        "reversibility not accepted",
    )?;
    if candidate["recovery"]["mode"] == "external" {
        require(
            candidate["recovery"]["source_digest"] == source["digest"],
            "resolver source mismatch",
        )?;
    }
    Ok(())
}
fn recovery(
    input: &Value,
    output: &Value,
    recovered: Option<&str>,
    observed_ms: u64,
) -> Result<(), Error> {
    let candidate = &output["candidate"];
    if candidate["reversibility"] != "unrecoverable" {
        require(
            recovered == input["artifact"]["content"].as_str(),
            "independent recovery did not reproduce source",
        )?;
    }
    if candidate["recovery"]["mode"] == "external" {
        require(
            number(&candidate["recovery"]["expires_at_ms"])? > observed_ms,
            "recovery reference expired at adoption",
        )?;
    }
    Ok(())
}
fn inspect_result(input: &Value, output: &Value, profile: &str) -> Result<(), Error> {
    let command = profile == "security-command-inspect";
    let artifact = if command {
        &input["command"]
    } else {
        &input["artifact"]
    };
    let result = if command {
        &output["decision"]
    } else {
        &output["inspection"]
    };
    let coverage = &result["coverage"];
    let input_bytes = string(&artifact["content"])?.len() as u64;
    let scanned = number(&coverage["scanned_bytes"])?;
    require(
        coverage["input_digest"] == artifact["digest"]
            && number(&coverage["input_bytes"])? == input_bytes,
        "coverage input mismatch",
    )?;
    require(
        scanned <= input_bytes && (coverage["complete"] == true) == (scanned == input_bytes),
        "coverage completeness mismatch",
    )?;
    let expected: BTreeSet<&str> = if profile == "security-content-inspect" {
        BTreeSet::new()
    } else {
        let language = if command {
            string(&artifact["language"])?
        } else {
            string(&input["constraints"]["language"])?
        };
        if language == "auto" {
            BTreeSet::from(["bash", "python"])
        } else {
            BTreeSet::from([language])
        }
    };
    let actual = array(&coverage["languages"])?
        .iter()
        .map(string)
        .collect::<Result<BTreeSet<_>, _>>()?;
    require(expected == actual, "scanner language coverage mismatch")?;
    if command {
        require(
            input["execution_intent_digest"] == result["execution_intent_digest"],
            "inspection intent mismatch",
        )?;
    }
    Ok(())
}

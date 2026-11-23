//! Side-effect-free Hook Source resolution over projected requirements and capabilities.
//! Qualifying [`SourceKind::Native`] Sources win first, each Hook point selects at most one
//! [`HookAuthority::Control`] owner, and auxiliary observers never duplicate step dispatch.

mod configuration;

pub use configuration::resolve_configuration;
use configuration::{provider_protocol, validate_boolean_effects};

use crate::admission::{AdapterCapabilities, AdmissionError, AdmittedStep, StepExecution};
use aw_config::{
    requirements::{HookRequirement, StepRequirement},
    Configuration,
};
pub use aw_contracts::{
    hook_plan::{
        Coverage, CoverageGap, CoverageGapReason, CoverageGapStatus, ResolvedHookPlan,
        ResolvedHookPoint, ResolvedStepPlan, SatisfiedHookCoverage, SelectedSource,
    },
    source::{
        event_supports_effect, event_supports_failure_action, is_hook_event,
        project_capability_cells, CapabilityCell, CapabilityStatus, FailureAction, HookAuthority,
        HookEffect, HookPointCapability, SourceCapability, SourceKind, HOOK_EVENTS,
    },
};
use aw_provider::{EventGate, MutationSlot};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Maximum number of candidate Sources accepted for one target resolution.
pub const MAX_CANDIDATE_SOURCES: usize = 16;

const ERR_EFF: &str = "requested effect is not supported by the implementation and Adapter";
const ERR_ACT: &str = "failure action is not supported by the implementation and Adapter";
const ERR_EVT: &str = "Adapter evidence lacks an enabled event";
const ERR_SURF: &str = "Adapter evidence exceeds the supported event or effect surface";
const ERR_ACT_SURF: &str = "Adapter evidence exceeds the supported failure action surface";
const ERR_VER_EP: &str = "Adapter evidence requires a version and entrypoint";
const ERR_TARGET_MISMATCH: &str = "Adapter evidence does not match the configured target";
const ERR_SRC_ADAPTER: &str = "candidate source adapter does not match target adapter";
const ERR_REPLACE_RES: &str = "result replacement must be the last applicable enabled event step";
const ERR_REPLACE_IN: &str =
    "input replacement requires a first sole-effect step and fail-closed before steps";

/// Pure Hook Source resolution failures without configuration values in diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// Desired configuration or target requirement projection failed.
    #[error(transparent)]
    Configuration(#[from] aw_config::Error),
    /// A Source capability or resolved Hook plan violated a contract schema or invariant.
    #[error(transparent)]
    Contract(#[from] aw_contracts::Error),
    /// Target or adapter identifier is invalid or mismatched.
    #[error("invalid resolution target or adapter: {0}")]
    Target(&'static str),
    /// Candidate Source set contains duplicate identities, capacity overflow or foreign adapters.
    #[error("invalid candidate source set: {0}")]
    SourceSet(&'static str),
    /// Desired Hook requirement slice violates structural step or event rules.
    #[error("invalid Hook requirement: {0}")]
    Requirement(&'static str),
    /// A required Hook event, effect, failure action or native step has no qualifying Source.
    #[error("unsatisfied required Hook {event}: {reason:?}")]
    UnsatisfiedRequired {
        /// Canonical semantic Hook event name that could not be satisfied.
        event: String,
        /// Machine-readable reason for the resolution failure.
        reason: CoverageGapReason,
        /// Specific requested step effects missing from candidate Sources.
        missing_effects: Vec<HookEffect>,
        /// Specific requested step failure actions missing from candidate Sources.
        missing_failure_actions: Vec<FailureAction>,
    },
}

use AdmissionError::Rejected as Reject;
use ResolveError::{Requirement as BadReq, SourceSet as BadSrc, Target as BadTarget};

type GapDiagnosis = (CoverageGapReason, Vec<HookEffect>, Vec<FailureAction>);

fn unsatisfied(
    ev: &str,
    (reason, missing_effects, missing_failure_actions): GapDiagnosis,
) -> ResolveError {
    let event = ev.to_owned();
    ResolveError::UnsatisfiedRequired {
        event,
        reason,
        missing_effects,
        missing_failure_actions,
    }
}

fn build_gap(
    ev: &str,
    status: CoverageGapStatus,
    d: GapDiagnosis,
    fallback_sources: Vec<String>,
) -> CoverageGap {
    let (event, required, (reason, missing_effects, missing_failure_actions)) =
        (ev.to_owned(), false, d);
    CoverageGap {
        event,
        required,
        status,
        reason,
        missing_effects,
        missing_failure_actions,
        fallback_sources,
    }
}

/// Resolves target [`HookRequirement`] entries against candidate [`SourceCapability`] declarations.
///
/// # Errors
/// Rejects invalid target/adapter names, candidate sets exceeding [`MAX_CANDIDATE_SOURCES`],
/// duplicate/foreign candidate Sources, invalid step ordering, and unsatisfied `required: true` hooks.
pub fn resolve(
    target: &str,
    adapter: &str,
    requirements: &[HookRequirement<'_>],
    sources: &[SourceCapability],
) -> Result<ResolvedHookPlan, ResolveError> {
    if !valid_identifier(target) {
        return Err(BadTarget("invalid target identifier"));
    }
    if !valid_identifier(adapter) {
        return Err(BadTarget("invalid adapter identifier"));
    }
    if sources.len() > MAX_CANDIDATE_SOURCES {
        return Err(BadSrc("candidate source set exceeds maximum capacity"));
    }
    let mut seen_sources = BTreeSet::new();
    for source in sources {
        source.validate()?;
        if source.adapter() != adapter {
            return Err(BadSrc(ERR_SRC_ADAPTER));
        }
        if !seen_sources.insert(source.source_id()) {
            return Err(BadSrc("duplicate candidate source identifier"));
        }
    }

    let mut seen_events = BTreeSet::new();
    let mut ordered: Vec<&HookRequirement<'_>> = Vec::with_capacity(requirements.len());
    for req in requirements {
        if !is_hook_event(req.event) {
            return Err(BadReq("unknown semantic Hook event"));
        }
        if !seen_events.insert(req.event) {
            return Err(BadReq("duplicate semantic Hook event in requirements"));
        }
        ordered.push(req);
    }
    ordered.sort_by_key(|req| req.event);

    let mut hooks = BTreeMap::new();
    let mut satisfied = Vec::new();
    let mut gaps = Vec::new();

    for req in ordered {
        let event = req.event;
        if req.configuration().get("guard").is_some() {
            return Err(BadReq("event guards are not supported"));
        }
        if req.budget_ms < 1 || req.budget_ms > u64::from(u32::MAX) {
            return Err(BadReq("event budget is out of range"));
        }
        if !valid_selectors(&req.event_tools) {
            return Err(BadReq("invalid event tool selectors"));
        }

        let mut steps = parse_steps(req)?;
        check_step_ordering(event, &steps, sources)?;

        let needs_native = steps.iter().any(|s| s.native);
        let mut req_fx = BTreeSet::new();
        let mut need_fx = BTreeSet::from([HookEffect::Observe]);
        let mut need_act = BTreeSet::new();
        for s in &steps {
            req_fx.extend(s.effects.iter().copied());
            need_fx.extend(s.effects.iter().copied());
            if s.on_error == FailureAction::Block {
                need_fx.insert(HookEffect::Block);
            }
            need_act.insert(s.on_error);
        }
        let needs_ctrl = needs_native
            || need_fx.iter().any(|e| e.is_control())
            || need_act.iter().any(|a| a.is_control());

        let mut cands: Vec<_> = sources
            .iter()
            .filter_map(|s| s.hook(event).map(|c| (s, c)))
            .collect();
        cands.sort_by_key(|(s, c)| (s.kind(), c.mode, s.source_id()));

        if cands.is_empty() {
            let fx: Vec<_> = need_fx.into_iter().collect();
            let act: Vec<_> = need_act.into_iter().filter(|a| a.is_control()).collect();
            let diag = (CoverageGapReason::UnsupportedEvent, fx, act);
            if req.required {
                return Err(unsatisfied(event, diag));
            }
            gaps.push(build_gap(
                event,
                CoverageGapStatus::Uncovered,
                diag,
                Vec::new(),
            ));
            continue;
        }

        let primary = cands.iter().copied().find(|(s, c)| {
            (!needs_native || s.kind() == SourceKind::Native)
                && (!needs_ctrl || c.mode == HookAuthority::Control)
                && need_fx.iter().all(|e| c.supports_effect(*e))
                && need_act.iter().all(|a| c.supports_failure_action(*a))
        });

        let Some((primary_src, primary_cap)) = primary else {
            let diag = diagnose_gap(&cands, needs_native, &req_fx, &need_act);
            if req.required {
                return Err(unsatisfied(event, diag));
            }
            let fallbacks = cands
                .iter()
                .map(|(s, _)| s.source_id().to_owned())
                .collect();
            gaps.push(build_gap(
                event,
                CoverageGapStatus::Degraded,
                diag,
                fallbacks,
            ));
            continue;
        };

        let primary_sel = to_selected(primary_src, primary_cap, true);
        let mut aux: Vec<_> = cands
            .iter()
            .filter(|(s, _)| s.source_id() != primary_src.source_id())
            .map(|(s, c)| to_selected(s, c, false))
            .collect();
        let (control_owner, observation_sources) = if primary_cap.mode == HookAuthority::Control {
            (Some(primary_sel), aux)
        } else {
            aux.insert(0, primary_sel);
            (None, aux)
        };
        for step in &mut steps {
            step.dispatch_owner = primary_src.source_id().to_owned();
        }
        satisfied.push(SatisfiedHookCoverage {
            event: event.to_owned(),
            required: req.required,
            control_owner: control_owner.as_ref().map(|o| o.source_id.clone()),
            observation_sources: observation_sources
                .iter()
                .map(|o| o.source_id.clone())
                .collect(),
            effects: need_fx.into_iter().collect(),
            failure_actions: need_act.into_iter().collect(),
        });
        let (event, required, budget_ms) = (event.to_owned(), req.required, req.budget_ms);
        let event_tools = req.event_tools.clone();
        let point = ResolvedHookPoint {
            event,
            required,
            budget_ms,
            event_tools,
            control_owner,
            observation_sources,
            steps,
        };
        hooks.insert(req.event.to_owned(), point);
    }

    let coverage = Coverage { satisfied, gaps };
    Ok(ResolvedHookPlan::new(target, adapter, hooks, coverage)?)
}

/// Converts trusted [`AdapterCapabilities`] into a canonical [`SourceKind::Native`] capability.
///
/// # Errors
/// Rejects empty adapter/version/entrypoint fields or events/effects outside the 16-Hook contract.
pub fn source_from_adapter(caps: &AdapterCapabilities) -> Result<SourceCapability, ResolveError> {
    if caps.version.trim().is_empty() || caps.entrypoint.trim().is_empty() {
        return Err(BadSrc(ERR_VER_EP));
    }
    if caps
        .failure_actions
        .keys()
        .any(|e| !caps.events.contains_key(e))
    {
        return Err(BadSrc(
            "Adapter failure action references an undeclared event",
        ));
    }
    let mut hooks = BTreeMap::new();
    for (event, raw_effects) in &caps.events {
        let mut effects = Vec::with_capacity(raw_effects.len());
        for raw in raw_effects {
            effects.push(
                HookEffect::parse_name(raw)
                    .ok_or(BadSrc("Adapter evidence contains an unknown effect"))?,
            );
        }
        let mut failure_actions = Vec::new();
        for raw in caps.failure_actions.get(event).into_iter().flatten() {
            failure_actions.push(FailureAction::parse_name(raw).ok_or(BadSrc(
                "Adapter evidence contains an unknown failure action",
            ))?);
        }
        let mode = HookAuthority::Control;
        hooks.insert(
            event.clone(),
            HookPointCapability {
                mode,
                effects,
                failure_actions,
            },
        );
    }
    let (id, kind, v, ep) = (
        &caps.adapter,
        SourceKind::Native,
        &caps.version,
        &caps.entrypoint,
    );
    Ok(SourceCapability::new(id, kind, id, v, ep, hooks)?)
}

/// Evaluates [`resolve`] as a standalone Shadow Plan for static preflight admission.
///
/// # Errors
/// Returns [`AdmissionError::Rejected`] with the exact static preflight rejection
/// reason whenever the target, capability surface, coverage or step configuration
/// is inadmissible.
pub fn resolve_shadow_preflight(
    config: &Configuration,
    target: &str,
    caps: &AdapterCapabilities,
) -> Result<(ResolvedHookPlan, Vec<AdmittedStep>), AdmissionError> {
    let spec = &config.as_value()["spec"];
    let agent = spec["agents"]
        .get(target)
        .ok_or(Reject("unknown configured Agent target"))?;
    if agent["adapter"] != caps.adapter {
        return Err(Reject(ERR_TARGET_MISMATCH));
    }
    if caps.version.trim().is_empty() || caps.entrypoint.trim().is_empty() {
        return Err(Reject(ERR_VER_EP));
    }
    for (event, effects) in &caps.events {
        if !EventGate::for_event(event).is_some_and(|g| effects.iter().all(|e| g.allows_effect(e)))
        {
            return Err(Reject(ERR_SURF));
        }
    }
    for (event, actions) in &caps.failure_actions {
        let ok_act = caps.events.contains_key(event)
            && EventGate::for_event(event).is_some_and(|g| {
                g.mutation_slot() == Some(MutationSlot::PostToolResult)
                    && actions.iter().all(|a| g.allows_extra_failure_action(a))
            });
        if !ok_act {
            return Err(Reject(ERR_ACT_SURF));
        }
    }
    if spec["execution"]["guarantee"] != "native_hook" {
        return Err(Reject("only native_hook execution can be admitted"));
    }
    let events = spec["events"]
        .as_object()
        .ok_or(Reject("configuration events must be an object"))?;
    for (name, ev) in events {
        if ev["enabled"] == true
            && EventGate::for_event(name).is_some_and(|g| !g.has_tool_payload())
        {
            let no_step_tools = ev["steps"].as_array().is_some_and(|steps| {
                steps
                    .iter()
                    .all(|s| s["enabled"] == false || s["match"].get("tools").is_none())
            });
            if ev["match"].get("tools").is_some() || !no_step_tools {
                return Err(Reject("non-tool event does not support tool selectors"));
            }
        }
    }

    let source = source_from_adapter(caps).map_err(|_| Reject(ERR_SURF))?;
    let requirements = aw_config::requirements::project(config, target)?;
    for req in &requirements {
        if EventGate::for_event(req.event).is_none() {
            return Err(Reject("enabled event is not supported"));
        }
        if source.hook(req.event).is_none() {
            return Err(Reject(ERR_EVT));
        }
    }

    let single_source = std::slice::from_ref(&source);
    let resolved = resolve(target, &caps.adapter, &requirements, single_source);
    if let Err(ResolveError::Requirement(reason)) = resolved {
        return Err(Reject(reason));
    }

    let mut steps = Vec::new();
    for req in &requirements {
        for s in &req.steps {
            steps.push(compile_shadow_step(spec, req, s, &source)?);
        }
    }

    let plan = match resolved {
        Ok(plan) => plan,
        Err(ResolveError::UnsatisfiedRequired { reason, .. }) => {
            return Err(Reject(gap_reason_to_preflight(reason)))
        }
        Err(_) => return Err(Reject("policy admission rejected by Hook resolver")),
    };
    if let Some(gap) = plan.coverage().gaps.first() {
        return Err(Reject(gap_reason_to_preflight(gap.reason)));
    }
    Ok((plan, steps))
}

fn parse_steps(req: &HookRequirement<'_>) -> Result<Vec<ResolvedStepPlan>, ResolveError> {
    let mut seen_steps = BTreeSet::new();
    let mut steps = Vec::with_capacity(req.steps.len());
    for step in &req.steps {
        if !valid_identifier(step.step_id) || !valid_identifier(step.provider) {
            return Err(BadReq("invalid step or provider name"));
        }
        if !seen_steps.insert(step.step_id) {
            return Err(BadReq("duplicate step identifier in Hook requirement"));
        }
        if !valid_selectors(&step.step_tools) {
            return Err(BadReq("invalid step tool selectors"));
        }
        let native = step.configuration().get("native").is_some();
        let on_error = FailureAction::parse_name(step.on_error)
            .filter(|a| event_supports_failure_action(req.event, *a))
            .ok_or(BadReq(ERR_ACT))?;
        let mut effects = Vec::with_capacity(step.effects.len());
        if native && !step.effects.is_empty() {
            return Err(BadReq("native Hook step cannot declare Provider effects"));
        } else if !native {
            if step.effects.is_empty() {
                return Err(BadReq("step effects must be a non-empty array"));
            }
            let mut seen_effects = BTreeSet::new();
            for raw in &step.effects {
                let effect = HookEffect::parse_name(raw)
                    .filter(|e| event_supports_effect(req.event, *e))
                    .ok_or(BadReq(ERR_EFF))?;
                if !seen_effects.insert(effect) {
                    return Err(BadReq("duplicate step effect"));
                }
                effects.push(effect);
            }
        }
        let (step_id, provider) = (step.step_id.to_owned(), step.provider.to_owned());
        let (step_tools, dispatch_owner) = (step.step_tools.clone(), String::new());
        steps.push(ResolvedStepPlan {
            step_id,
            provider,
            effects,
            on_error,
            step_tools,
            native,
            dispatch_owner,
        });
    }
    Ok(steps)
}

fn check_step_ordering(
    event: &str,
    steps: &[ResolvedStepPlan],
    sources: &[SourceCapability],
) -> Result<(), ResolveError> {
    if steps
        .iter()
        .position(|s| s.effects.contains(&HookEffect::ReplaceResult))
        .is_some_and(|idx| idx + 1 != steps.len())
    {
        return Err(BadReq(ERR_REPLACE_RES));
    }
    let any_replace_input = sources.iter().any(|s| {
        s.hook(event)
            .is_some_and(|c| c.supports_effect(HookEffect::ReplaceInput))
    });
    for (idx, step) in steps.iter().enumerate() {
        if step.effects.contains(&HookEffect::ReplaceInput)
            && any_replace_input
            && !(idx == 0
                && step.effects == [HookEffect::ReplaceInput]
                && steps.iter().all(|c| c.on_error == FailureAction::Block))
        {
            return Err(BadReq(ERR_REPLACE_IN));
        }
    }
    Ok(())
}

fn diagnose_gap(
    candidates: &[(&SourceCapability, &HookPointCapability)],
    needs_native: bool,
    requested_effects: &BTreeSet<HookEffect>,
    needed_actions: &BTreeSet<FailureAction>,
) -> GapDiagnosis {
    let mut ranked = candidates.to_vec();
    ranked.sort_by_key(|(s, c)| (c.mode, s.kind(), s.source_id()));
    let ctrl = HookAuthority::Control;
    let ok_fx = |c: &HookPointCapability, e: HookEffect| {
        c.supports_effect(e) && (!e.is_control() || c.mode == ctrl)
    };
    let effects_ok: Vec<_> = ranked
        .iter()
        .copied()
        .filter(|(_, c)| requested_effects.iter().all(|e| ok_fx(c, *e)))
        .collect();
    if effects_ok.is_empty() {
        let (_, best) = ranked[0];
        let missing = requested_effects
            .iter()
            .copied()
            .filter(|e| !ok_fx(best, *e))
            .collect();
        return (CoverageGapReason::UnsupportedEffect, missing, Vec::new());
    }
    if needs_native
        && !effects_ok
            .iter()
            .any(|(s, c)| s.kind() == SourceKind::Native && c.mode == ctrl)
    {
        let r = CoverageGapReason::UnsupportedNativeStep;
        return (r, Vec::new(), Vec::new());
    }
    let (_, best) = effects_ok[0];
    let missing = needed_actions
        .iter()
        .copied()
        .filter(|a| !best.supports_failure_action(*a))
        .collect();
    let r = CoverageGapReason::UnsupportedFailureAction;
    (r, Vec::new(), missing)
}

fn to_selected(
    src: &SourceCapability,
    cap: &HookPointCapability,
    dispatches_steps: bool,
) -> SelectedSource {
    SelectedSource {
        source_id: src.source_id().to_owned(),
        kind: src.kind(),
        contract: src.contract().to_owned(),
        entrypoint: src.entrypoint().to_owned(),
        mode: cap.mode,
        effects: cap.effects.clone(),
        failure_actions: cap.failure_actions.clone(),
        dispatches_steps,
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

fn valid_selectors(selectors: &[String]) -> bool {
    !selectors.is_empty()
        && selectors.len() <= 128
        && selectors
            .iter()
            .all(|s| !s.is_empty() && s.chars().count() <= 256 && !s.chars().any(char::is_control))
}

fn gap_reason_to_preflight(reason: CoverageGapReason) -> &'static str {
    match reason {
        CoverageGapReason::UnsupportedEvent => ERR_EVT,
        CoverageGapReason::UnsupportedEffect => ERR_EFF,
        CoverageGapReason::UnsupportedFailureAction => ERR_ACT,
        CoverageGapReason::UnsupportedNativeStep => "native hook step is required",
    }
}

fn compile_shadow_step(
    spec: &Value,
    req: &HookRequirement<'_>,
    step_req: &StepRequirement<'_>,
    source: &SourceCapability,
) -> Result<AdmittedStep, AdmissionError> {
    let event = req.event;
    let gate = EventGate::for_event(event).ok_or(Reject("enabled event is not supported"))?;
    let cap = source.hook(event).ok_or(Reject(ERR_EVT))?;
    let provider = spec["providers"]
        .get(step_req.provider)
        .ok_or(Reject("unknown configured Provider"))?;
    let protocol = provider_protocol(provider);
    let ok_proto = matches!(
        protocol,
        "aw-provider/v1alpha1" | "native-hook/v1alpha1" | aw_config::HOOK_PROTOCOL
    );
    if !ok_proto
        || provider["transport"]["type"] != "stdio"
        || provider["transport"]["location"] != "agent"
    {
        return Err(Reject("Provider transport or protocol is not supported"));
    }
    let is_native = step_req.configuration().get("native").is_some();
    let execution = if protocol == "native-hook/v1alpha1" {
        if !is_native {
            return Err(Reject("native hook step is required"));
        }
        if !gate.allows_native_hook() {
            return Err(Reject("native hook step is not supported for this event"));
        }
        StepExecution::Native
    } else {
        if is_native {
            return Err(Reject("step effects must be an array"));
        }
        let effects: Vec<String> = step_req.effects.iter().map(|r| (*r).to_owned()).collect();
        let valid_fx = |r: &String| {
            gate.allows_effect(r)
                && HookEffect::parse_name(r).is_some_and(|e| cap.supports_effect(e))
        };
        if !effects.iter().all(valid_fx) {
            return Err(Reject(ERR_EFF));
        }
        if protocol == aw_config::HOOK_PROTOCOL {
            validate_boolean_effects(step_req).map_err(Reject)?;
            let reason_code = step_req.configuration()["reason_code"]
                .as_str()
                .map(str::to_owned);
            StepExecution::Hook {
                effects,
                reason_code,
            }
        } else {
            let op = step_req.configuration()["operation"].as_str();
            let operation = op
                .ok_or(Reject("configuration field must be a string"))?
                .to_owned();
            StepExecution::Provider { operation, effects }
        }
    };
    let ok_act = gate.allows_failure_action(step_req.on_error)
        && FailureAction::parse_name(step_req.on_error)
            .is_some_and(|a| cap.supports_failure_action(a));
    if !ok_act {
        return Err(Reject(ERR_ACT));
    }
    Ok(AdmittedStep {
        event: event.to_owned(),
        step_id: step_req.step_id.to_owned(),
        provider: step_req.provider.to_owned(),
        execution,
        on_error: step_req.on_error.to_owned(),
        event_tools: req.event_tools.clone(),
        step_tools: step_req.step_tools.clone(),
    })
}

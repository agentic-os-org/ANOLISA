//! Versioned resolved Hook plans and per-Hook coverage reports.
//!
//! A [`ResolvedHookPlan`] records the deterministic outcome of pure Hook Source
//! resolution for one configured Agent target: at most one control owner per
//! enabled Hook point, optional auxiliary observation sources that never
//! re-dispatch policy steps, and an explicit [`Coverage`] summary of satisfied
//! points and optional capability gaps. It must be paired with the corresponding
//! immutable policy snapshot/revision for step execution and is completely
//! separate from the independent `Core::prepare/execute` capability plan contract.

use crate::{
    canonical, require,
    source::{
        event_supports_effect, event_supports_failure_action, is_hook_event, shared_registry,
        FailureAction, HookAuthority, HookEffect, HookPointCapability, SourceKind,
    },
    Error, Registry,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Identifies the step that needs post-permission adoption, if one is configured.
///
/// A first logical rewrite may also observe. A step that can deny and replace,
/// or a rewrite following other checks, requires a terminal handoff.
///
/// # Errors
/// Rejects multiple rewrites, invalid positions and fail-open enforcing steps.
pub fn input_handoff<'a, E>(
    steps: impl Iterator<Item = (&'a str, E, bool)>,
) -> Result<Option<&'a str>, &'static str>
where
    E: Iterator<Item = &'a str> + Clone,
{
    let mut seen_rewrite = false;
    let mut handoff = None;
    let mut closed = true;
    for (index, (id, mut effects, block)) in steps.enumerate() {
        closed &= block;
        if handoff.is_some() {
            return Err(INPUT_REPLACEMENT_ORDER);
        }
        if effects.clone().any(|effect| effect == "replace_input") {
            if seen_rewrite
                || !effects
                    .clone()
                    .all(|effect| matches!(effect, "observe" | "block" | "replace_input"))
            {
                return Err(INPUT_REPLACEMENT_ORDER);
            }
            seen_rewrite = true;
            if index > 0 || effects.any(|effect| effect == "block") {
                handoff = Some(id);
            }
        }
    }
    if seen_rewrite && !closed {
        return Err(INPUT_REPLACEMENT_ORDER);
    }
    Ok(handoff)
}

const INPUT_REPLACEMENT_ORDER: &str =
    "input replacement requires one initial logical rewrite or terminal handoff and fail-closed before steps";

/// Admission diagnostic when an entrypoint cannot adopt arguments after native checks.
pub const HANDOFF_UNSUPPORTED: &str = "entrypoint lacks post-permission input handoff";

/// Version of the resolved Hook plan contract understood by this build.
pub const VERSION: &str = "aw-resolved-hook-plan/v1alpha1";

/// Registered schema catalogue name for [`ResolvedHookPlan`].
pub const SCHEMA_NAME: &str = "resolved-hook-plan-v1alpha1";

/// One candidate Source selected as either the control owner or an observer.
///
/// `mode`, `effects`, and `failure_actions` record the Source's declared
/// capability at this Hook point, whereas `dispatches_steps` and placement in
/// `control_owner` versus `observation_sources` record its assigned plan role.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedSource {
    /// Whether this entrypoint adopts tool input after native permission checks.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub post_permission: bool,
    /// Unique identifier of the selected Source.
    pub source_id: String,
    /// Mechanism class of the selected Source.
    pub kind: SourceKind,
    /// Verified integration surface or Hook contract version.
    pub contract: String,
    /// Validated runtime interaction entrypoint.
    pub entrypoint: String,
    /// Control versus observation authority at this Hook point.
    pub mode: HookAuthority,
    /// Supported Provider effects at this Hook point.
    pub effects: Vec<HookEffect>,
    /// Supported failure actions at this Hook point.
    pub failure_actions: Vec<FailureAction>,
    /// Whether this Source is the sole policy-step dispatch owner for the Hook point.
    pub dispatches_steps: bool,
}

impl SelectedSource {
    fn as_capability(&self) -> HookPointCapability {
        HookPointCapability {
            post_permission: self.post_permission,
            mode: self.mode,
            effects: self.effects.clone(),
            failure_actions: self.failure_actions.clone(),
        }
    }
}

/// One admitted policy step bound to a single dispatch owner Source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedStepPlan {
    /// Step identifier within its semantic Hook event.
    pub step_id: String,
    /// Configured Provider reference.
    pub provider: String,
    /// Requested Provider effects; empty for uninterpreted native Hook steps.
    pub effects: Vec<HookEffect>,
    /// Failure action enforced when step execution fails.
    pub on_error: FailureAction,
    /// Step-level tool selectors.
    pub step_tools: Vec<String>,
    /// Whether this step uses `native-hook/v1alpha1`.
    pub native: bool,
    /// Whether the configured step only observes and reports its outcome.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shadow: bool,
    /// Unique `source_id` responsible for dispatching this policy step once.
    pub dispatch_owner: String,
}

/// Resolution binding for one satisfied enabled semantic Hook point.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedHookPoint {
    /// Canonical semantic Hook event name.
    pub event: String,
    /// Whether configured availability requires this Hook point.
    pub required: bool,
    /// Effective event budget in milliseconds.
    pub budget_ms: u64,
    /// Event-level tool selectors.
    pub event_tools: Vec<String>,
    /// Unique control owner for this Hook point, if a control source was selected.
    pub control_owner: Option<SelectedSource>,
    /// Auxiliary or observation-only sources attached to this Hook point.
    pub observation_sources: Vec<SelectedSource>,
    /// Ordered policy steps bound to the single dispatch owner of this Hook point.
    pub steps: Vec<ResolvedStepPlan>,
}

impl ResolvedHookPoint {
    /// Returns the single Source that dispatches policy steps at this Hook point.
    pub fn dispatch_owner(&self) -> Option<&SelectedSource> {
        if let Some(owner) = &self.control_owner {
            Some(owner)
        } else {
            self.observation_sources
                .iter()
                .find(|source| source.dispatches_steps)
        }
    }

    fn validate(&self) -> Result<(), Error> {
        require(
            is_hook_event(&self.event),
            "unknown resolved Hook event name",
        )?;
        require(
            self.budget_ms >= 1 && self.budget_ms <= u64::from(u32::MAX),
            "resolved Hook budget is out of range",
        )?;
        require(
            !self.event_tools.is_empty(),
            "resolved Hook point requires event tool selectors",
        )?;
        let mut seen_sources = BTreeSet::new();
        if let Some(owner) = &self.control_owner {
            owner
                .as_capability()
                .validate_for_event(owner.kind, &self.event)?;
            require(
                owner.mode == HookAuthority::Control,
                "control owner must have control authority",
            )?;
            require(
                owner.dispatches_steps,
                "control owner must own policy step dispatch",
            )?;
            require(
                seen_sources.insert(owner.source_id.as_str()),
                "duplicate Source identity on Hook point",
            )?;
            for observer in &self.observation_sources {
                observer
                    .as_capability()
                    .validate_for_event(observer.kind, &self.event)?;
                require(
                    seen_sources.insert(observer.source_id.as_str()),
                    "duplicate Source identity on Hook point",
                )?;
                require(
                    !observer.dispatches_steps,
                    "auxiliary observation source cannot dispatch steps when a control owner exists",
                )?;
            }
        } else {
            require(
                !self.observation_sources.is_empty(),
                "resolved Hook point without control owner requires an observation source",
            )?;
            for (index, observer) in self.observation_sources.iter().enumerate() {
                observer
                    .as_capability()
                    .validate_for_event(observer.kind, &self.event)?;
                require(
                    seen_sources.insert(observer.source_id.as_str()),
                    "duplicate Source identity on Hook point",
                )?;
                require(
                    observer.dispatches_steps == (index == 0),
                    "exactly one primary observation source must dispatch steps",
                )?;
                require(
                    !observer.dispatches_steps || observer.mode == HookAuthority::Observe,
                    "resolved Hook point without control owner cannot dispatch steps from a control-mode observer",
                )?;
            }
        }

        let dispatch_owner = self.dispatch_owner().ok_or(Error::Invariant(
            "resolved Hook point has no dispatch owner",
        ))?;
        let dispatch_capability = dispatch_owner.as_capability();
        let enforcing = self
            .steps
            .iter()
            .enumerate()
            .filter(|(_, step)| !step.shadow);
        let last = enforcing.clone().next_back().map(|(index, _)| index);
        let mut seen_steps = BTreeSet::new();
        for (index, step) in self.steps.iter().enumerate() {
            require(
                seen_steps.insert(step.step_id.as_str()),
                "duplicate step identifier in resolved Hook point",
            )?;
            require(
                !step.step_tools.is_empty(),
                "resolved step requires tool selectors",
            )?;
            require(
                step.dispatch_owner == dispatch_owner.source_id,
                "resolved step dispatch owner does not match the Hook point dispatch owner",
            )?;
            if step.shadow {
                require(
                    !step.native
                        && step.effects == [HookEffect::Observe]
                        && step.on_error == FailureAction::Report,
                    "shadow step requires observe and report",
                )?;
            }
            if step.native {
                require(
                    matches!(self.event.as_str(), "tool.before" | "tool.after"),
                    "native Hook steps require tool.before or tool.after",
                )?;
                require(
                    step.effects.is_empty(),
                    "native Hook step cannot declare Provider effects",
                )?;
                require(
                    dispatch_owner.kind == SourceKind::Native
                        && dispatch_owner.mode == HookAuthority::Control,
                    "native Hook step requires a Native control owner",
                )?;
                require(
                    matches!(step.on_error, FailureAction::Report | FailureAction::Block),
                    "native Hook step supports report or block failure actions",
                )?;
            } else {
                require(
                    !step.effects.is_empty(),
                    "structured step requires at least one effect",
                )?;
                let mut seen_effects = BTreeSet::new();
                for effect in &step.effects {
                    require(
                        seen_effects.insert(*effect),
                        "duplicate effect in resolved step",
                    )?;
                    require(
                        event_supports_effect(&self.event, *effect)
                            && dispatch_capability.supports_effect(*effect),
                        "resolved step effect is not supported by the dispatch owner",
                    )?;
                    if effect.is_control() {
                        require(
                            self.control_owner.is_some(),
                            "control effect requires a control owner",
                        )?;
                    }
                }
            }
            require(
                event_supports_failure_action(&self.event, step.on_error)
                    && dispatch_capability.supports_failure_action(step.on_error),
                "resolved step failure action is not supported by the dispatch owner",
            )?;
            if step.on_error.is_control() {
                require(
                    self.control_owner.is_some(),
                    "control failure action requires a control owner",
                )?;
            }
            if step.effects.contains(&HookEffect::ReplaceResult) {
                require(
                    self.event == "tool.after" && Some(index) == last,
                    "result replacement must be the last applicable enabled event step",
                )?;
            }
        }
        let handoff = input_handoff(enforcing.map(|(_, step)| {
            (
                step.step_id.as_str(),
                step.effects.iter().map(|effect| effect.as_str()),
                step.on_error == FailureAction::Block,
            )
        }))
        .map_err(Error::Invariant)?;
        require(
            handoff.is_none() || dispatch_owner.post_permission,
            HANDOFF_UNSUPPORTED,
        )?;
        Ok(())
    }
}

/// Coverage summary for one satisfied enabled semantic Hook point.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SatisfiedHookCoverage {
    /// Canonical semantic Hook event name.
    pub event: String,
    /// Whether this Hook point was marked `required: true`.
    pub required: bool,
    /// Selected control owner `source_id`, if any.
    pub control_owner: Option<String>,
    /// Selected auxiliary or observation-only `source_id`s.
    pub observation_sources: Vec<String>,
    /// Effective required effects satisfied at this Hook point.
    pub effects: Vec<HookEffect>,
    /// Effective required failure actions satisfied at this Hook point.
    pub failure_actions: Vec<FailureAction>,
}

/// Status of an unsatisfied optional Hook requirement in [`Coverage`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageGapStatus {
    /// No candidate Source declared support for the semantic Hook event.
    Uncovered,
    /// One or more Sources can observe the event, but required control/effects are missing.
    Degraded,
}

/// Machine-readable reason explaining why an optional Hook requirement was not satisfied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageGapReason {
    /// No candidate Source declared the semantic Hook event.
    UnsupportedEvent,
    /// Candidate Sources for the event lack one or more requested step effects.
    UnsupportedEffect,
    /// Candidate Sources for the event lack a requested step failure action.
    UnsupportedFailureAction,
    /// A step requires a `native-hook/v1alpha1` Source, but only non-native Sources exist.
    UnsupportedNativeStep,
}

/// Detailed gap record for an optional Hook requirement that could not be satisfied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageGap {
    /// Canonical semantic Hook event name.
    pub event: String,
    /// Whether the requirement was marked `required` (`false` in valid resolved plans).
    pub required: bool,
    /// Whether the event is completely uncovered or degraded to observation candidates.
    pub status: CoverageGapStatus,
    /// Primary cause of the coverage gap.
    pub reason: CoverageGapReason,
    /// Specific requested effects missing from candidate Sources.
    pub missing_effects: Vec<HookEffect>,
    /// Specific requested failure actions missing from candidate Sources.
    pub missing_failure_actions: Vec<FailureAction>,
    /// Candidate `source_id`s that can observe the event when `status` is `degraded`.
    pub fallback_sources: Vec<String>,
}

impl CoverageGap {
    fn validate(&self) -> Result<(), Error> {
        require(
            is_hook_event(&self.event),
            "unknown coverage gap Hook event",
        )?;
        require(
            !self.required,
            "required Hook requirement cannot degrade into a coverage gap",
        )?;
        let mut seen_effects = BTreeSet::new();
        for effect in &self.missing_effects {
            require(
                seen_effects.insert(*effect) && event_supports_effect(&self.event, *effect),
                "coverage gap missing effect is duplicate or incompatible with the event",
            )?;
        }
        let mut seen_actions = BTreeSet::new();
        for action in &self.missing_failure_actions {
            require(
                seen_actions.insert(*action)
                    && action.is_control()
                    && event_supports_failure_action(&self.event, *action),
                "coverage gap missing failure action is duplicate or incompatible with the event",
            )?;
        }
        match self.status {
            CoverageGapStatus::Uncovered => {
                require(
                    self.reason == CoverageGapReason::UnsupportedEvent
                        && self.fallback_sources.is_empty(),
                    "uncovered Hook gap must use unsupported_event without fallback sources",
                )?;
            }
            CoverageGapStatus::Degraded => {
                require(
                    self.reason != CoverageGapReason::UnsupportedEvent
                        && !self.fallback_sources.is_empty(),
                    "degraded Hook gap requires fallback observation sources and a specific reason",
                )?;
            }
        }
        match self.reason {
            CoverageGapReason::UnsupportedEvent => {}
            CoverageGapReason::UnsupportedEffect => {
                require(
                    !self.missing_effects.is_empty(),
                    "unsupported_effect gap must list missing effects",
                )?;
            }
            CoverageGapReason::UnsupportedFailureAction => {
                require(
                    !self.missing_failure_actions.is_empty(),
                    "unsupported_failure_action gap must list missing failure actions",
                )?;
            }
            CoverageGapReason::UnsupportedNativeStep => {
                require(
                    self.missing_effects.is_empty() && self.missing_failure_actions.is_empty(),
                    "unsupported_native_step gap cannot carry missing Provider effects or failure actions",
                )?;
            }
        }
        Ok(())
    }
}

/// Complete Hook resolution coverage report for one target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    /// Satisfied enabled Hook points sorted by event name.
    pub satisfied: Vec<SatisfiedHookCoverage>,
    /// Uncovered or degraded optional Hook points sorted by event name.
    pub gaps: Vec<CoverageGap>,
}

impl Coverage {
    /// Returns whether every applicable enabled Hook requirement was satisfied without gaps.
    pub fn is_complete(&self) -> bool {
        self.gaps.is_empty()
    }
}

/// Versioned resolved Hook plan binding enabled requirements to candidate Sources.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ResolvedHookPlan(Fields);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fields {
    version: String,
    target: String,
    adapter: String,
    hooks: BTreeMap<String, ResolvedHookPoint>,
    coverage: Coverage,
}

impl ResolvedHookPlan {
    /// Builds and validates a [`ResolvedHookPlan`] from typed fields.
    ///
    /// # Errors
    /// Rejects duplicate or mismatched Hook entries, multiple control or dispatch
    /// owners, unsupported step effects/failure actions, or inconsistent coverage.
    pub fn new(
        target: impl Into<String>,
        adapter: impl Into<String>,
        hooks: BTreeMap<String, ResolvedHookPoint>,
        coverage: Coverage,
    ) -> Result<Self, Error> {
        let candidate = Self(Fields {
            version: VERSION.into(),
            target: target.into(),
            adapter: adapter.into(),
            hooks,
            coverage,
        });
        let value = serde_json::to_value(&candidate)
            .map_err(|_| Error::SchemaMismatch(SCHEMA_NAME.into()))?;
        let encoded = canonical::bytes(&value)?;
        Self::parse(&encoded)
    }

    /// Parses bounded JSON bytes and checks schema and semantic invariants.
    ///
    /// # Errors
    /// Rejects duplicate keys, unknown fields, unsupported versions, and broken
    /// owner, step-deduplication or coverage invariants.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let value = canonical::parse(bytes)?;
        shared_registry()?.validate_hook_plan(&value)
    }

    /// Validates a decoded JSON value as a [`ResolvedHookPlan`].
    ///
    /// # Errors
    /// Returns a contract error if the value violates the schema or invariants.
    pub fn from_value(value: Value) -> Result<Self, Error> {
        shared_registry()?.validate_hook_plan(&value)
    }

    /// Exact contract version string (`aw-resolved-hook-plan/v1alpha1`).
    pub fn version(&self) -> &str {
        &self.0.version
    }

    /// Configured Agent target for which this plan was resolved.
    pub fn target(&self) -> &str {
        &self.0.target
    }

    /// Configured Adapter identifier for `target`.
    pub fn adapter(&self) -> &str {
        &self.0.adapter
    }

    /// Satisfied Hook points keyed by canonical semantic Hook name.
    pub fn hooks(&self) -> &BTreeMap<String, ResolvedHookPoint> {
        &self.0.hooks
    }

    /// Looks up the resolution binding for one semantic Hook event.
    pub fn hook(&self, event: &str) -> Option<&ResolvedHookPoint> {
        self.0.hooks.get(event)
    }

    /// Complete coverage summary of satisfied Hook points and optional gaps.
    pub fn coverage(&self) -> &Coverage {
        &self.0.coverage
    }

    /// Re-verifies all semantic invariants on this resolved Hook plan.
    ///
    /// # Errors
    /// Rejects broken single-owner, step-deduplication or coverage invariants.
    pub fn validate(&self) -> Result<(), Error> {
        require(
            self.0.version == VERSION,
            "unsupported resolved Hook plan version",
        )?;
        for (key, point) in &self.0.hooks {
            require(
                key == &point.event,
                "resolved Hook key does not match event",
            )?;
            point.validate()?;
        }
        require(
            self.0.coverage.satisfied.len() == self.0.hooks.len(),
            "coverage satisfied count does not match resolved Hook points",
        )?;
        for (satisfied, (event, point)) in self.0.coverage.satisfied.iter().zip(self.0.hooks.iter())
        {
            require(
                &satisfied.event == event && satisfied.required == point.required,
                "coverage satisfied entry does not match resolved Hook point",
            )?;
            require(
                satisfied.control_owner.as_deref()
                    == point
                        .control_owner
                        .as_ref()
                        .map(|owner| owner.source_id.as_str()),
                "coverage control owner does not match resolved Hook point",
            )?;
            let expected_observers: Vec<String> = point
                .observation_sources
                .iter()
                .map(|source| source.source_id.clone())
                .collect();
            require(
                satisfied.observation_sources == expected_observers,
                "coverage observation sources do not match resolved Hook point",
            )?;
            let dispatch_owner = point.dispatch_owner().ok_or(Error::Invariant(
                "resolved Hook point has no dispatch owner",
            ))?;
            let capability = dispatch_owner.as_capability();
            let mut required_effects = BTreeSet::from([HookEffect::Observe]);
            let mut required_actions = BTreeSet::new();
            for step in &point.steps {
                for effect in &step.effects {
                    required_effects.insert(*effect);
                }
                if step.on_error == FailureAction::Block {
                    required_effects.insert(HookEffect::Block);
                }
                required_actions.insert(step.on_error);
            }
            let expected_effects: Vec<HookEffect> = required_effects.into_iter().collect();
            let expected_actions: Vec<FailureAction> = required_actions.into_iter().collect();
            require(
                satisfied.effects == expected_effects
                    && satisfied
                        .effects
                        .iter()
                        .all(|effect| capability.supports_effect(*effect)),
                "coverage satisfied effects do not match resolved steps",
            )?;
            require(
                satisfied.failure_actions == expected_actions
                    && satisfied
                        .failure_actions
                        .iter()
                        .all(|action| capability.supports_failure_action(*action)),
                "coverage satisfied failure actions do not match resolved steps",
            )?;
        }
        let mut seen_gaps = BTreeSet::new();
        for gap in &self.0.coverage.gaps {
            gap.validate()?;
            require(
                seen_gaps.insert(gap.event.as_str()),
                "duplicate event in coverage gaps",
            )?;
            require(
                !self.0.hooks.contains_key(&gap.event),
                "coverage gap event cannot also be present in resolved hooks",
            )?;
        }
        Ok(())
    }

    /// Encodes this resolved Hook plan into canonical AW JSON bytes.
    ///
    /// # Errors
    /// Returns [`Error::InvalidDocument`] if serialization fails canonical bounds.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, Error> {
        let value = serde_json::to_value(self).map_err(|_| Error::InvalidDocument)?;
        canonical::bytes(&value)
    }
}

impl Registry {
    /// Validates a JSON value against `resolved-hook-plan-v1alpha1` and semantic rules.
    ///
    /// # Errors
    /// Rejects schema mismatches, multiple control/dispatch owners, duplicate steps
    /// and inconsistent coverage summaries.
    pub fn validate_hook_plan(&self, value: &Value) -> Result<ResolvedHookPlan, Error> {
        self.validate(SCHEMA_NAME, value)?;
        let plan: ResolvedHookPlan = serde_json::from_value(value.clone())
            .map(ResolvedHookPlan)
            .map_err(|_| Error::SchemaMismatch(SCHEMA_NAME.into()))?;
        plan.validate()?;
        Ok(plan)
    }
}

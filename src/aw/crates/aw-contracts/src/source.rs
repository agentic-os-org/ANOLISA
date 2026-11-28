//! Versioned Hook Source capability declarations and 16-Hook capability cells.
//!
//! A [`SourceCapability`] describes the static or probed upper bound of one Hook
//! Source (such as a framework native callback, Python/JS runtime instrumentation,
//! or out-of-band AgentSight observation) at a specific contract and entrypoint.
//! Validation establishes shape and capability invariants only, never installed
//! wiring, process ownership or runtime effect adoption.

use crate::{canonical, require, Error, Registry};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

/// Version of the Hook Source capability contract understood by this build.
pub const VERSION: &str = "aw-source-capability/v1alpha1";

/// Registered schema catalogue name for [`SourceCapability`].
pub const SCHEMA_NAME: &str = "source-capability-v1alpha1";

/// Canonical 16 semantic Hook event names shared across configuration and contracts.
pub const HOOK_EVENTS: [&str; 16] = [
    "session.start",
    "input.submit",
    "tool.before",
    "tool.after",
    "permission.request",
    "compact.before",
    "compact.after",
    "subagent.start",
    "subagent.stop",
    "turn.stop",
    "session.end",
    "model.before_request",
    "runtime.observed",
    "runtime.exited",
    "security.violation",
    "coverage.changed",
];

/// Returns whether `event` is one of the 16 canonical semantic Hook names.
pub fn is_hook_event(event: &str) -> bool {
    HOOK_EVENTS.contains(&event)
}

/// Returns whether `event` permits `effect` under the semantic Hook gate contract.
pub fn event_supports_effect(event: &str, effect: HookEffect) -> bool {
    is_hook_event(event)
        && match effect {
            HookEffect::Observe => true,
            HookEffect::Block => matches!(
                event,
                "input.submit"
                    | "tool.before"
                    | "permission.request"
                    | "compact.before"
                    | "subagent.start"
                    | "turn.stop"
                    | "model.before_request"
                    | "security.violation"
            ),
            HookEffect::ReplaceInput => event == "tool.before",
            HookEffect::ReplaceResult => event == "tool.after",
            HookEffect::Ask => matches!(event, "tool.before" | "permission.request"),
        }
}

/// Returns whether `event` permits `action` as a step failure handling contract.
pub fn event_supports_failure_action(event: &str, action: FailureAction) -> bool {
    is_hook_event(event)
        && match action {
            FailureAction::Report => true,
            FailureAction::Block => event_supports_effect(event, HookEffect::Block),
            FailureAction::WithholdResult => event == "tool.after",
        }
}

/// Implementation mechanism class of a Hook Source, ordered by selection preference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Framework or application native Hook callback.
    Native,
    /// Python runtime monitoring or wrapper instrumentation.
    Python,
    /// JavaScript or TypeScript module/loader instrumentation.
    Js,
    /// Out-of-band AgentSight / eBPF / ptrace observation source.
    Sight,
}

impl SourceKind {
    /// Canonical lowercase wire identifier for this Source kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Python => "python",
            Self::Js => "js",
            Self::Sight => "sight",
        }
    }
}

/// Control versus passive observation authority at a semantic Hook point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookAuthority {
    /// Inline synchronous source eligible to act as the unique control owner.
    Control,
    /// Passive observation source that cannot enforce blocking or mutation effects.
    Observe,
}

impl HookAuthority {
    /// Canonical lowercase wire identifier for this authority mode.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Observe => "observe",
        }
    }
}

/// Semantic Hook effect requested by a policy step or offered by a Source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEffect {
    /// Observe event metadata or payload without altering execution.
    Observe,
    /// Deny or stop the guarded action before its side-effect boundary.
    Block,
    /// Replace tool input before execution under fail-closed before steps.
    ReplaceInput,
    /// Replace tool output before model or caller delivery.
    ReplaceResult,
    /// Request interactive or external permission confirmation.
    Ask,
}

impl HookEffect {
    /// Canonical lowercase wire identifier for this effect.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::Block => "block",
            Self::ReplaceInput => "replace_input",
            Self::ReplaceResult => "replace_result",
            Self::Ask => "ask",
        }
    }

    /// Parses a wire effect identifier without allocation.
    pub fn parse_name(name: &str) -> Option<Self> {
        match name {
            "observe" => Some(Self::Observe),
            "block" => Some(Self::Block),
            "replace_input" => Some(Self::ReplaceInput),
            "replace_result" => Some(Self::ReplaceResult),
            "ask" => Some(Self::Ask),
            _ => None,
        }
    }

    /// Returns whether this effect requires synchronous control authority.
    pub fn is_control(self) -> bool {
        !matches!(self, Self::Observe)
    }
}

/// Host/Source action taken when a Provider step execution fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureAction {
    /// Record the failure and allow native execution or delivery to continue.
    Report,
    /// Deny the guarded action when the Provider step fails.
    Block,
    /// Withhold unchecked post-tool output and stop subsequent dispatch.
    WithholdResult,
}

impl FailureAction {
    /// Canonical lowercase wire identifier for this failure action.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Report => "report",
            Self::Block => "block",
            Self::WithholdResult => "withhold_result",
        }
    }

    /// Parses a wire failure-action identifier without allocation.
    pub fn parse_name(name: &str) -> Option<Self> {
        match name {
            "report" => Some(Self::Report),
            "block" => Some(Self::Block),
            "withhold_result" => Some(Self::WithholdResult),
            _ => None,
        }
    }

    /// Returns whether this failure action requires synchronous control authority.
    pub fn is_control(self) -> bool {
        !matches!(self, Self::Report)
    }
}

/// Per-Hook capability declaration within a [`SourceCapability`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookPointCapability {
    /// Whether this entrypoint adopts tool input after native permission checks.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub post_permission: bool,
    /// Whether this source can own synchronous control or only passive observation.
    pub mode: HookAuthority,
    /// Supported Provider effects at this semantic Hook point.
    pub effects: Vec<HookEffect>,
    /// Additional or explicit failure actions supported at this Hook point.
    pub failure_actions: Vec<FailureAction>,
}

impl HookPointCapability {
    /// Returns whether this Hook point supports `effect`.
    pub fn supports_effect(&self, effect: HookEffect) -> bool {
        self.effects.contains(&effect)
    }

    /// Returns whether this Hook point can enforce `action` on Provider failure.
    pub fn supports_failure_action(&self, action: FailureAction) -> bool {
        match action {
            FailureAction::Report => true,
            FailureAction::Block => {
                self.mode == HookAuthority::Control && self.supports_effect(HookEffect::Block)
            }
            FailureAction::WithholdResult => {
                self.mode == HookAuthority::Control
                    && self
                        .failure_actions
                        .contains(&FailureAction::WithholdResult)
            }
        }
    }

    pub(crate) fn validate_for_event(&self, kind: SourceKind, event: &str) -> Result<(), Error> {
        require(is_hook_event(event), "unsupported semantic Hook event")?;
        require(
            !self.effects.is_empty(),
            "Hook capability requires at least one effect",
        )?;
        let mut seen_effects = BTreeSet::new();
        for effect in &self.effects {
            require(
                seen_effects.insert(*effect),
                "duplicate Hook capability effect",
            )?;
            require(
                event_supports_effect(event, *effect),
                "Hook capability effect is incompatible with the event",
            )?;
        }
        require(
            seen_effects.contains(&HookEffect::Observe),
            "Hook capability must include observe",
        )?;
        require(
            !self.post_permission
                || (self.mode == HookAuthority::Control
                    && self.supports_effect(HookEffect::ReplaceInput)),
            "post-permission handoff requires input replacement control",
        )?;
        let mut seen_actions = BTreeSet::new();
        for action in &self.failure_actions {
            require(
                seen_actions.insert(*action),
                "duplicate Hook capability failure action",
            )?;
            require(
                event_supports_failure_action(event, *action),
                "Hook capability failure action is incompatible with the event",
            )?;
            if *action == FailureAction::Block {
                require(
                    seen_effects.contains(&HookEffect::Block),
                    "block failure action requires block effect support",
                )?;
            }
        }
        if kind == SourceKind::Sight {
            require(
                self.mode == HookAuthority::Observe,
                "Sight observation sources cannot claim control authority",
            )?;
        }
        if self.mode == HookAuthority::Observe {
            require(
                self.effects.iter().all(|effect| !effect.is_control()),
                "observation-only Hook capability cannot declare control effects",
            )?;
            require(
                self.failure_actions
                    .iter()
                    .all(|action| !action.is_control()),
                "observation-only Hook capability cannot declare control failure actions",
            )?;
        }
        Ok(())
    }
}

/// Versioned capability contract for one candidate Hook Source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct SourceCapability(Fields);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fields {
    version: String,
    source_id: String,
    kind: SourceKind,
    adapter: String,
    contract: String,
    entrypoint: String,
    hooks: BTreeMap<String, HookPointCapability>,
}

impl SourceCapability {
    /// Builds and validates a [`SourceCapability`] from typed fields.
    ///
    /// # Errors
    /// Rejects invalid identifiers, unknown Hook events, or inconsistent control
    /// and observation declarations.
    pub fn new(
        source_id: impl Into<String>,
        kind: SourceKind,
        adapter: impl Into<String>,
        contract: impl Into<String>,
        entrypoint: impl Into<String>,
        hooks: BTreeMap<String, HookPointCapability>,
    ) -> Result<Self, Error> {
        let candidate = Self(Fields {
            version: VERSION.into(),
            source_id: source_id.into(),
            kind,
            adapter: adapter.into(),
            contract: contract.into(),
            entrypoint: entrypoint.into(),
            hooks,
        });
        let value = serde_json::to_value(&candidate)
            .map_err(|_| Error::SchemaMismatch(SCHEMA_NAME.into()))?;
        Self::parse(&canonical::bytes(&value)?)
    }

    /// Parses bounded JSON bytes and checks schema and semantic invariants.
    ///
    /// # Errors
    /// Rejects duplicate keys, unknown fields, unsupported versions, and invalid
    /// event/effect/authority combinations.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let value = canonical::parse(bytes)?;
        shared_registry()?.validate_source_capability(&value)
    }

    /// Validates a decoded JSON value as a [`SourceCapability`].
    ///
    /// # Errors
    /// Returns a contract error if the value violates the schema or invariants.
    pub fn from_value(value: Value) -> Result<Self, Error> {
        shared_registry()?.validate_source_capability(&value)
    }

    /// Exact contract version string (`aw-source-capability/v1alpha1`).
    pub fn version(&self) -> &str {
        &self.0.version
    }

    /// Unique identifier for this Source within a target resolution.
    pub fn source_id(&self) -> &str {
        &self.0.source_id
    }

    /// Mechanism category (`native`, `python`, `js`, or `sight`).
    pub fn kind(&self) -> SourceKind {
        self.0.kind
    }

    /// Configured Adapter identifier to which this Source applies.
    pub fn adapter(&self) -> &str {
        &self.0.adapter
    }

    /// Verified integration surface or Hook contract identifier.
    pub fn contract(&self) -> &str {
        &self.0.contract
    }

    /// Validated runtime interaction entrypoint.
    pub fn entrypoint(&self) -> &str {
        &self.0.entrypoint
    }

    /// Per-Hook capability map keyed by canonical semantic Hook name.
    pub fn hooks(&self) -> &BTreeMap<String, HookPointCapability> {
        &self.0.hooks
    }

    /// Looks up the declared capability for one semantic Hook event.
    pub fn hook(&self, event: &str) -> Option<&HookPointCapability> {
        self.0.hooks.get(event)
    }

    /// Re-verifies semantic invariants on this capability declaration.
    ///
    /// # Errors
    /// Rejects invalid versions or event/effect/authority contradictions.
    pub fn validate(&self) -> Result<(), Error> {
        require(
            self.0.version == VERSION,
            "unsupported source capability version",
        )?;
        for (event, capability) in &self.0.hooks {
            capability.validate_for_event(self.0.kind, event)?;
        }
        Ok(())
    }

    /// Encodes this capability into canonical AW JSON bytes.
    ///
    /// # Errors
    /// Returns [`Error::InvalidDocument`] if serialization fails canonical bounds.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, Error> {
        canonical::bytes(&serde_json::to_value(self).map_err(|_| Error::InvalidDocument)?)
    }
}

impl Registry {
    /// Validates a JSON value against `source-capability-v1alpha1` and semantic rules.
    ///
    /// # Errors
    /// Rejects schema mismatches and invalid authority/effect/failure-action combinations.
    pub fn validate_source_capability(&self, value: &Value) -> Result<SourceCapability, Error> {
        self.validate(SCHEMA_NAME, value)?;
        let capability: SourceCapability = serde_json::from_value(value.clone())
            .map(SourceCapability)
            .map_err(|_| Error::SchemaMismatch(SCHEMA_NAME.into()))?;
        capability.validate()?;
        Ok(capability)
    }
}

/// Executable support status of one `(adapter, event)` capability cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatus {
    /// Verified executable support in the profile.
    Supported,
    /// Experimentally verified source not enabled in the default profile.
    Experimental,
    /// Planned semantic Hook cell with no verified runtime pipeline yet.
    Planned,
    /// Not supported by the profile.
    Unsupported,
}

/// Structured capability declaration for one `(adapter, event)` cell in the 16-Hook grid.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityCell {
    /// Target framework or adapter identifier.
    pub adapter: String,
    /// Canonical semantic Hook name (one of [`HOOK_EVENTS`]).
    pub event: String,
    /// Executable support status of this cell.
    pub status: CapabilityStatus,
    /// Contributing Source kinds for this cell, ordered by priority.
    pub sources: Vec<SourceKind>,
    /// Effective authority (`control` or `observe`) when supported.
    pub mode: Option<HookAuthority>,
    /// Supported Provider effects at this cell.
    pub effects: Vec<HookEffect>,
    /// Supported failure actions at this cell.
    pub failure_actions: Vec<FailureAction>,
}

impl CapabilityCell {
    /// Checks semantic consistency of a single `16 × framework` capability cell.
    ///
    /// # Errors
    /// Rejects unknown Hook names, unsupported cells carrying effects/sources, or
    /// supported cells lacking sources, authority or `observe`.
    pub fn validate(&self) -> Result<(), Error> {
        require(
            valid_identifier(&self.adapter),
            "invalid capability cell adapter identifier",
        )?;
        require(
            is_hook_event(&self.event),
            "unknown capability cell Hook event",
        )?;
        match self.status {
            CapabilityStatus::Unsupported | CapabilityStatus::Planned => require(
                self.sources.is_empty()
                    && self.mode.is_none()
                    && self.effects.is_empty()
                    && self.failure_actions.is_empty(),
                "unsupported or planned capability cell cannot claim executable capabilities",
            ),
            CapabilityStatus::Supported | CapabilityStatus::Experimental => {
                require(
                    !self.sources.is_empty(),
                    "supported capability cell requires at least one Source kind",
                )?;
                let mode = self
                    .mode
                    .ok_or(Error::Invariant("supported capability cell requires mode"))?;
                let mut seen_kinds = BTreeSet::new();
                for kind in &self.sources {
                    require(
                        seen_kinds.insert(*kind),
                        "duplicate capability cell Source kind",
                    )?;
                }
                // Sight sources are observation-only, so a mixed cell's
                // authority depends on whether any non-Sight source is
                // present, never on `self.sources` slice order.
                let representative = self
                    .sources
                    .iter()
                    .copied()
                    .find(|kind| *kind != SourceKind::Sight)
                    .unwrap_or(SourceKind::Sight);
                if representative == SourceKind::Sight {
                    require(
                        mode == HookAuthority::Observe,
                        "Sight-only capability cell cannot claim control mode",
                    )?;
                }
                HookPointCapability {
                    post_permission: false,
                    mode,
                    effects: self.effects.clone(),
                    failure_actions: self.failure_actions.clone(),
                }
                .validate_for_event(representative, &self.event)
            }
        }
    }
}

/// Projects the complete 16-Hook structured capability cell row for one framework adapter.
///
/// # Errors
/// Rejects invalid adapter identifiers or a [`SourceCapability`] whose adapter
/// disagrees with `adapter`.
pub fn project_capability_cells(
    adapter: &str,
    sources: &[SourceCapability],
) -> Result<BTreeMap<String, CapabilityCell>, Error> {
    require(
        valid_identifier(adapter),
        "invalid adapter identifier for capability projection",
    )?;
    for source in sources {
        source.validate()?;
        require(
            source.adapter() == adapter,
            "source capability adapter does not match projection target",
        )?;
    }
    let mut cells = BTreeMap::new();
    for event in HOOK_EVENTS {
        let mut kinds = BTreeSet::new();
        let mut effects = BTreeSet::new();
        let mut failure_actions = BTreeSet::new();
        let mut has_control = false;
        let mut has_any = false;
        for source in sources {
            if let Some(hook) = source.hook(event) {
                has_any = true;
                kinds.insert(source.kind());
                has_control |= hook.mode == HookAuthority::Control;
                effects.extend(hook.effects.iter().copied());
                failure_actions.extend(hook.failure_actions.iter().copied());
            }
        }
        let cell = if has_any {
            let mode = if has_control {
                HookAuthority::Control
            } else {
                HookAuthority::Observe
            };
            CapabilityCell {
                adapter: adapter.into(),
                event: event.into(),
                status: CapabilityStatus::Supported,
                sources: kinds.into_iter().collect(),
                mode: Some(mode),
                effects: effects.into_iter().collect(),
                failure_actions: failure_actions.into_iter().collect(),
            }
        } else {
            CapabilityCell {
                adapter: adapter.into(),
                event: event.into(),
                status: CapabilityStatus::Unsupported,
                sources: Vec::new(),
                mode: None,
                effects: Vec::new(),
                failure_actions: Vec::new(),
            }
        };
        cell.validate()?;
        cells.insert(event.into(), cell);
    }
    Ok(cells)
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

pub(crate) fn shared_registry() -> Result<&'static Registry, Error> {
    static REGISTRY: OnceLock<Result<Registry, Error>> = OnceLock::new();
    REGISTRY
        .get_or_init(Registry::new)
        .as_ref()
        .map_err(|_| Error::InvalidSchema(SCHEMA_NAME.into()))
}

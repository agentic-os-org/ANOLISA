//! Pure Hook route compilation over admitted steps and configuration budgets.
//!
//! Projects [`AdmittedStep`]s and event budgets into immutable [`PreparedRoutes`]
//! for Scope snapshots and O(1) SDK callback dispatch. Keep this module separate
//! from the independent [`crate::Core::prepare`] plan execution API.
//!
//! Compiled route types expose read-only accessors with no public struct constructor:
//!
//! ```compile_fail
//! let routes = aw_core::routes::PreparedRoutes {
//!     events: Default::default(),
//!     hooks: Vec::new(),
//! };
//! ```

use crate::admission::{matches_tool_selectors, AdapterCapabilities, AdmittedStep, StepExecution};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};

/// Synthetic observation step ID emitted when an observed event has no configured steps.
///
/// Uses a `$aw.` prefix so it cannot collide with any schema-validated step ID.
pub const OBSERVE_STEP: &str = "$aw.observe";

/// Default inclusive upper bound in milliseconds for per-event budgets.
pub const DEFAULT_MAX_BUDGET_MS: u64 = 55_000;

/// Standard four-event projection order used by existing SDK tool/submission bridges.
pub const STANDARD_EVENTS: &[&str] = &[
    "input.submit",
    "tool.before",
    "tool.after",
    "security.violation",
];

/// Tool events that emit [`OBSERVE_STEP`] when observation is enabled on an empty policy.
pub const OBSERVED_TOOL_EVENTS: &[&str] = &["tool.before", "tool.after"];

/// Canonical 16-Hook semantic lifecycle order for capability-driven Source profiles.
pub const SEMANTIC_EVENTS: &[&str] = &[
    "session.start",
    "input.submit",
    "model.before_request",
    "permission.request",
    "tool.before",
    "tool.after",
    "subagent.start",
    "subagent.stop",
    "compact.before",
    "compact.after",
    "security.violation",
    "coverage.changed",
    "runtime.observed",
    "turn.stop",
    "session.end",
    "runtime.exited",
];

/// Pure route compilation failures; diagnostics omit configuration values.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RouteError {
    /// Configured or default event budget is missing or non-numeric.
    #[error("invalid event budget")]
    InvalidBudget,
    /// Floating-point event budget has a non-zero fractional part.
    #[error("event budget must be an integer millisecond value")]
    NonIntegralBudget,
    /// Event budget falls outside `1..=max_ms` milliseconds.
    #[error("event budget must be within 1..={max_ms} ms")]
    BudgetOutOfRange {
        /// Inclusive upper bound in milliseconds for this route profile.
        max_ms: u64,
    },
    /// Result replacement requires sequential step execution.
    #[error("result replacement requires sequential step execution")]
    UnorderedReplacement,
    /// Policy requests `check_skill` on a Source without resolved Skill inspection.
    #[error("resolved Skill inspection is unsupported by this route profile")]
    UnsupportedSkillCheck,
}

/// Subscription projection policy for capability-scoped Source routes.
#[derive(Clone, Copy, Debug, Default)]
pub enum EventFilter<'a> {
    /// Emit all evaluated events that have steps or synthetic observation.
    #[default]
    All,
    /// Skip unsubscribed events before budget or observation checks.
    Subscribed(&'a BTreeMap<String, Vec<String>>),
    /// Validate budgets across observed events first, then project only subscribed events.
    ProjectAfterValidation(&'a BTreeMap<String, Vec<String>>),
}

/// Declarative rule for annotating steps that inspect a resolved Skill payload.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SkillCheck<'a> {
    /// Never emit `skill_check` annotations.
    #[default]
    None,
    /// Mark `tool.before` `check_skill` steps matching `native:<source_id>:<tool_name>` or `*`.
    Tool {
        /// Target source/adapter identifier used in `native:<id>:<tool>` selectors.
        source_id: &'a str,
        /// Native tool name carrying the Skill reference.
        tool_name: &'a str,
    },
    /// Mark `input.submit` steps whose Provider operation is `check_skill`.
    InputSubmit,
    /// Reject policies that request `check_skill` on a Source without Skill resolution.
    Unsupported,
}

/// Declarative Source routing parameters used to compile [`PreparedRoutes`].
#[derive(Clone, Copy, Debug)]
pub struct RouteOptions<'a> {
    /// Ordered semantic events to evaluate when building `hooks`.
    pub events: &'a [&'a str],
    /// Subset of `events` that register [`OBSERVE_STEP`] when no policy step applies.
    pub observe_events: &'a [&'a str],
    /// Subscription projection policy restricting emitted routes to declared Source events.
    pub event_filter: EventFilter<'a>,
    /// Inclusive upper bound in milliseconds for per-event budgets (`1..=max_budget_ms`).
    pub max_budget_ms: u64,
    /// Reverse per-event hook registration order when an event contains `replace_result`
    /// so onion-style middleware unwinds observers before final replacement.
    pub reverse_on_result_replacement: bool,
    /// Emit boolean `replace_result` on every `hooks` entry.
    pub include_replace_result: bool,
    /// Emit `replace_input: true` on steps requesting input replacement.
    pub include_replace_input: bool,
    /// Emit `event_tools` and `step_tools` selectors on every `hooks` entry.
    pub include_tool_selectors: bool,
    /// Declarative Skill inspection annotation or rejection rule.
    pub skill_check: SkillCheck<'a>,
}

impl<'a> RouteOptions<'a> {
    /// Builds standard capability-driven route options for a resident Source profile.
    pub fn standard(capabilities: &'a AdapterCapabilities) -> Self {
        Self {
            events: SEMANTIC_EVENTS,
            observe_events: OBSERVED_TOOL_EVENTS,
            event_filter: EventFilter::Subscribed(&capabilities.events),
            max_budget_ms: DEFAULT_MAX_BUDGET_MS,
            reverse_on_result_replacement: false,
            include_replace_result: true,
            include_replace_input: false,
            include_tool_selectors: false,
            skill_check: SkillCheck::Tool {
                source_id: &capabilities.adapter,
                tool_name: "Skill",
            },
        }
    }
}

/// Immutable compiled route for a single semantic Hook event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventRoute {
    budget_ms: u64,
    replacement_step: Option<String>,
    steps: HashMap<String, String>,
}

impl EventRoute {
    /// Whole-event budget in milliseconds.
    pub fn budget_ms(&self) -> u64 {
        self.budget_ms
    }

    /// Step ID authorized to replace the final tool text result, if any.
    pub fn replacement_step(&self) -> Option<&str> {
        self.replacement_step.as_deref()
    }

    /// Looks up a step's `on_error` action in O(1) time.
    pub fn step_on_error(&self, step_id: &str) -> Option<&str> {
        self.steps.get(step_id).map(String::as_str)
    }

    /// Returns a deterministic key-sorted copy of step IDs and their `on_error` actions.
    pub fn steps_btree(&self) -> BTreeMap<String, String> {
        self.steps
            .iter()
            .map(|(id, on_error)| (id.clone(), on_error.clone()))
            .collect()
    }

    /// Encodes this event route in the canonical `scope_policy` JSON shape.
    pub fn to_value(&self) -> Value {
        let steps: Map<String, Value> = self
            .steps
            .iter()
            .map(|(id, on_error)| (id.clone(), Value::String(on_error.clone())))
            .collect();
        let mut object = Map::new();
        object.insert("budget_ms".into(), json!(self.budget_ms));
        if let Some(replacement) = &self.replacement_step {
            object.insert("replacement_step".into(), json!(replacement));
        }
        object.insert("steps".into(), Value::Object(steps));
        Value::Object(object)
    }
}

/// Immutable per-step registration descriptor emitted in `hooks`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookRoute {
    event: String,
    step: String,
    on_error: String,
    budget_ms: u64,
    replace_result: Option<bool>,
    replace_input: bool,
    post_permission: bool,
    skill_check: bool,
    tool_selectors: Option<(Vec<String>, Vec<String>)>,
}

impl HookRoute {
    /// Semantic Hook event name.
    pub fn event(&self) -> &str {
        &self.event
    }

    /// Admitted or synthetic step identifier.
    pub fn step(&self) -> &str {
        &self.step
    }

    /// Failure action (`report`, `block`, or `withhold_result`).
    pub fn on_error(&self) -> &str {
        &self.on_error
    }

    /// Shared event budget in milliseconds.
    pub fn budget_ms(&self) -> u64 {
        self.budget_ms
    }

    /// Encodes this hook entry in the bridge's expected JSON shape.
    pub fn to_value(&self) -> Value {
        let mut hook = json!({
            "event": self.event,
            "step": self.step,
            "on_error": self.on_error,
            "budget_ms": self.budget_ms,
        });
        if let Some(replace_result) = self.replace_result {
            hook["replace_result"] = json!(replace_result);
        }
        if self.replace_input {
            hook["replace_input"] = json!(true);
        }
        if self.post_permission {
            hook["post_permission"] = json!(true);
        }
        if let Some((event_tools, step_tools)) = &self.tool_selectors {
            hook["event_tools"] = json!(event_tools);
            hook["step_tools"] = json!(step_tools);
        }
        if self.skill_check {
            hook["skill_check"] = json!(true);
        }
        hook
    }
}

/// Immutable route snapshot compiled once per Scope/Instance policy revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedRoutes {
    events: HashMap<String, EventRoute>,
    hooks: Vec<HookRoute>,
}

impl PreparedRoutes {
    /// Looks up a compiled [`EventRoute`] by event name in O(1) time.
    pub fn event(&self, name: &str) -> Option<&EventRoute> {
        self.events.get(name)
    }

    /// Iterates compiled event routes.
    pub fn events(&self) -> impl Iterator<Item = (&str, &EventRoute)> {
        self.events
            .iter()
            .map(|(name, route)| (name.as_str(), route))
    }

    /// Ordered hook registration descriptors.
    pub fn hooks(&self) -> &[HookRoute] {
        &self.hooks
    }

    /// Encodes the sanitized `{"events": ..., "hooks": ...}` Scope policy snapshot.
    pub fn to_policy_value(&self) -> Value {
        let events: Map<String, Value> = self
            .events
            .iter()
            .map(|(name, route)| (name.clone(), route.to_value()))
            .collect();
        let hooks: Vec<Value> = self.hooks.iter().map(HookRoute::to_value).collect();
        json!({
            "events": Value::Object(events),
            "hooks": hooks,
        })
    }
}

/// Step execution grouping strategy for dynamic native reload plans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupOrder {
    /// Run steps one group per step in configured order.
    Sequential,
    /// Reverse configured order unless an event step replaces the tool result.
    ReverseUnlessReplacement,
    /// Run all steps concurrently in one group; disallowed with result replacement.
    Parallel,
}

/// One step entry in a scheduled dynamic reload [`EventSchedule`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledStep {
    /// Configured step ID.
    pub id: String,
    /// Failure action (`report` or `block`).
    pub on_error: String,
}

/// Pure single-event schedule compiled for dynamic native reload plans.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventSchedule {
    /// Validated whole-event budget in milliseconds.
    pub budget_ms: u64,
    /// Ordered groups of concurrent steps.
    pub groups: Vec<Vec<ScheduledStep>>,
    /// Whether any step on this event replaces the final tool result.
    pub replacement: bool,
    /// Aggregated `on_error` action for matching Skill checks (`block` overrides `report`).
    pub skill_check: Option<String>,
}

/// Checks whether a `tool.before` step inspects a specific native Skill tool.
pub fn matches_skill_tool(step: &AdmittedStep, source_id: &str, tool_name: &str) -> bool {
    step.event == "tool.before"
        && matches!(&step.execution, StepExecution::Provider { operation, .. } if operation == "check_skill")
        && matches_tool_selectors(
            &step.event_tools,
            &step.step_tools,
            Some(source_id),
            Some(tool_name),
            Some(tool_name),
        )
}

/// Resolves and validates one event's millisecond budget within `1..=max_ms`.
///
/// # Errors
/// Returns [`RouteError::InvalidBudget`] for missing/non-numeric values,
/// [`RouteError::NonIntegralBudget`] for fractional floats, and
/// [`RouteError::BudgetOutOfRange`] when outside `1..=max_ms`.
pub fn event_budget_ms(document: &Value, event: &str, max_ms: u64) -> Result<u64, RouteError> {
    let field = document["spec"]["events"][event]
        .get("budget_ms")
        .unwrap_or(&document["spec"]["execution"]["default_event_budget_ms"]);
    if let Some(value) = field.as_u64() {
        return (1..=max_ms)
            .contains(&value)
            .then_some(value)
            .ok_or(RouteError::BudgetOutOfRange { max_ms });
    }
    if field.as_i64().is_some() {
        return Err(RouteError::BudgetOutOfRange { max_ms });
    }
    let raw = field.as_f64().ok_or(RouteError::InvalidBudget)?;
    const TWO_POW_64: f64 = 18_446_744_073_709_551_616.0;
    if !raw.is_finite() {
        return Err(RouteError::InvalidBudget);
    }
    if raw.fract() != 0.0 {
        return Err(RouteError::NonIntegralBudget);
    }
    if !(1.0..TWO_POW_64).contains(&raw) {
        return Err(RouteError::BudgetOutOfRange { max_ms });
    }
    let budget = raw as u64;
    (1..=max_ms)
        .contains(&budget)
        .then_some(budget)
        .ok_or(RouteError::BudgetOutOfRange { max_ms })
}

/// Whether this admitted step is an observation deferred by the event runtime.
pub fn is_shadow(document: &Value, step: &AdmittedStep) -> bool {
    aw_config::requirements::configured_step(document, &step.event, &step.step_id)
        .is_some_and(|configured| configured["mode"] == "shadow")
}

/// Select the native callbacks for an event.
///
/// Enforcing steps own the callback path. A shadow-only event retains one
/// neutral trigger so the Service can open the event and evaluate its shadows.
pub fn callback_steps<'a>(
    document: &Value,
    steps: &'a [AdmittedStep],
    event: &str,
) -> Vec<&'a AdmittedStep> {
    let mut selected: Vec<_> = steps.iter().filter(|step| step.event == event).collect();
    if selected.iter().any(|step| !is_shadow(document, step)) {
        selected.retain(|step| !is_shadow(document, step));
    } else {
        selected.truncate(1);
    }
    selected
}

/// Compiles [`AdmittedStep`]s and event budgets into an immutable [`PreparedRoutes`].
///
/// # Errors
/// Rejects missing, fractional or out-of-range budgets and unsupported `check_skill` steps.
pub fn compile(
    document: &Value,
    steps: &[AdmittedStep],
    options: &RouteOptions<'_>,
) -> Result<PreparedRoutes, RouteError> {
    let mut events = HashMap::new();
    let mut hooks = Vec::new();
    for &event in options.events {
        if matches!(options.event_filter, EventFilter::Subscribed(allowed) if !allowed.contains_key(event))
        {
            continue;
        }
        let selected = callback_steps(document, steps, event);
        let observe = options.observe_events.contains(&event);
        if selected.is_empty() && !observe {
            continue;
        }
        let budget = event_budget_ms(document, event, options.max_budget_ms)?;
        let retained = match options.event_filter {
            EventFilter::All | EventFilter::Subscribed(_) => true,
            EventFilter::ProjectAfterValidation(allowed) => allowed.contains_key(event),
        };
        if selected.is_empty() {
            if retained {
                hooks.push(HookRoute {
                    event: event.into(),
                    step: OBSERVE_STEP.into(),
                    on_error: "report".into(),
                    budget_ms: budget,
                    replace_result: options.include_replace_result.then_some(false),
                    replace_input: false,
                    post_permission: false,
                    skill_check: false,
                    tool_selectors: options
                        .include_tool_selectors
                        .then(|| (vec!["*".into()], vec!["*".into()])),
                });
                events.insert(
                    event.into(),
                    EventRoute {
                        budget_ms: budget,
                        replacement_step: None,
                        steps: HashMap::from([(OBSERVE_STEP.into(), "report".into())]),
                    },
                );
            }
            continue;
        }
        let any_replacement = selected.iter().any(|s| s.execution.replaces_result());
        let mut registration = selected.clone();
        if options.reverse_on_result_replacement && any_replacement {
            registration.reverse();
        }
        for step in registration {
            let skill_check = eval_skill_check(step, options.skill_check)?;
            if !retained {
                continue;
            }
            let replace_input = options.include_replace_input && step.execution.replaces_input();
            hooks.push(HookRoute {
                event: event.into(),
                step: step.step_id.clone(),
                on_error: step.on_error.clone(),
                budget_ms: budget,
                replace_result: options
                    .include_replace_result
                    .then(|| step.execution.replaces_result()),
                replace_input,
                post_permission: replace_input && step.post_permission,
                skill_check,
                tool_selectors: options.include_tool_selectors.then(|| {
                    if is_shadow(document, step) {
                        (vec!["*".into()], vec!["*".into()])
                    } else {
                        (step.event_tools.clone(), step.step_tools.clone())
                    }
                }),
            });
        }
        if retained {
            events.insert(
                event.into(),
                EventRoute {
                    budget_ms: budget,
                    replacement_step: selected
                        .iter()
                        .find(|s| s.execution.replaces_result())
                        .map(|s| s.step_id.clone()),
                    steps: selected
                        .into_iter()
                        .map(|s| (s.step_id.clone(), s.on_error.clone()))
                        .collect(),
                },
            );
        }
    }
    Ok(PreparedRoutes { events, hooks })
}

/// Compiles step groups and budget metadata for one dynamic native reload event.
///
/// # Errors
/// Rejects invalid/out-of-range budgets, parallel result replacement, and unsupported Skill checks.
pub fn schedule_event(
    document: &Value,
    steps: &[AdmittedStep],
    event: &str,
    max_budget_ms: u64,
    order: GroupOrder,
    skill_check: SkillCheck<'_>,
) -> Result<EventSchedule, RouteError> {
    let selected = callback_steps(document, steps, event);
    let replacement = selected.iter().any(|s| s.execution.replaces_result());
    let budget_ms = event_budget_ms(document, event, max_budget_ms)?;
    if replacement && order == GroupOrder::Parallel {
        return Err(RouteError::UnorderedReplacement);
    }
    let mut ordered: Vec<ScheduledStep> = selected
        .iter()
        .map(|s| ScheduledStep {
            id: s.step_id.clone(),
            on_error: s.on_error.clone(),
        })
        .collect();
    if order == GroupOrder::ReverseUnlessReplacement && !replacement {
        ordered.reverse();
    }
    let groups = match order {
        GroupOrder::Sequential | GroupOrder::ReverseUnlessReplacement => {
            ordered.into_iter().map(|step| vec![step]).collect()
        }
        GroupOrder::Parallel if ordered.is_empty() => Vec::new(),
        GroupOrder::Parallel => vec![ordered],
    };
    let mut resolved_skill = None;
    for step in &selected {
        if eval_skill_check(step, skill_check)? && resolved_skill.as_deref() != Some("block") {
            resolved_skill = Some(step.on_error.clone());
        }
    }
    Ok(EventSchedule {
        budget_ms,
        groups,
        replacement,
        skill_check: resolved_skill,
    })
}

fn eval_skill_check(step: &AdmittedStep, rule: SkillCheck<'_>) -> Result<bool, RouteError> {
    let checks_skill_op = matches!(
        &step.execution,
        StepExecution::Provider { operation, .. } if operation == "check_skill"
    );
    match rule {
        SkillCheck::None => Ok(false),
        SkillCheck::Tool {
            source_id,
            tool_name,
        } => Ok(matches_skill_tool(step, source_id, tool_name)),
        SkillCheck::InputSubmit => Ok(step.event == "input.submit" && checks_skill_op),
        SkillCheck::Unsupported if checks_skill_op => Err(RouteError::UnsupportedSkillCheck),
        SkillCheck::Unsupported => Ok(false),
    }
}

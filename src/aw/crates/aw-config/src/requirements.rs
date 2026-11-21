//! Target-specific desired Hook requirements, before capability admission.

use crate::{Configuration, Error};
use serde_json::Value;

/// One applicable enabled event, including events with no enabled steps.
///
/// This is desired policy, not proof of installed wiring or admitted capability.
/// Event order is unspecified; step order is the configured order.
#[derive(Debug, PartialEq)]
pub struct HookRequirement<'a> {
    /// Semantic Hook name from the validated configuration.
    pub event: &'a str,
    /// Whether configured availability requires this event.
    pub required: bool,
    /// Event budget, using the execution default when omitted on the event.
    pub budget_ms: u64,
    /// Event-level selectors, intersected with each step's selectors.
    pub event_tools: Vec<String>,
    /// Applicable enabled steps in their original order.
    pub steps: Vec<StepRequirement<'a>>,
    configuration: &'a Value,
}

impl HookRequirement<'_> {
    /// Original event fields, including guards that admission must still check.
    pub fn configuration(&self) -> &Value {
        self.configuration
    }
}

/// One desired step whose selector intersection may apply to the target.
#[derive(Debug, PartialEq)]
pub struct StepRequirement<'a> {
    /// Identifier within the event.
    pub step_id: &'a str,
    /// Configured Provider reference; projection never discovers it.
    pub provider: &'a str,
    /// Requested effects; native steps have no declared Provider effects.
    pub effects: Vec<&'a str>,
    /// Failure action, kept separate from requested effects.
    pub on_error: &'a str,
    /// Step-level selectors; omitted selectors mean all tools.
    pub step_tools: Vec<String>,
    configuration: &'a Value,
}

impl StepRequirement<'_> {
    /// Original step fields, retaining native, protocol and operation checks.
    pub fn configuration(&self) -> &Value {
        self.configuration
    }
}

/// Whether an applicable enabled event requires Service for this target.
///
/// Reuses [`project`], retaining empty events and uncertain native aliases while
/// excluding only requirements whose selectors cannot apply to this target.
///
/// # Errors
/// Rejects an unknown target or an inconsistent bundled configuration schema.
pub fn requires_service(config: &Configuration, target: &str) -> Result<bool, Error> {
    Ok(project(config, target)?.iter().any(|event| event.required))
}

/// Projects desired requirements for one configured target without invoking it.
///
/// All entry modes use this same projection. Disabled events and steps are
/// omitted. Exact foreign native selectors and disjoint normalized selectors
/// are excluded; unknown normalized/native aliases remain applicable. An event
/// with enabled steps all excluded is omitted. An applicable enabled event with
/// no enabled steps is retained, regardless of `required`.
///
/// # Errors
/// Rejects an unknown target or an inconsistent bundled configuration schema.
pub fn project<'a>(
    config: &'a Configuration,
    target: &str,
) -> Result<Vec<HookRequirement<'a>>, Error> {
    project_document(config.as_value(), target)
}

/// Projects a configuration snapshot already validated by the caller.
///
/// # Errors
/// Rejects an unknown target or a missing field required by the projection.
pub fn project_document<'a>(
    document: &'a Value,
    target: &str,
) -> Result<Vec<HookRequirement<'a>>, Error> {
    let spec = &document["spec"];
    let agent = spec["agents"].get(target).ok_or(Error::Invariant {
        path: "/spec/agents".into(),
        reason: "unknown configured Agent target",
    })?;
    let adapter = string(&agent["adapter"])?;
    let default_budget = &spec["execution"]["default_event_budget_ms"];
    let mut requirements = Vec::new();
    for (event_name, event) in spec["events"].as_object().ok_or(Error::InvalidSchema)? {
        if event["enabled"] != true {
            continue;
        }
        let event_tools = tool_selectors(event)?;
        if !selectors_overlap(&event_tools, &["*".into()], adapter) {
            continue;
        }
        let configured = event["steps"].as_array().ok_or(Error::InvalidSchema)?;
        let mut steps = Vec::new();
        let mut has_enabled_steps = false;
        for step in configured {
            if !step_enabled(step) {
                continue;
            }
            has_enabled_steps = true;
            let step_tools = tool_selectors(step)?;
            if !selectors_overlap(&event_tools, &step_tools, adapter) {
                continue;
            }
            let effects = step
                .get("effects")
                .map(|effects| {
                    effects
                        .as_array()
                        .ok_or(Error::InvalidSchema)?
                        .iter()
                        .map(string)
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?
                .unwrap_or_default();
            let effects = if step["mode"] == "shadow" {
                vec!["observe"]
            } else {
                effects
            };
            steps.push(StepRequirement {
                step_id: string(&step["id"])?,
                provider: string(&step["provider"])?,
                effects,
                on_error: if step["mode"] == "shadow" {
                    "report"
                } else {
                    string(&step["on_error"])?
                },
                step_tools,
                configuration: step,
            });
        }
        // An explicitly empty enabled event still requires an Adapter entrypoint.
        // An off step does not introduce that requirement when no step applies.
        if steps.is_empty()
            && (has_enabled_steps || configured.iter().any(|step| step["mode"] == "off"))
        {
            continue;
        }
        requirements.push(HookRequirement {
            event: event_name,
            required: event["required"] == true,
            budget_ms: positive(event.get("budget_ms").unwrap_or(default_budget))?,
            event_tools,
            steps,
            configuration: event,
        });
    }
    Ok(requirements)
}

/// Finds the original configuration for a projected step.
pub fn configured_step<'a>(document: &'a Value, event: &str, step_id: &str) -> Option<&'a Value> {
    document["spec"]["events"][event]["steps"]
        .as_array()?
        .iter()
        .find(|step| step["id"] == step_id)
}

/// Whether the step is enabled by its switch and execution mode.
pub fn step_enabled(step: &Value) -> bool {
    step["enabled"] != false && step["mode"] != "off"
}

/// Lists executor routes that can be applied by this configured step.
pub fn active_obligation_routes(step: &Value) -> impl Iterator<Item = (&String, &Value)> {
    step["obligations"]
        .as_object()
        .filter(|_| step_enabled(step) && step["mode"] != "shadow")
        .into_iter()
        .flatten()
}

/// Conservatively intersects selectors before an actual native tool is known.
///
/// Inputs must be validated tool selectors. Foreign native owners never match;
/// normalized/native aliases remain possible until the Adapter supplies identity.
pub fn selectors_overlap(outer: &[String], inner: &[String], adapter: &str) -> bool {
    outer.iter().any(|left| {
        inner.iter().any(|right| {
            let left_native = native(left);
            let right_native = native(right);
            if [left_native.as_ref(), right_native.as_ref()]
                .into_iter()
                .flatten()
                .any(|(owner, _)| *owner != adapter)
            {
                return false;
            }
            if left == "*" || right == "*" {
                return true;
            }
            match (left_native, right_native) {
                (Some((_, left)), Some((_, right))) => left == "*" || right == "*" || left == right,
                (None, None) => left == right,
                _ => true,
            }
        })
    })
}

/// Evaluates one configured selector against a normalized tool and its native identity.
pub fn matches_tool_selector(
    selector: &str,
    adapter: Option<&str>,
    tool_name: Option<&str>,
    native_name: Option<&str>,
) -> bool {
    if selector == "*" {
        return true;
    }
    if let Some((owner, name)) = native(selector) {
        return adapter == Some(owner)
            && native_name.is_some_and(|native| name == "*" || native == name);
    }
    tool_name == Some(selector)
}

fn native(selector: &str) -> Option<(&str, &str)> {
    selector
        .strip_prefix("native:")
        .and_then(|value| value.split_once(':'))
}

fn positive(value: &Value) -> Result<u64, Error> {
    // JSON Schema integers include integral decimal and exponent forms. The
    // configured u32 range is exactly representable as f64, as in Host budgets.
    value
        .as_f64()
        .filter(|value| *value >= 1.0 && *value <= f64::from(u32::MAX) && value.fract() == 0.0)
        .map(|value| value as u64)
        .ok_or(Error::InvalidSchema)
}

fn string(value: &Value) -> Result<&str, Error> {
    value.as_str().ok_or(Error::InvalidSchema)
}

fn tool_selectors(value: &Value) -> Result<Vec<String>, Error> {
    match value["match"].get("tools") {
        None => Ok(vec!["*".into()]),
        Some(tools) => tools
            .as_array()
            .ok_or(Error::InvalidSchema)?
            .iter()
            .map(|tool| string(tool).map(str::to_owned))
            .collect(),
    }
}

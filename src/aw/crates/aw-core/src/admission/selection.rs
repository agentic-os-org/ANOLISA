//! Conservative selector intersection for a fixed Adapter, before native tool data exists.

use super::rules::{string, tool_selectors};
use super::AdmissionError;

/// An enabled step whose selectors cannot overlap for a configured target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectedOutStep {
    /// The configured event containing the step.
    pub event: String,
    /// The step identifier within its event.
    pub step_id: String,
}

/// Lists enabled steps excluded by the same selector intersection as [`super::preflight`].
///
/// Disabled events and steps are omitted. Unknown normalized/native aliases stay
/// applicable. This offline diagnostic checks no capabilities and invokes no
/// Provider; the supplied configuration has already passed global validation.
///
/// # Errors
/// Rejects an unknown target or malformed configuration fields.
pub fn projected_out_steps(
    config: &aw_config::Configuration,
    target: &str,
) -> Result<Vec<ProjectedOutStep>, AdmissionError> {
    let spec = &config.as_value()["spec"];
    let agent = spec["agents"]
        .get(target)
        .ok_or(AdmissionError::Rejected("unknown configured Agent target"))?;
    let adapter = string(&agent["adapter"])?;
    let events = spec["events"].as_object().ok_or(AdmissionError::Rejected(
        "configuration events must be an object",
    ))?;
    let mut excluded = Vec::new();
    for (event_name, event) in events {
        if event["enabled"] != true {
            continue;
        }
        let event_tools = tool_selectors(event)?;
        for step in event["steps"]
            .as_array()
            .ok_or(AdmissionError::Rejected("event steps must be an array"))?
        {
            if step["enabled"] != false && !overlap(&event_tools, &tool_selectors(step)?, adapter) {
                excluded.push(ProjectedOutStep {
                    event: event_name.clone(),
                    step_id: string(&step["id"])?.to_owned(),
                });
            }
        }
    }
    Ok(excluded)
}

pub(super) fn overlap(outer: &[String], inner: &[String], adapter: &str) -> bool {
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
                (Some((_, left)), Some((_, right))) => left == right,
                (None, None) => left == right,
                // Normalized/native aliases belong to the Adapter. Without a
                // native event, this intersection must retain strict admission.
                _ => true,
            }
        })
    })
}

fn native(selector: &str) -> Option<(&str, &str)> {
    selector
        .strip_prefix("native:")
        .and_then(|value| value.split_once(':'))
}

/// Evaluates whether event-level and step-level selectors admit the given tool identity.
pub fn matches_tool_selectors(
    event_tools: &[String],
    step_tools: &[String],
    adapter: Option<&str>,
    tool_name: Option<&str>,
    native_name: Option<&str>,
) -> bool {
    let matches = |selectors: &[String]| {
        selectors.iter().any(|selector| {
            if selector == "*" {
                return true;
            }
            if let Some((owner, name)) = native(selector) {
                return adapter == Some(owner) && native_name == Some(name);
            }
            tool_name == Some(selector.as_str())
        })
    };
    matches(event_tools) && matches(step_tools)
}

//! Target projection diagnostics and runtime tool selector matching.

use super::rules::string;
use super::AdmissionError;
use aw_config::requirements::{matches_tool_selector, project, step_enabled};
use std::collections::HashSet;

/// An enabled step excluded by target or tool selectors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectedOutStep {
    /// The configured event containing the step.
    pub event: String,
    /// The step identifier within its event.
    pub step_id: String,
}

/// Lists enabled steps excluded by the same target projection as [`super::preflight`].
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
    let mut applicable = HashSet::new();
    for requirement in project(config, target)? {
        for step in requirement.steps {
            applicable.insert((requirement.event, step.step_id));
        }
    }
    let spec = &config.as_value()["spec"];
    let events = spec["events"].as_object().ok_or(AdmissionError::Rejected(
        "configuration events must be an object",
    ))?;
    let mut excluded = Vec::new();
    for (event_name, event) in events {
        if event["enabled"] != true {
            continue;
        }
        for step in event["steps"]
            .as_array()
            .ok_or(AdmissionError::Rejected("event steps must be an array"))?
        {
            if !step_enabled(step) {
                continue;
            }
            let step_id = string(&step["id"])?;
            if !applicable.contains(&(event_name.as_str(), step_id)) {
                excluded.push(ProjectedOutStep {
                    event: event_name.clone(),
                    step_id: step_id.to_owned(),
                });
            }
        }
    }
    Ok(excluded)
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
        selectors
            .iter()
            .any(|selector| matches_tool_selector(selector, adapter, tool_name, native_name))
    };
    matches(event_tools) && matches(step_tools)
}

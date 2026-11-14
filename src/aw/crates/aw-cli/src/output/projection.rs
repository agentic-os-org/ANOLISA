//! Offline target projection diagnostics contain only bounded configuration identifiers.

use serde::Serialize;
use std::io::Write;

#[derive(Debug)]
pub(crate) enum Error {
    Admission(aw_core::admission::AdmissionError),
    StepLimit { limit: usize },
}

impl From<aw_core::admission::AdmissionError> for Error {
    fn from(error: aw_core::admission::AdmissionError) -> Self {
        Self::Admission(error)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Admission(error) => error.fmt(formatter),
            Self::StepLimit { limit } => write!(
                formatter,
                "configured target exceeds maximum_steps_per_binding: {limit}"
            ),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Serialize)]
pub(crate) struct ProjectedOutStep {
    target: String,
    event: String,
    step_id: String,
}

pub(crate) fn collect(config: &aw_config::Configuration) -> Result<Vec<ProjectedOutStep>, Error> {
    let agents = config.as_value()["spec"]["agents"].as_object().ok_or(
        aw_core::admission::AdmissionError::Rejected("configuration agents must be an object"),
    )?;
    let enabled_count = config.as_value()["spec"]["events"]
        .as_object()
        .ok_or(aw_core::admission::AdmissionError::Rejected(
            "configuration events must be an object",
        ))?
        .values()
        .filter(|event| event["enabled"] == true)
        .filter_map(|event| event["steps"].as_array())
        .flatten()
        .filter(|step| step["enabled"] != false)
        .count();
    let mut steps = Vec::new();
    for target in agents.keys() {
        let excluded = aw_core::admission::projected_out_steps(config, target)?;
        // Projection excludes only enabled steps from the same validated document.
        if enabled_count - excluded.len() > aw_service::MAX_STEPS_PER_BINDING {
            return Err(Error::StepLimit {
                limit: aw_service::MAX_STEPS_PER_BINDING,
            });
        }
        for step in excluded {
            steps.push(ProjectedOutStep {
                target: target.clone(),
                event: step.event,
                step_id: step.step_id,
            });
        }
    }
    Ok(steps)
}

pub(crate) fn human(steps: &[ProjectedOutStep], output: &mut impl Write) -> std::io::Result<()> {
    for step in steps {
        writeln!(
            output,
            "target {}: projected out {}/{} (tool selectors do not overlap)",
            super::terminal::escape(&step.target),
            super::terminal::escape(&step.event),
            super::terminal::escape(&step.step_id),
        )?;
    }
    Ok(())
}

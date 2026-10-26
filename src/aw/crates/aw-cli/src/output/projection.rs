//! Offline target projection diagnostics contain only bounded configuration identifiers.

use serde::Serialize;
use std::io::Write;

#[derive(Debug, Serialize)]
pub(crate) struct ProjectedOutStep {
    target: String,
    event: String,
    step_id: String,
}

pub(crate) fn collect(
    config: &aw_config::Configuration,
) -> Result<Vec<ProjectedOutStep>, aw_provider::Error> {
    let agents =
        config.as_value()["spec"]["agents"]
            .as_object()
            .ok_or(aw_provider::Error::Invalid(
                "configuration agents must be an object",
            ))?;
    let mut steps = Vec::new();
    for target in agents.keys() {
        for step in aw_provider::admission::projected_out_steps(config, target)? {
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

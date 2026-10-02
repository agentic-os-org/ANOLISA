//! Validation of Gateway Task snapshot projections.

use serde_json::Value;

use super::view::safe_field;

#[derive(Debug)]
pub(super) struct TaskProjection {
    pub(super) state: String,
    pub(super) revision: u64,
}

pub(super) fn task_projection(value: &Value, task_id: &str) -> Result<TaskProjection, String> {
    let response_task_id = value
        .get("task_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "Gateway snapshot response did not contain a Task ID".to_owned())?;
    if response_task_id != task_id {
        return Err("Gateway snapshot response did not match the requested Task".to_owned());
    }
    let state = value
        .get("state")
        .and_then(Value::as_str)
        .ok_or_else(|| "Gateway snapshot response did not contain a lifecycle state".to_owned())?;
    let revision = value
        .get("revision")
        .and_then(Value::as_u64)
        .ok_or_else(|| "Gateway snapshot response did not contain a revision".to_owned())?;
    Ok(TaskProjection {
        state: state.to_owned(),
        revision,
    })
}

pub(super) fn terminal_task_projection(
    value: &Value,
    task_id: &str,
) -> Result<TaskProjection, String> {
    let projection = task_projection(value, task_id)?;
    if !is_terminal_task_state(&projection.state) {
        return Err(format!(
            "Task {} is {}; switching snapshots is available only after the Task reaches a terminal state",
            safe_field(task_id),
            safe_field(&projection.state)
        ));
    }
    Ok(projection)
}

pub(super) fn is_terminal_task_state(state: &str) -> bool {
    matches!(state, "succeeded" | "failed" | "cancelled")
}

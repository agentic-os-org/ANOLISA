//! Input decoding and selection indexes for Task form navigation.

use crate::runtime::prelude::ShellEvent;

use super::{
    TaskCapabilities, TaskCheckpoint, TaskFormPhase, TaskLaunchForm, TaskRuntime,
    CONFIRM_OPTION_COUNT,
};

pub(super) fn parse_id_value(event: &ShellEvent) -> Option<(String, usize)> {
    let (id, value) = event.input.as_deref()?.split_once(':')?;
    Some((id.trim().to_owned(), value.trim().parse().ok()?))
}

pub(super) fn parse_id_text(event: &ShellEvent) -> Option<(String, String)> {
    let (id, text) = event.input.as_deref()?.split_once(':')?;
    Some((id.trim().to_owned(), text.to_owned()))
}

pub(super) fn form_option_count(form: &TaskLaunchForm) -> usize {
    match form.phase {
        TaskFormPhase::Goal => 0,
        TaskFormPhase::Runtime => form.capabilities.ready_runtimes().len(),
        TaskFormPhase::Checkpoint => form.capabilities.checkpoint_options().len(),
        TaskFormPhase::Confirm => CONFIRM_OPTION_COUNT,
    }
}

pub(super) fn runtime_index(capabilities: &TaskCapabilities, runtime: TaskRuntime) -> usize {
    capabilities
        .ready_runtimes()
        .iter()
        .position(|candidate| *candidate == runtime)
        .unwrap_or(0)
}

pub(super) fn checkpoint_index(
    capabilities: &TaskCapabilities,
    checkpoint: TaskCheckpoint,
) -> usize {
    capabilities
        .checkpoint_options()
        .iter()
        .position(|candidate| *candidate == checkpoint)
        .unwrap_or(0)
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EcsTrialContext {
    pub(crate) schema_version: u32,
    pub(crate) shell_session_id: String,
    pub(crate) intent: TrialIntent,
    pub(crate) auto_use_allowed: bool,
}

impl EcsTrialContext {
    pub(crate) fn valid(&self) -> bool {
        self.schema_version == 1
            && !self.shell_session_id.is_empty()
            && self.shell_session_id.len() <= 128
            && !self.shell_session_id.chars().any(char::is_control)
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TrialIntent {
    UserInitiated,
    InternalBestEffort,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct EcsTrialControl {
    pub(crate) run_id: String,
    pub(crate) flow_id: String,
    pub(crate) revision: u64,
    pub(crate) action: TrialAction,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TrialAction {
    Pause,
    Resume,
    Cancel,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct EcsTrialProgress {
    pub(crate) run_id: String,
    pub(crate) flow_id: String,
    pub(crate) revision: u64,
    pub(crate) phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) code: Option<String>,
    pub(crate) dispatch_state: &'static str,
    pub(crate) local_terminated: bool,
    pub(crate) resume_allowed: bool,
}

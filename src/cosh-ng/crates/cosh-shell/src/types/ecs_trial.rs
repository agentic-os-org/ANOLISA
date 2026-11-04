use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EcsTrialContext {
    pub schema_version: u32,
    pub shell_session_id: String,
    pub intent: String,
    pub auto_use_allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EcsTrialProgress {
    pub run_id: String,
    pub flow_id: String,
    pub revision: u64,
    pub phase: String,
    #[serde(default)]
    pub code: Option<String>,
    pub dispatch_state: String,
    pub local_terminated: bool,
    pub resume_allowed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EcsTrialAction {
    Pause,
    Resume,
    Cancel,
}

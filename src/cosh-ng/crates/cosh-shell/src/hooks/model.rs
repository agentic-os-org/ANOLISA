use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
pub enum HookTrigger {
    OnComplete,
    OnSuccess,
    OnFail,
}

#[derive(Debug, Clone)]
pub struct HookMatcher {
    pub id: String,
    pub commands: Vec<String>,
    pub command_patterns: Vec<String>,
    pub command_regex: Option<String>,
    pub exit_codes: Option<Vec<i32>>,
    pub min_output_bytes: Option<u64>,
    pub trigger: HookTrigger,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookInput {
    pub command: String,
    pub cwd: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub output_ref: Option<String>,
    pub output_bytes: u64,
    pub output_preview: String,
}

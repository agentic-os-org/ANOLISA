use super::{AgentMode, AgentRequest};

pub(crate) const USER_APPROVAL_MODE_HINT_PREFIX: &str = "__cosh_user_approval_mode=";
pub(crate) const SHELL_HANDOFF_CONTINUATION_HINT: &str =
    "analysis-only continuation after foreground shell handoff";

pub(crate) fn request_is_analysis_only_continuation(request: &AgentRequest) -> bool {
    request.mode == AgentMode::RecommendOnly
        && request
            .context_hints
            .iter()
            .any(|hint| hint.contains(SHELL_HANDOFF_CONTINUATION_HINT))
}

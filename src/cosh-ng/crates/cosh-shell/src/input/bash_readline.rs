use super::InputClassifier;

impl InputClassifier {
    pub(crate) fn with_bash_slash_submission_guard(mut self, enabled: bool) -> Self {
        self.bash_slash_submission_guard = enabled;
        self
    }

    pub(crate) fn bash_slash_submission_guard_enabled(&self) -> bool {
        self.bash_slash_submission_guard
    }

    pub(crate) fn with_bash_legacy_readline(mut self, enabled: bool) -> Self {
        self.bash_legacy_readline = enabled;
        self
    }

    pub(crate) fn bash_legacy_readline(&self) -> bool {
        self.bash_legacy_readline
    }
}

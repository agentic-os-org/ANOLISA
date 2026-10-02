use std::fs::{self, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
pub(crate) mod composer;
pub(crate) mod path_prompt;
pub(crate) use path_prompt::{PathPromptIntercept, ShellPathCommandNames, ShellPromptCwd};

#[derive(Debug, Clone)]
pub(crate) struct AssistanceControl {
    enabled: Arc<AtomicBool>,
    at_prompt: Arc<AtomicBool>,
    state_file: PathBuf,
}

impl AssistanceControl {
    pub(crate) fn enabled(state_file: PathBuf) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(true)),
            at_prompt: Arc::new(AtomicBool::new(false)),
            state_file,
        }
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub(crate) fn set_at_prompt(&self, at_prompt: bool) {
        self.at_prompt.store(at_prompt, Ordering::Release);
    }

    pub(crate) fn is_at_prompt(&self) -> bool {
        self.at_prompt.load(Ordering::Acquire)
    }

    pub(crate) fn toggle(&self) -> std::io::Result<bool> {
        self.set_enabled(!self.is_enabled())
    }

    pub(crate) fn set_enabled(&self, enabled: bool) -> std::io::Result<bool> {
        if enabled {
            if let Err(error) = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&self.state_file)
            {
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
        } else if let Err(error) = fs::remove_file(&self.state_file) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error);
            }
        }
        self.enabled.store(enabled, Ordering::Release);
        Ok(enabled)
    }
}

impl PartialEq for AssistanceControl {
    fn eq(&self, other: &Self) -> bool {
        self.state_file == other.state_file
            && self.is_enabled() == other.is_enabled()
            && self.is_at_prompt() == other.is_at_prompt()
    }
}

impl Eq for AssistanceControl {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputClassifier {
    slash_commands: Vec<String>,
    slash_hint_commands: Vec<String>,
    ai_enabled: bool,
    shell_passthrough: bool,
    bash_readline_history_privacy: bool,
    bash_slash_submission_guard: bool,
    assistance_control: Option<AssistanceControl>,
    shell_prompt_cwd: ShellPromptCwd,
    shell_path_command_names: ShellPathCommandNames,
}

impl InputClassifier {
    pub fn with_ai_enabled(mut self, ai_enabled: bool) -> Self {
        self.ai_enabled = ai_enabled;
        self
    }

    pub(crate) fn with_shell_passthrough(mut self, shell_passthrough: bool) -> Self {
        self.shell_passthrough = shell_passthrough;
        self
    }

    pub(crate) fn with_bash_readline_history_privacy(mut self, enabled: bool) -> Self {
        self.bash_readline_history_privacy = enabled;
        self
    }

    pub(crate) fn with_bash_slash_submission_guard(mut self, enabled: bool) -> Self {
        self.bash_slash_submission_guard = enabled;
        self
    }

    pub(crate) fn with_assistance_control(mut self, control: AssistanceControl) -> Self {
        self.assistance_control = Some(control);
        self
    }

    pub(crate) fn prompt_cwd(&self) -> ShellPromptCwd {
        self.shell_prompt_cwd.clone()
    }
    pub(crate) fn shell_path_command_names(&self) -> ShellPathCommandNames {
        self.shell_path_command_names.clone()
    }
}

impl Default for InputClassifier {
    fn default() -> Self {
        Self {
            slash_commands: crate::slash::registry::exact_slash_control_commands()
                .map(str::to_string)
                .collect(),
            slash_hint_commands: crate::slash::registry::active_slash_hint_commands()
                .map(str::to_string)
                .collect(),
            ai_enabled: true,
            shell_passthrough: false,
            bash_readline_history_privacy: false,
            bash_slash_submission_guard: false,
            assistance_control: None,
            shell_prompt_cwd: ShellPromptCwd::default(),
            shell_path_command_names: ShellPathCommandNames::default(),
        }
    }
}

impl InputClassifier {
    pub(crate) fn ai_enabled(&self) -> bool {
        self.ai_enabled
    }

    pub(crate) fn shell_owns_input(&self) -> bool {
        self.shell_passthrough
    }

    pub(crate) fn bash_readline_history_privacy_enabled(&self) -> bool {
        self.bash_readline_history_privacy
    }

    pub(crate) fn bash_slash_submission_guard_enabled(&self) -> bool {
        self.bash_slash_submission_guard
    }

    pub(crate) fn assistance_control(&self) -> Option<&AssistanceControl> {
        self.assistance_control.as_ref()
    }

    pub(crate) fn assistance_enabled(&self) -> bool {
        self.assistance_control
            .as_ref()
            .is_none_or(AssistanceControl::is_enabled)
    }

    pub(crate) fn classify_missing_path_submission(
        &self,
        input: &str,
    ) -> Option<PathPromptIntercept> {
        if self.shell_passthrough || !self.ai_enabled || !self.assistance_enabled() {
            return None;
        }
        let shell_cwd = self.shell_prompt_cwd.current()?;
        path_prompt::is_slash_bearing_han_prompt(input, Some(std::path::Path::new(&shell_cwd)))
            .then(|| PathPromptIntercept {
                input: input.to_string(),
                reason: InterceptReason::NaturalLanguage,
                cwd: shell_cwd,
            })
    }

    pub(crate) fn is_slash_control_candidate(&self, token: &str) -> bool {
        if !self.assistance_enabled() {
            return token == "/" || "/mode".starts_with(token);
        }
        self.is_slash_control_input(token)
    }

    /// Exact registry hits only, excluding hint prefixes such as `/sk`. The
    /// shell marker's case lists carry the same exact tokens, so only these
    /// submissions can be routed through the shell for history recording
    /// (issue #1718) with a guaranteed shell-side intercept.
    pub(crate) fn is_exact_slash_control_command(&self, token: &str) -> bool {
        self.slash_commands.iter().any(|command| token == command)
    }

    pub fn classify(&self, input: &str) -> InputDecision {
        if self.shell_passthrough {
            return InputDecision::SendToShell(input.to_string());
        }
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return InputDecision::SendToShell(input.to_string());
        }

        if !self.assistance_enabled() {
            let mut tokens = trimmed.split_whitespace();
            if tokens.next() == Some("/mode") && tokens.next() == Some("routing") {
                return InputDecision::Intercept {
                    input: input.to_string(),
                    reason: InterceptReason::Slash,
                };
            }
            return InputDecision::SendToShell(input.to_string());
        }

        let first_token = trimmed.split_whitespace().next().unwrap_or_default();
        if self.is_slash_control_input(first_token) {
            return InputDecision::Intercept {
                input: input.to_string(),
                reason: InterceptReason::Slash,
            };
        }

        if trimmed.starts_with("??") {
            if !self.ai_enabled {
                return InputDecision::Consume;
            }
            return InputDecision::Intercept {
                input: input.to_string(),
                reason: InterceptReason::AgentMarker,
            };
        }

        InputDecision::SendToShell(input.to_string())
    }

    fn is_slash_control_input(&self, token: &str) -> bool {
        if self.slash_commands.iter().any(|command| token == command) {
            return true;
        }

        is_slash_hint_candidate(token, &self.slash_hint_commands)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputDecision {
    SendToShell(String),
    Intercept {
        input: String,
        reason: InterceptReason,
    },
    Consume,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterceptReason {
    Slash,
    NaturalLanguage,
    AgentMarker,
    PromptGhost,
}

impl InterceptReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Slash => "slash",
            Self::NaturalLanguage => "natural_language",
            Self::AgentMarker => "agent_marker",
            Self::PromptGhost => "prompt_ghost",
        }
    }
}

fn is_slash_hint_candidate(token: &str, slash_commands: &[String]) -> bool {
    if token == "/" {
        return true;
    }
    if !token.starts_with('/') || token[1..].contains('/') {
        return false;
    }
    if std::path::Path::new(token).exists() {
        return false;
    }
    slash_commands
        .iter()
        .any(|command| command.starts_with(token) || edit_distance_at_most(token, command, 2))
}

fn edit_distance_at_most(left: &str, right: &str, max: usize) -> bool {
    left.len().abs_diff(right.len()) <= max && edit_distance(left, right) <= max
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right_chars = right.chars().collect::<Vec<_>>();
    let mut prev = (0..=right_chars.len()).collect::<Vec<_>>();
    let mut curr = vec![0; right_chars.len() + 1];

    for (left_idx, left_ch) in left.chars().enumerate() {
        curr[0] = left_idx + 1;
        for (right_idx, right_ch) in right_chars.iter().enumerate() {
            let cost = usize::from(left_ch != *right_ch);
            curr[right_idx + 1] = (prev[right_idx + 1] + 1)
                .min(curr[right_idx] + 1)
                .min(prev[right_idx] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[right_chars.len()]
}

#[cfg(test)]
mod tests {
    use super::{AssistanceControl, InputClassifier, InputDecision, InterceptReason};

    #[test]
    fn classifies_known_slash_commands_without_capturing_paths() {
        let classifier = InputClassifier::default();
        assert_eq!(
            classifier.classify("/explain last error"),
            InputDecision::Intercept {
                input: "/explain last error".to_string(),
                reason: InterceptReason::Slash
            }
        );
        assert_eq!(
            classifier.classify("/tmp/tool --help"),
            InputDecision::SendToShell("/tmp/tool --help".to_string())
        );
        assert_eq!(
            classifier.classify("/select 1"),
            InputDecision::Intercept {
                input: "/select 1".to_string(),
                reason: InterceptReason::Slash
            }
        );
        assert_eq!(
            classifier.classify("/allow 1"),
            InputDecision::Intercept {
                input: "/allow 1".to_string(),
                reason: InterceptReason::Slash
            }
        );
        assert_eq!(
            classifier.classify("/approve 1"),
            InputDecision::Intercept {
                input: "/approve 1".to_string(),
                reason: InterceptReason::Slash
            }
        );
        assert_eq!(
            classifier.classify("/deny 1"),
            InputDecision::Intercept {
                input: "/deny 1".to_string(),
                reason: InterceptReason::Slash
            }
        );
        assert_eq!(
            classifier.classify("/answer Blue"),
            InputDecision::Intercept {
                input: "/answer Blue".to_string(),
                reason: InterceptReason::Slash
            }
        );
        assert_eq!(
            classifier.classify("/approval-mode auto"),
            InputDecision::Intercept {
                input: "/approval-mode auto".to_string(),
                reason: InterceptReason::Slash
            }
        );
        assert_eq!(
            classifier.classify("/approval"),
            InputDecision::SendToShell("/approval".to_string())
        );
        assert_eq!(
            classifier.classify("/hep"),
            InputDecision::Intercept {
                input: "/hep".to_string(),
                reason: InterceptReason::Slash
            }
        );
        assert_eq!(
            classifier.classify("/sk"),
            InputDecision::Intercept {
                input: "/sk".to_string(),
                reason: InterceptReason::Slash
            }
        );
        assert_eq!(
            classifier.classify("/cancel"),
            InputDecision::Intercept {
                input: "/cancel".to_string(),
                reason: InterceptReason::Slash
            }
        );
        assert_eq!(
            classifier.classify("/"),
            InputDecision::Intercept {
                input: "/".to_string(),
                reason: InterceptReason::Slash
            }
        );
        assert_eq!(
            classifier.classify("/mo"),
            InputDecision::Intercept {
                input: "/mo".to_string(),
                reason: InterceptReason::Slash
            }
        );
        assert_eq!(
            classifier.classify("/modd"),
            InputDecision::Intercept {
                input: "/modd".to_string(),
                reason: InterceptReason::Slash
            }
        );
        assert_eq!(
            classifier.classify("/tmp/tool --help"),
            InputDecision::SendToShell("/tmp/tool --help".to_string())
        );
        assert_eq!(
            classifier.classify("/tmp"),
            InputDecision::SendToShell("/tmp".to_string())
        );
        assert_eq!(
            classifier.classify("/details req-1"),
            InputDecision::Intercept {
                input: "/details req-1".to_string(),
                reason: InterceptReason::Slash
            }
        );
    }

    #[test]
    fn classifies_ordinary_and_marker_inputs() {
        let classifier = InputClassifier::default();
        assert_eq!(
            classifier.classify("\u{5e2e}\u{6211}\u{5206}\u{6790}"),
            InputDecision::SendToShell("\u{5e2e}\u{6211}\u{5206}\u{6790}".to_string())
        );
        assert_eq!(
            classifier.classify("?? last command"),
            InputDecision::Intercept {
                input: "?? last command".to_string(),
                reason: InterceptReason::AgentMarker
            }
        );
        assert_eq!(
            classifier.classify("echo why not"),
            InputDecision::SendToShell("echo why not".to_string())
        );
    }

    #[test]
    fn ordinary_input_is_always_shell_first() {
        for classifier in [
            InputClassifier::default(),
            InputClassifier::default().with_ai_enabled(false),
        ] {
            for input in [
                "Who are you",
                "how file",
                "please explain this",
                "\u{5e2e}\u{6211}\u{770b}\u{770b}\u{5f53}\u{524d}\u{76ee}\u{5f55}",
            ] {
                assert_eq!(
                    classifier.classify(input),
                    InputDecision::SendToShell(input.to_string()),
                    "{input:?}"
                );
            }
        }
    }

    #[test]
    fn classifies_shell_commands_with_non_ascii_arguments_as_shell() {
        let classifier = InputClassifier::default();
        let chinese_doc = "\u{8bbe}\u{8ba1}\u{6587}\u{6863}.md";
        let escaped_vim_path = format!(
            "vim cosh-ng\\ AI\\ Shell\\ \\{}\\ {}",
            "\u{2014}", chinese_doc
        );
        assert_eq!(
            classifier.classify(&format!("cat {chinese_doc}")),
            InputDecision::SendToShell(format!("cat {chinese_doc}"))
        );
        assert_eq!(
            classifier.classify(&escaped_vim_path),
            InputDecision::SendToShell(escaped_vim_path)
        );
        assert_eq!(
            classifier.classify("echo \u{4f60}\u{597d}"),
            InputDecision::SendToShell("echo \u{4f60}\u{597d}".to_string())
        );
        assert_eq!(
            classifier.classify(&format!("printf ok > /tmp/{chinese_doc}")),
            InputDecision::SendToShell(format!("printf ok > /tmp/{chinese_doc}"))
        );
        assert_eq!(
            classifier.classify(&format!("LC_ALL=C cat {chinese_doc}")),
            InputDecision::SendToShell(format!("LC_ALL=C cat {chinese_doc}"))
        );
    }

    #[test]
    fn routing_classifier_intercepts_agent_marker() {
        let c = InputClassifier::default();
        assert_eq!(
            c.classify("?? what happened"),
            InputDecision::Intercept {
                input: "?? what happened".to_string(),
                reason: InterceptReason::AgentMarker,
            }
        );
    }

    #[test]
    fn routing_classifier_intercepts_slash_commands() {
        let c = InputClassifier::default();
        assert_eq!(
            c.classify("/explain last error"),
            InputDecision::Intercept {
                input: "/explain last error".to_string(),
                reason: InterceptReason::Slash,
            }
        );
        assert_eq!(
            c.classify("/help"),
            InputDecision::Intercept {
                input: "/help".to_string(),
                reason: InterceptReason::Slash,
            }
        );
    }

    #[test]
    fn routing_classifier_sends_natural_language_to_shell() {
        let c = InputClassifier::default();
        for input in [
            "\u{5e2e}\u{6211}\u{5206}\u{6790}",
            "why is the build failing",
            "how do I reset my password",
            "what is a mutex",
            "explain the error",
        ] {
            assert_eq!(
                c.classify(input),
                InputDecision::SendToShell(input.to_string())
            );
        }
    }

    #[test]
    fn routing_classifier_sends_unknown_commands_to_shell() {
        let c = InputClassifier::default();
        assert_eq!(
            c.classify("git status"),
            InputDecision::SendToShell("git status".to_string())
        );
        assert_eq!(
            c.classify("ls -la"),
            InputDecision::SendToShell("ls -la".to_string())
        );
        assert_eq!(
            c.classify("cargo build"),
            InputDecision::SendToShell("cargo build".to_string())
        );
        assert_eq!(
            c.classify("mycustomtool run"),
            InputDecision::SendToShell("mycustomtool run".to_string())
        );
    }

    #[test]
    fn routing_classifier_rejects_nl_with_flags() {
        let c = InputClassifier::default();
        assert_eq!(
            c.classify("why -v"),
            InputDecision::SendToShell("why -v".to_string())
        );
        assert_eq!(
            c.classify("fix --dry-run"),
            InputDecision::SendToShell("fix --dry-run".to_string())
        );
    }

    #[test]
    fn routing_classifier_rejects_nl_with_paths() {
        let c = InputClassifier::default();
        assert_eq!(
            c.classify("explain /etc/passwd"),
            InputDecision::SendToShell("explain /etc/passwd".to_string())
        );
        assert_eq!(
            c.classify("fix src/main.rs"),
            InputDecision::SendToShell("fix src/main.rs".to_string())
        );
        assert_eq!(
            c.classify("what ~/docs"),
            InputDecision::SendToShell("what ~/docs".to_string())
        );
    }

    #[test]
    fn routing_classifier_rejects_nl_with_shell_metacharacters() {
        let c = InputClassifier::default();
        assert_eq!(
            c.classify("why echo | grep foo"),
            InputDecision::SendToShell("why echo | grep foo".to_string())
        );
        assert_eq!(
            c.classify("how to cat > file"),
            InputDecision::SendToShell("how to cat > file".to_string())
        );
        assert_eq!(
            c.classify("fix $HOME"),
            InputDecision::SendToShell("fix $HOME".to_string())
        );
    }

    #[test]
    fn routing_classifier_rejects_non_ascii_with_command_tokens() {
        let c = InputClassifier::default();
        assert_eq!(
            c.classify("cat \u{8bbe}\u{8ba1}\u{6587}\u{6863}.md"),
            InputDecision::SendToShell("cat \u{8bbe}\u{8ba1}\u{6587}\u{6863}.md".to_string())
        );
        assert_eq!(
            c.classify("\u{67e5}\u{770b} --help"),
            InputDecision::SendToShell("\u{67e5}\u{770b} --help".to_string())
        );
    }

    #[test]
    fn default_mode_sends_all_ordinary_inputs_to_shell() {
        let d = InputClassifier::default();
        assert_eq!(
            d.classify("git status"),
            InputDecision::SendToShell("git status".to_string())
        );
        assert_eq!(
            d.classify("\u{5e2e}\u{6211}\u{5206}\u{6790}"),
            InputDecision::SendToShell("\u{5e2e}\u{6211}\u{5206}\u{6790}".to_string())
        );

        let state_file = std::env::temp_dir().join(format!(
            "cosh-shell-only-classifier-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let control = AssistanceControl::enabled(state_file);
        control.set_enabled(false).expect("disable assistance");
        let shell_only = InputClassifier::default().with_assistance_control(control);
        assert_eq!(
            shell_only.classify("/mode routing assisted"),
            InputDecision::Intercept {
                input: "/mode routing assisted".to_string(),
                reason: InterceptReason::Slash,
            }
        );
        for input in ["/help", "/agent", "?? explain this", "hello"] {
            assert_eq!(
                shell_only.classify(input),
                InputDecision::SendToShell(input.to_string()),
                "{input}"
            );
        }
    }

    #[test]
    fn ai_disabled_consumes_agent_inputs_but_keeps_shell_and_slash() {
        let d = InputClassifier::default().with_ai_enabled(false);
        assert_eq!(d.classify("?? last command"), InputDecision::Consume);
        assert_eq!(
            d.classify("why is this failing"),
            InputDecision::SendToShell("why is this failing".to_string())
        );
        assert_eq!(
            d.classify("\u{5e2e}\u{6211}\u{5206}\u{6790}"),
            InputDecision::SendToShell("\u{5e2e}\u{6211}\u{5206}\u{6790}".to_string())
        );
        assert_eq!(
            d.classify("/help"),
            InputDecision::Intercept {
                input: "/help".to_string(),
                reason: InterceptReason::Slash,
            }
        );
        assert_eq!(
            d.classify("echo ok"),
            InputDecision::SendToShell("echo ok".to_string())
        );
    }
}

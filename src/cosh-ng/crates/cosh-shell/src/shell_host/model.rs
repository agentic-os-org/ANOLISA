use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nix::libc;
use nix::pty::Winsize;

use crate::input::InputClassifier;
use crate::types::{ShellEnvironmentSnapshot, ShellEvent};

use super::raw_relay::interactive_sentinel::InputWaitStatus;
use super::transcript::TranscriptRetention;

pub(super) const INTERACTIVE_TRANSCRIPT_WINDOW_BYTES: usize = 256 * 1024;
pub(super) const INTERACTIVE_EVENT_WINDOW_EVENTS: usize = 1024;

/// Selects whether Cosh participates in shell command routing.
///
/// Native sessions leave command ownership entirely with the child shell.
/// Enhanced sessions use the marker hooks needed for implicit Agent routing
/// and command-boundary events, and remain the default for compatibility.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ShellIntegration {
    /// Leaves all input, startup files, options, and traps under Shell ownership.
    Native,
    /// Enables the marker hooks required for implicit Agent routing and command events.
    #[default]
    Enhanced,
}

impl ShellIntegration {
    pub(crate) fn parse_config(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "native" => Some(Self::Native),
            "enhanced" => Some(Self::Enhanced),
            _ => None,
        }
    }

    pub(crate) fn uses_markers(self) -> bool {
        matches!(self, Self::Enhanced)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ShellEventView<'a> {
    base: usize,
    events: &'a [ShellEvent],
}

impl<'a> ShellEventView<'a> {
    pub(crate) fn new(base: usize, events: &'a [ShellEvent]) -> Self {
        Self { base, events }
    }

    pub(crate) fn base(self) -> usize {
        self.base
    }

    pub(crate) fn events(self) -> &'a [ShellEvent] {
        self.events
    }

    pub(crate) fn position(self) -> usize {
        self.base.saturating_add(self.events.len())
    }
}

#[derive(Clone)]
pub(super) struct ShellEnvironmentObserver(
    Arc<dyn Fn(ShellEnvironmentSnapshot) + Send + Sync + 'static>,
);

impl std::fmt::Debug for ShellEnvironmentObserver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ShellEnvironmentObserver")
    }
}

#[derive(Clone)]
pub(super) struct ShellHistoryFileObserver(Arc<dyn Fn(PathBuf) + Send + Sync + 'static>);

impl std::fmt::Debug for ShellHistoryFileObserver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ShellHistoryFileObserver")
    }
}

/// #2179: renders the input-wait hint card body into panel-family framed
/// lines. Injected by the runtime bootstrap (which owns the UI renderer)
/// so the relay-side sentinel emits the exact NoticePanel framing — width
/// contract, closed borders, plain fallback — without the shell host
/// depending on the UI layer.
type HintCardRenderFn = dyn Fn(&str, Vec<String>) -> Vec<String> + Send + Sync + 'static;

#[derive(Clone)]
pub(crate) struct HintCardRenderer(Arc<HintCardRenderFn>);

impl std::fmt::Debug for HintCardRenderer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HintCardRenderer")
    }
}

impl HintCardRenderer {
    pub(crate) fn new<F>(render: F) -> Self
    where
        F: Fn(&str, Vec<String>) -> Vec<String> + Send + Sync + 'static,
    {
        Self(Arc::new(render))
    }

    pub(crate) fn render(&self, title: &str, body: Vec<String>) -> Vec<String> {
        (self.0)(title, body)
    }
}

impl ShellHistoryFileObserver {
    pub(super) fn new<F>(observer: F) -> Self
    where
        F: Fn(PathBuf) + Send + Sync + 'static,
    {
        Self(Arc::new(observer))
    }

    pub(super) fn observe(&self, path: PathBuf) {
        (self.0)(path);
    }
}

impl ShellEnvironmentObserver {
    pub(super) fn new<F>(observer: F) -> Self
    where
        F: Fn(ShellEnvironmentSnapshot) + Send + Sync + 'static,
    {
        Self(Arc::new(observer))
    }

    pub(super) fn observe(&self, snapshot: ShellEnvironmentSnapshot) {
        (self.0)(snapshot);
    }
}

#[derive(Debug, Clone)]
pub struct ShellHostConfig {
    pub session_id: String,
    pub work_dir: PathBuf,
    pub bash_path: String,
    pub zsh_path: String,
    pub prompt: String,
    pub winsize: Winsize,
    pub input_classifier: InputClassifier,
    /// Chooses transparent Shell ownership or marker integration.
    pub integration: ShellIntegration,
    /// Emits the `◇ `/`◌ ` ownership status line above Enhanced prompts.
    /// Off by default; only meaningful when `integration` uses markers.
    pub status_symbols: bool,
    /// Controls whether user startup files are loaded. This remains
    /// orthogonal to `integration`: isolated sessions can run with or without
    /// Cosh marker hooks.
    pub native_mode: bool,
    pub login_shell: bool,
    /// #R2: when a login + Enhanced + non-isolated session runs, launch the
    /// inner Bash with a real login identity (`argv0="-bash" --posix` + `$ENV`
    /// inject) instead of the interactive-but-non-login `--rcfile <marker>`.
    /// The `shell.login_identity` config default is now `on` (flip-on gate
    /// passed). This struct default stays `false`: the runtime always sets it
    /// from `CoshConfig` (`controller::bootstrap`), so the conservative struct
    /// default only affects direct construction such as unit tests.
    pub login_identity: bool,
    /// #R2: whether the inner Bash can safely use the posix `$ENV` marker path.
    /// Bash < 4 does not source it, while posix startup drops exported functions
    /// whose names are not shell identifiers; either condition falls back to
    /// `--rcfile`. Defaults `true`; the runtime overwrites it after probing.
    pub bash_login_env_posix: bool,
    /// Routes exact slash-control submissions through Bash Readline so they
    /// enter native history without reaching shell parsing (issue #2912).
    /// Defaults from `COSH_SLASH_VIA_SHELL` (on unless "0"); disabling keeps
    /// the Rust intercept path. Zsh never enables this Bash-only route.
    pub slash_via_shell: bool,
    pub env_overrides: Vec<(String, String)>,
    pub raw_action_watchdog: Duration,
    /// #2161: shared input-wait episode clock. The relay's interactive
    /// sentinel marks/clears it; the runtime controller reads it to drive
    /// the `shell.input_wait_timeout_secs` interrupt. Clone the handle
    /// before handing the config to the runner.
    pub(crate) input_wait_status: InputWaitStatus,
    /// #2025/#2161: language for the relay-rendered input-wait hint card.
    pub(crate) hint_language: crate::config::Language,
    /// #2179: panel-family renderer for the hint card; when absent the
    /// sentinel stays fail-quiet and emits no card.
    pub(crate) hint_card_renderer: Option<HintCardRenderer>,
    /// #2161: mirrors `shell.input_wait_timeout_secs` so the hint card can
    /// forecast the auto-interrupt (0 = disabled, no forecast line).
    pub(crate) input_wait_timeout_secs: u64,
    pub(crate) assistance_control: Option<crate::input::AssistanceControl>,
    pub(super) shell_environment_observer: Option<ShellEnvironmentObserver>,
    pub(super) shell_history_file_observer: Option<ShellHistoryFileObserver>,
    pub(super) transcript_retention: TranscriptRetention,
}

impl ShellHostConfig {
    pub fn new(session_id: impl Into<String>, work_dir: impl Into<PathBuf>) -> Self {
        let winsize = current_terminal_winsize().unwrap_or_else(default_winsize);
        Self {
            session_id: session_id.into(),
            work_dir: work_dir.into(),
            bash_path: "bash".to_string(),
            zsh_path: "zsh".to_string(),
            prompt: "cosh-osc$ ".to_string(),
            winsize,
            input_classifier: InputClassifier::default(),
            integration: ShellIntegration::Enhanced,
            status_symbols: false,
            native_mode: true,
            login_shell: false,
            login_identity: false,
            bash_login_env_posix: true,
            slash_via_shell: slash_via_shell_default(),
            env_overrides: Vec::new(),
            raw_action_watchdog: Duration::from_secs(120),
            input_wait_status: InputWaitStatus::default(),
            hint_language: crate::config::Language::default(),
            hint_card_renderer: None,
            input_wait_timeout_secs: 120,
            assistance_control: None,
            shell_environment_observer: None,
            shell_history_file_observer: None,
            transcript_retention: TranscriptRetention::Full,
        }
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env_overrides.push((key.into(), value.into()));
        self
    }

    pub fn with_status_symbols(mut self, enabled: bool) -> Self {
        self.status_symbols = enabled;
        self
    }

    pub fn with_ai_enabled(mut self, enabled: bool) -> Self {
        self.input_classifier = self.input_classifier.with_ai_enabled(enabled);
        self
    }

    /// Selects the Shell integration policy for this host session.
    pub fn with_integration(mut self, integration: ShellIntegration) -> Self {
        self.integration = integration;
        self
    }

    pub(crate) fn set_assistance_control(&mut self, control: crate::input::AssistanceControl) {
        self.assistance_control = Some(control);
    }

    /// Installs the input-wait hint card frame renderer (#2196 review):
    /// `new()` leaves it unset and the sentinel then stays fail-quiet, so
    /// crate-external callers of the public raw relay entry points need
    /// this to opt back into the card. The closure receives the card
    /// title and body lines and returns the framed lines to emit; the
    /// in-process runtime injects the NoticePanel framing here.
    pub fn set_hint_card_renderer<F>(&mut self, render: F)
    where
        F: Fn(&str, Vec<String>) -> Vec<String> + Send + Sync + 'static,
    {
        self.hint_card_renderer = Some(HintCardRenderer::new(render));
    }

    pub(crate) fn set_shell_environment_observer<F>(&mut self, observer: F)
    where
        F: Fn(ShellEnvironmentSnapshot) + Send + Sync + 'static,
    {
        self.shell_environment_observer = Some(ShellEnvironmentObserver::new(observer));
    }

    pub(crate) fn clear_shell_environment_observer(&mut self) {
        self.shell_environment_observer = None;
    }

    pub(crate) fn set_shell_history_file_observer<F>(&mut self, observer: F)
    where
        F: Fn(PathBuf) + Send + Sync + 'static,
    {
        self.shell_history_file_observer = Some(ShellHistoryFileObserver::new(observer));
    }

    pub(crate) fn clear_shell_history_file_observer(&mut self) {
        self.shell_history_file_observer = None;
    }

    /// Bounds byte-transcript memory for the real interactive runtime. Public
    /// and scripted callers keep full retention unless they enter this path.
    pub(crate) fn bound_interactive_transcript(&mut self) {
        self.transcript_retention = TranscriptRetention::Bounded {
            window_bytes: INTERACTIVE_TRANSCRIPT_WINDOW_BYTES,
        };
    }
}

/// `COSH_SLASH_VIA_SHELL` gates native Bash history for exact slash
/// submissions; any value other than "0" (including unset) keeps it on.
fn slash_via_shell_default() -> bool {
    std::env::var("COSH_SLASH_VIA_SHELL")
        .map(|value| value != "0")
        .unwrap_or(true)
}

fn default_winsize() -> Winsize {
    Winsize {
        ws_row: 40,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

pub(super) fn current_terminal_winsize() -> Option<Winsize> {
    [libc::STDOUT_FILENO, libc::STDIN_FILENO, libc::STDERR_FILENO]
        .into_iter()
        .filter_map(read_fd_winsize)
        .find(|winsize| winsize.ws_row > 0 && winsize.ws_col > 0)
}

fn read_fd_winsize(fd: i32) -> Option<Winsize> {
    let mut winsize = default_winsize();
    let result = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ as libc::c_ulong, &mut winsize) };
    if result == 0 {
        Some(winsize)
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptedInput {
    Command(String),
    UserLine(String),
    Intercept { input: String, reason: String },
}

impl ScriptedInput {
    pub fn command(command: impl Into<String>) -> Self {
        Self::Command(command.into())
    }

    pub fn user_line(input: impl Into<String>) -> Self {
        Self::UserLine(input.into())
    }

    pub fn intercept(input: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Intercept {
            input: input.into(),
            reason: reason.into(),
        }
    }
}

#[derive(Debug)]
pub struct ShellHostOutput {
    pub events: Vec<ShellEvent>,
    pub terminal_output: Vec<u8>,
    pub work_dir: PathBuf,
    pub journal_path: PathBuf,
    pub exit_status: Option<i32>,
}

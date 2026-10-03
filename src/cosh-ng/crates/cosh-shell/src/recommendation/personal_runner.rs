use std::path::PathBuf;
use std::time::{Duration, Instant};

pub(crate) const MAX_ANALYZER_OUTPUT_BYTES: usize = 16 * 1024;
pub(crate) const ANALYZER_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunnerCommand {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) cwd: PathBuf,
}

pub(crate) fn analyzer_command(empty_cwd: PathBuf, model: Option<&str>) -> RunnerCommand {
    let mut args = ["--headless", "--bare", "--tools", ""]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if let Some(model) = model.filter(|model| !model.trim().is_empty()) {
        args.extend(["--model".to_string(), model.to_string()]);
    }
    RunnerCommand {
        program: "cosh-core".to_string(),
        args,
        env: vec![("COSH_MAX_TURNS".to_string(), "1".to_string())],
        cwd: empty_cwd,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InitializeResult {
    Ready { model: String, tools: Vec<String> },
    AuthRequired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunnerEvent {
    AssistantDelta(String),
    Assistant(String),
    Result { success: bool },
    ToolCall,
    ApprovalRequest,
    Question,
    AuthRequired,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessFailure {
    Timeout,
    TimeoutAfterWrite,
    Transport,
    TransportAfterWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunnerError {
    AuthRequired { body_sent: bool },
    ToolsEnabled { body_sent: bool },
    BodyTransitionFailed { body_sent: bool },
    Timeout { body_sent: bool },
    Transport { body_sent: bool },
    InteractiveEvent { body_sent: bool },
    OutputTooLarge { body_sent: bool },
    MultipleAssistantMessages { body_sent: bool },
    MultipleResults { body_sent: bool },
    DeltaFullMismatch { body_sent: bool },
    MissingAssistant { body_sent: bool },
    MissingResult { body_sent: bool },
    FailedResult { body_sent: bool },
    InvalidSequence { body_sent: bool },
}

impl RunnerError {
    pub(crate) fn body_sent(self) -> bool {
        match self {
            Self::AuthRequired { body_sent }
            | Self::ToolsEnabled { body_sent }
            | Self::BodyTransitionFailed { body_sent }
            | Self::Timeout { body_sent }
            | Self::Transport { body_sent }
            | Self::InteractiveEvent { body_sent }
            | Self::OutputTooLarge { body_sent }
            | Self::MultipleAssistantMessages { body_sent }
            | Self::MultipleResults { body_sent }
            | Self::DeltaFullMismatch { body_sent }
            | Self::MissingAssistant { body_sent }
            | Self::MissingResult { body_sent }
            | Self::FailedResult { body_sent }
            | Self::InvalidSequence { body_sent } => body_sent,
        }
    }
}

pub(crate) trait AnalyzerProcess {
    fn initialize(&mut self, timeout: Duration) -> Result<InitializeResult, ProcessFailure>;
    fn send_body(&mut self, body: &str, timeout: Duration) -> Result<(), ProcessFailure>;
    fn next_event(&mut self, timeout: Duration) -> Result<RunnerEvent, ProcessFailure>;
    fn cancel(&mut self);
}

#[cfg(test)]
pub(crate) fn run_initialized(
    process: &mut impl AnalyzerProcess,
    body: &str,
) -> Result<String, RunnerError> {
    run_initialized_with_body_hook(process, body, || Ok(()))
}

#[cfg(test)]
pub(crate) fn run_initialized_with_body_hook(
    process: &mut impl AnalyzerProcess,
    body: &str,
    before_body: impl FnOnce() -> Result<(), ()>,
) -> Result<String, RunnerError> {
    run_initialized_with_body_hooks(process, body, before_body, || Ok(()))
}

pub(crate) fn run_initialized_with_body_hooks(
    process: &mut impl AnalyzerProcess,
    body: &str,
    before_body: impl FnOnce() -> Result<(), ()>,
    after_body: impl FnOnce() -> Result<(), ()>,
) -> Result<String, RunnerError> {
    let started = Instant::now();
    let initialized = process.initialize(ANALYZER_TIMEOUT).map_err(|failure| {
        process.cancel();
        map_process_failure(failure, false)
    })?;
    match initialized {
        InitializeResult::AuthRequired => {
            process.cancel();
            return Err(RunnerError::AuthRequired { body_sent: false });
        }
        InitializeResult::Ready { tools, .. } if !tools.is_empty() => {
            process.cancel();
            return Err(RunnerError::ToolsEnabled { body_sent: false });
        }
        InitializeResult::Ready { .. } => {}
    }
    if before_body().is_err() {
        process.cancel();
        return Err(RunnerError::BodyTransitionFailed { body_sent: false });
    }
    let Some(timeout) = remaining(started) else {
        process.cancel();
        return Err(RunnerError::Timeout { body_sent: false });
    };
    if let Err(failure) = process.send_body(body, timeout) {
        process.cancel();
        return Err(map_process_failure(failure, false));
    }
    if after_body().is_err() {
        process.cancel();
        return Err(RunnerError::BodyTransitionFailed { body_sent: true });
    }

    let mut deltas = String::new();
    let mut assistant = None;
    let mut result = None;
    loop {
        let Some(timeout) = remaining(started) else {
            process.cancel();
            return Err(RunnerError::Timeout { body_sent: true });
        };
        let event = match process.next_event(timeout) {
            Ok(event) => event,
            Err(failure) => {
                process.cancel();
                return Err(map_process_failure(failure, true));
            }
        };
        let error = match event {
            RunnerEvent::AssistantDelta(delta) => {
                if assistant.is_some() || result.is_some() {
                    Some(RunnerError::InvalidSequence { body_sent: true })
                } else if deltas.len().saturating_add(delta.len()) > MAX_ANALYZER_OUTPUT_BYTES {
                    Some(RunnerError::OutputTooLarge { body_sent: true })
                } else {
                    deltas.push_str(&delta);
                    None
                }
            }
            RunnerEvent::Assistant(full) => {
                if assistant.is_some() {
                    Some(RunnerError::MultipleAssistantMessages { body_sent: true })
                } else if result.is_some() {
                    Some(RunnerError::InvalidSequence { body_sent: true })
                } else if full.len() > MAX_ANALYZER_OUTPUT_BYTES {
                    Some(RunnerError::OutputTooLarge { body_sent: true })
                } else if !deltas.is_empty() && deltas != full {
                    Some(RunnerError::DeltaFullMismatch { body_sent: true })
                } else {
                    assistant = Some(full);
                    None
                }
            }
            RunnerEvent::Result { success } => {
                if result.is_some() {
                    Some(RunnerError::MultipleResults { body_sent: true })
                } else if assistant.is_none() {
                    Some(RunnerError::InvalidSequence { body_sent: true })
                } else if !success {
                    Some(RunnerError::FailedResult { body_sent: true })
                } else {
                    result = Some(());
                    None
                }
            }
            RunnerEvent::ToolCall | RunnerEvent::ApprovalRequest | RunnerEvent::Question => {
                Some(RunnerError::InteractiveEvent { body_sent: true })
            }
            RunnerEvent::AuthRequired => Some(RunnerError::AuthRequired { body_sent: true }),
            RunnerEvent::End => {
                let Some(output) = assistant else {
                    process.cancel();
                    return Err(RunnerError::MissingAssistant { body_sent: true });
                };
                if result.is_none() {
                    process.cancel();
                    return Err(RunnerError::MissingResult { body_sent: true });
                }
                return Ok(output);
            }
        };
        if let Some(error) = error {
            process.cancel();
            return Err(error);
        }
    }
}

fn remaining(started: Instant) -> Option<Duration> {
    ANALYZER_TIMEOUT
        .checked_sub(started.elapsed())
        .filter(|remaining| !remaining.is_zero())
}

fn map_process_failure(failure: ProcessFailure, body_sent: bool) -> RunnerError {
    match failure {
        ProcessFailure::Timeout => RunnerError::Timeout { body_sent },
        ProcessFailure::TimeoutAfterWrite => RunnerError::Timeout { body_sent: true },
        ProcessFailure::Transport => RunnerError::Transport { body_sent },
        ProcessFailure::TransportAfterWrite => RunnerError::Transport { body_sent: true },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;

    #[test]
    fn command_uses_foreground_model_and_runs_from_an_empty_directory() {
        let command = analyzer_command(PathBuf::from("/tmp/empty"), Some("project-model"));

        assert_eq!(command.program, "cosh-core");
        assert_eq!(
            command.args,
            [
                "--headless",
                "--bare",
                "--tools",
                "",
                "--model",
                "project-model"
            ]
        );
        assert_eq!(
            command.env,
            [("COSH_MAX_TURNS".to_string(), "1".to_string())]
        );
        assert_eq!(command.cwd, PathBuf::from("/tmp/empty"));
    }

    #[test]
    fn initializes_with_empty_tools_before_writing_body() {
        let mut process = FakeProcess::new(
            InitializeResult::Ready {
                model: "main-model".to_string(),
                tools: vec!["shell".to_string()],
            },
            [],
        );

        let error = run_initialized(&mut process, "body").expect_err("tools must be empty");

        assert_eq!(error, RunnerError::ToolsEnabled { body_sent: false });
        assert_eq!(process.body_writes, 0);
        assert_eq!(process.actions, ["initialize", "cancel"]);
    }

    #[test]
    fn uses_the_model_selected_by_core() {
        let mut process = FakeProcess::new(
            InitializeResult::Ready {
                model: "other-model".to_string(),
                tools: vec![],
            },
            [],
        );

        process.events = VecDeque::from([
            RunnerEvent::Assistant("{}".to_string()),
            RunnerEvent::Result { success: true },
            RunnerEvent::End,
        ]);

        assert_eq!(run_initialized(&mut process, "body"), Ok("{}".to_string()));
        assert_eq!(process.body_writes, 1);
    }

    #[test]
    fn body_transition_hook_runs_after_preflight_and_can_stop_the_write() {
        let mut process = FakeProcess::ready([]);
        let mut hook_called = false;

        let result = run_initialized_with_body_hook(&mut process, "body", || {
            hook_called = true;
            Err(())
        });

        assert!(hook_called);
        assert_eq!(
            result,
            Err(RunnerError::BodyTransitionFailed { body_sent: false })
        );
        assert_eq!(process.body_writes, 0);
        assert_eq!(process.actions, ["initialize", "cancel"]);
    }

    #[test]
    fn accepts_one_matching_delta_full_assistant_and_result() {
        let mut process = FakeProcess::new(
            InitializeResult::Ready {
                model: "main-model".to_string(),
                tools: vec![],
            },
            [
                RunnerEvent::AssistantDelta("{\"ok\":".to_string()),
                RunnerEvent::AssistantDelta("true}".to_string()),
                RunnerEvent::Assistant("{\"ok\":true}".to_string()),
                RunnerEvent::Result { success: true },
                RunnerEvent::End,
            ],
        );

        let output = run_initialized(&mut process, "body").expect("valid response");

        assert_eq!(output, "{\"ok\":true}");
        assert_eq!(process.body_writes, 1);
        assert!(process
            .timeouts
            .iter()
            .all(|timeout| *timeout <= Duration::from_secs(20)));
    }

    #[test]
    fn body_write_failure_tracks_whether_any_bytes_were_sent() {
        let mut zero = FakeProcess::ready([]);
        zero.body_failure = Some(ProcessFailure::Transport);
        assert_eq!(
            run_initialized(&mut zero, "body"),
            Err(RunnerError::Transport { body_sent: false })
        );

        let mut partial = FakeProcess::ready([]);
        partial.body_failure = Some(ProcessFailure::TransportAfterWrite);
        assert_eq!(
            run_initialized(&mut partial, "body"),
            Err(RunnerError::Transport { body_sent: true })
        );
    }

    #[test]
    fn rejects_mismatched_or_multiple_assistant_output_without_leaking_it() {
        let mut mismatch = FakeProcess::ready([
            RunnerEvent::AssistantDelta("secret-a".to_string()),
            RunnerEvent::Assistant("secret-b".to_string()),
            RunnerEvent::Result { success: true },
            RunnerEvent::End,
        ]);
        assert_eq!(
            run_initialized(&mut mismatch, "body"),
            Err(RunnerError::DeltaFullMismatch { body_sent: true })
        );
        assert_eq!(mismatch.actions.last(), Some(&"cancel"));

        let mut multiple = FakeProcess::ready([
            RunnerEvent::Assistant("one".to_string()),
            RunnerEvent::Assistant("two".to_string()),
        ]);
        assert_eq!(
            run_initialized(&mut multiple, "body"),
            Err(RunnerError::MultipleAssistantMessages { body_sent: true })
        );
    }

    #[test]
    fn rejects_oversized_output_and_post_body_interaction() {
        let mut oversized = FakeProcess::ready([RunnerEvent::Assistant(
            "x".repeat(MAX_ANALYZER_OUTPUT_BYTES + 1),
        )]);
        assert_eq!(
            run_initialized(&mut oversized, "body"),
            Err(RunnerError::OutputTooLarge { body_sent: true })
        );

        for event in [
            RunnerEvent::ToolCall,
            RunnerEvent::ApprovalRequest,
            RunnerEvent::Question,
        ] {
            let mut process = FakeProcess::ready([event]);
            assert_eq!(
                run_initialized(&mut process, "body"),
                Err(RunnerError::InteractiveEvent { body_sent: true })
            );
            assert_eq!(process.actions.last(), Some(&"cancel"));
        }
        let mut auth = FakeProcess::ready([RunnerEvent::AuthRequired]);
        assert_eq!(
            run_initialized(&mut auth, "body"),
            Err(RunnerError::AuthRequired { body_sent: true })
        );
        assert_eq!(auth.actions.last(), Some(&"cancel"));
    }

    #[test]
    fn auth_preflight_never_sends_body_and_timeout_cancels() {
        let mut auth = FakeProcess::new(InitializeResult::AuthRequired, []);
        assert_eq!(
            run_initialized(&mut auth, "body"),
            Err(RunnerError::AuthRequired { body_sent: false })
        );
        assert_eq!(auth.body_writes, 0);

        let mut timeout = FakeProcess::ready([]);
        timeout.timeout = true;
        assert_eq!(
            run_initialized(&mut timeout, "body"),
            Err(RunnerError::Timeout { body_sent: true })
        );
        assert_eq!(timeout.timeouts.len(), 3);
        assert!(timeout
            .timeouts
            .iter()
            .all(|value| *value <= Duration::from_secs(20)));
        assert_eq!(timeout.actions.last(), Some(&"cancel"));
    }

    struct FakeProcess {
        initialize_result: InitializeResult,
        events: VecDeque<RunnerEvent>,
        actions: Vec<&'static str>,
        body_writes: usize,
        timeouts: Vec<Duration>,
        timeout: bool,
        body_failure: Option<ProcessFailure>,
    }

    impl FakeProcess {
        fn new(
            initialize_result: InitializeResult,
            events: impl IntoIterator<Item = RunnerEvent>,
        ) -> Self {
            Self {
                initialize_result,
                events: events.into_iter().collect(),
                actions: Vec::new(),
                body_writes: 0,
                timeouts: Vec::new(),
                timeout: false,
                body_failure: None,
            }
        }

        fn ready(events: impl IntoIterator<Item = RunnerEvent>) -> Self {
            Self::new(
                InitializeResult::Ready {
                    model: "main-model".to_string(),
                    tools: vec![],
                },
                events,
            )
        }
    }

    impl AnalyzerProcess for FakeProcess {
        fn initialize(&mut self, timeout: Duration) -> Result<InitializeResult, ProcessFailure> {
            self.actions.push("initialize");
            self.timeouts.push(timeout);
            Ok(self.initialize_result.clone())
        }

        fn send_body(&mut self, _body: &str, timeout: Duration) -> Result<(), ProcessFailure> {
            self.actions.push("body");
            self.timeouts.push(timeout);
            self.body_writes += 1;
            self.body_failure.take().map_or(Ok(()), Err)
        }

        fn next_event(&mut self, timeout: Duration) -> Result<RunnerEvent, ProcessFailure> {
            self.timeouts.push(timeout);
            if self.timeout {
                return Err(ProcessFailure::Timeout);
            }
            Ok(self.events.pop_front().unwrap_or(RunnerEvent::End))
        }

        fn cancel(&mut self) {
            self.actions.push("cancel");
        }
    }
}

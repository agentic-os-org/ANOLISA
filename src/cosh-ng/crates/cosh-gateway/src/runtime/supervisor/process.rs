/// Observable supervisor lifecycle for one process generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeState {
    /// No process has been launched.
    Idle,
    /// Launch validation passed and spawn is in progress.
    Starting,
    /// Child pipes are owned and protocol initialization may begin.
    Initializing,
    /// The protocol bridge marked initialization successful.
    Ready,
    /// Graceful shutdown or kill escalation is in progress.
    Stopping,
    /// The child was reaped and its sole process terminal was materialized.
    Exited,
}

/// Outcome of a deadline-bounded stdout poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeFrameRead {
    /// One complete protocol frame was received.
    Frame(String),
    /// The Agent closed stdout.
    Eof,
    /// No frame arrived within the requested duration.
    TimedOut,
}

/// Stable process-level exit classification; this is not a Task terminal event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessExit {
    /// The runtime returned an ordinary platform exit code.
    Code(i32),
    /// The runtime was terminated by a signal on Unix.
    Signal(i32),
    /// The platform did not expose an exit code or signal.
    Unknown,
}

/// The sole process terminal produced after the supervised child is reaped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessTerminal {
    /// Reaped platform exit classification.
    pub exit: ProcessExit,
    /// Bounded stderr diagnostics collected before settlement.
    pub stderr: StderrSnapshot,
}

/// Runtime supervisor failure.
#[derive(Debug, Error)]
pub enum RuntimeSupervisorError {
    /// Launch validation failed before the state changed.
    #[error(transparent)]
    Launch(#[from] RuntimeLaunchError),
    /// The requested operation is invalid in the current lifecycle state.
    #[error("runtime operation {operation} is invalid while state is {state:?}")]
    InvalidState {
        /// Requested lifecycle operation.
        operation: &'static str,
        /// Current lifecycle state.
        state: RuntimeState,
    },
    /// Creating or controlling the child process failed.
    #[error("runtime process operation failed: {0}")]
    Process(#[from] io::Error),
    /// Runtime stdout violated its framing bound.
    #[error(transparent)]
    Stdout(#[from] BoundedLineError),
    /// Process-group signalling failed after the direct child was cleaned up.
    #[error("failed to send {signal} to runtime process group; direct child was killed and reaped: {source}")]
    ProcessGroupSignal {
        /// Signal operation that failed.
        signal: &'static str,
        /// Underlying platform failure.
        #[source]
        source: io::Error,
    },
}

/// Sole owner of one runtime child, its pipes, process group, and reap result.
#[derive(Debug)]
pub struct RuntimeSupervisor {
    state: RuntimeState,
    child: Option<Child>,
    stdin: Option<BoundedWriteChannel>,
    stdout: Option<BoundedLineChannel>,
    stderr: Option<StderrCollector>,
    process_group: Arc<dyn ProcessGroupLifecycle>,
    process_group_id: Option<u32>,
    terminal: Option<ProcessTerminal>,
    terminal_delivered: bool,
    stdin_write_timeout: Duration,
}

impl Default for RuntimeSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeSupervisor {
    /// Builds an idle supervisor using the native process-group implementation.
    pub fn new() -> Self {
        Self::with_process_group(Arc::new(PlatformProcessGroup))
    }

    /// Builds an idle supervisor with an injected lifecycle implementation.
    pub fn with_process_group(process_group: Arc<dyn ProcessGroupLifecycle>) -> Self {
        Self {
            state: RuntimeState::Idle,
            child: None,
            stdin: None,
            stdout: None,
            stderr: None,
            process_group,
            process_group_id: None,
            terminal: None,
            terminal_delivered: false,
            stdin_write_timeout: Duration::from_secs(5),
        }
    }

    /// Returns the current process lifecycle state.
    pub fn state(&self) -> RuntimeState {
        self.state
    }

    /// Validates and starts one direct child in a dedicated process group.
    ///
    /// # Errors
    ///
    /// Returns launch validation, pipe setup, thread creation, or spawn errors.
    /// A failed launch owns no child and returns to `Idle`.
    pub fn launch(&mut self, spec: &RuntimeLaunchSpec) -> Result<(), RuntimeSupervisorError> {
        if self.state != RuntimeState::Idle {
            return Err(RuntimeSupervisorError::InvalidState {
                operation: "launch",
                state: self.state,
            });
        }
        spec.validate()?;
        self.state = RuntimeState::Starting;

        let launch_result = self.launch_validated(spec);
        if launch_result.is_err() {
            self.state = RuntimeState::Idle;
        }
        launch_result
    }

    fn launch_validated(&mut self, spec: &RuntimeLaunchSpec) -> Result<(), RuntimeSupervisorError> {
        let (program, inherited_script_descriptor) = match &spec.program {
            LaunchProgram::Pinned {
                executable,
                inherit_for_script,
            } => (
                executable.descriptor_path(),
                inherit_for_script.then(|| executable.descriptor_fd()),
            ),
            LaunchProgram::Invalid { .. } => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid Runtime launch program reached spawn",
                )
                .into());
            }
        };
        let working_directory = match &spec.working_directory {
            LaunchDirectory::Pinned(directory) => directory.descriptor_path(),
            LaunchDirectory::Invalid { .. } => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid Runtime working directory reached spawn",
                )
                .into());
            }
        };
        let mut command = Command::new(program);
        command
            .args(&spec.arguments)
            .current_dir(working_directory)
            .env_clear()
            .envs(&spec.environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.process_group.configure(&mut command);
        if let Some(descriptor) = inherited_script_descriptor {
            configure_script_descriptor_inheritance(&mut command, descriptor);
        }

        let mut child = command.spawn()?;
        let process_group_id = child.id();
        let stdin = match child.stdin.take() {
            Some(stdin) => stdin,
            None => {
                cleanup_partial_child(&self.process_group, &mut child, process_group_id);
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "runtime stdin pipe unavailable",
                )
                .into());
            }
        };
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                cleanup_partial_child(&self.process_group, &mut child, process_group_id);
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "runtime stdout pipe unavailable",
                )
                .into());
            }
        };
        let stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                cleanup_partial_child(&self.process_group, &mut child, process_group_id);
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "runtime stderr pipe unavailable",
                )
                .into());
            }
        };

        let collector = match StderrCollector::spawn(stderr, spec.stderr_capacity) {
            Ok(collector) => collector,
            Err(error) => {
                cleanup_partial_child(&self.process_group, &mut child, process_group_id);
                return Err(error.into());
            }
        };

        let stdin = match BoundedWriteChannel::spawn(stdin) {
            Ok(channel) => channel,
            Err(error) => {
                cleanup_partial_child(&self.process_group, &mut child, process_group_id);
                let _ = collector.finish();
                return Err(error.into());
            }
        };
        let stdout = match BoundedLineChannel::spawn(stdout, spec.stdout_line_limit) {
            Ok(channel) => channel,
            Err(error) => {
                cleanup_partial_child(&self.process_group, &mut child, process_group_id);
                stdin.finish();
                let _ = collector.finish();
                return Err(error.into());
            }
        };
        self.stdin = Some(stdin);
        self.stdout = Some(stdout);
        self.stderr = Some(collector);
        self.process_group_id = Some(process_group_id);
        self.child = Some(child);
        self.terminal = None;
        self.terminal_delivered = false;
        self.stdin_write_timeout = spec.stdin_write_timeout;
        self.state = RuntimeState::Initializing;
        Ok(())
    }

    /// Marks successful protocol negotiation without changing process ownership.
    ///
    /// # Errors
    ///
    /// Returns an invalid-state error unless the child is initializing.
    pub fn mark_ready(&mut self) -> Result<(), RuntimeSupervisorError> {
        if self.state != RuntimeState::Initializing {
            return Err(RuntimeSupervisorError::InvalidState {
                operation: "mark_ready",
                state: self.state,
            });
        }
        self.state = RuntimeState::Ready;
        Ok(())
    }

    /// Writes one already-encoded protocol frame and flushes it.
    ///
    /// # Errors
    ///
    /// Returns an invalid-state error before launch/after exit or an I/O error
    /// when the child closed its input.
    pub fn write_frame(&mut self, frame: &str) -> Result<(), RuntimeSupervisorError> {
        if !matches!(self.state, RuntimeState::Initializing | RuntimeState::Ready) {
            return Err(RuntimeSupervisorError::InvalidState {
                operation: "write_frame",
                state: self.state,
            });
        }
        let stdin = self.stdin.as_ref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "runtime stdin pipe unavailable")
        })?;
        let mut bytes = frame.as_bytes().to_vec();
        if !frame.ends_with('\n') {
            bytes.push(b'\n');
        }
        stdin.write_timeout(bytes, self.stdin_write_timeout)?;
        Ok(())
    }

    /// Reads one bounded protocol line from runtime stdout.
    ///
    /// # Errors
    ///
    /// Returns invalid-state, I/O, invalid UTF-8, or oversized-frame errors.
    pub fn read_frame(&mut self) -> Result<Option<String>, RuntimeSupervisorError> {
        loop {
            match self.read_frame_timeout(Duration::from_secs(60))? {
                RuntimeFrameRead::Frame(frame) => return Ok(Some(frame)),
                RuntimeFrameRead::Eof => return Ok(None),
                RuntimeFrameRead::TimedOut => {}
            }
        }
    }

    /// Waits at most `timeout` for one bounded protocol line.
    ///
    /// # Errors
    ///
    /// Returns invalid-state, I/O, invalid UTF-8, or oversized-frame errors.
    pub fn read_frame_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<RuntimeFrameRead, RuntimeSupervisorError> {
        if !matches!(
            self.state,
            RuntimeState::Initializing | RuntimeState::Ready | RuntimeState::Stopping
        ) {
            return Err(RuntimeSupervisorError::InvalidState {
                operation: "read_frame",
                state: self.state,
            });
        }
        let outcome = self
            .stdout
            .as_mut()
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::BrokenPipe, "runtime stdout pipe unavailable")
            })?
            .read_timeout(timeout)?;
        Ok(match outcome {
            BoundedLineRead::Line(frame) => RuntimeFrameRead::Frame(frame),
            BoundedLineRead::Eof => RuntimeFrameRead::Eof,
            BoundedLineRead::TimedOut => RuntimeFrameRead::TimedOut,
        })
    }

    /// Returns a current bounded stderr snapshot without waiting for exit.
    pub fn stderr_snapshot(&self) -> Option<StderrSnapshot> {
        self.stderr.as_ref().map(StderrCollector::snapshot)
    }

    /// Polls for process exit and delivers the process terminal at most once.
    ///
    /// # Errors
    ///
    /// Returns an invalid-state error before launch or an OS wait error.
    pub fn poll_terminal(&mut self) -> Result<Option<ProcessTerminal>, RuntimeSupervisorError> {
        if self.state == RuntimeState::Idle {
            return Err(RuntimeSupervisorError::InvalidState {
                operation: "poll_terminal",
                state: self.state,
            });
        }
        if self.state == RuntimeState::Exited {
            return Ok(self.take_terminal());
        }

        let status = self
            .child
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "runtime child unavailable"))?
            .try_wait()?;
        if let Some(status) = status {
            self.settle(status);
        }
        Ok(self.take_terminal())
    }

    /// Sends TERM, waits for the grace period, escalates to KILL, and reaps.
    ///
    /// The returned process terminal is `None` only if it was already
    /// delivered by `poll_terminal`.
    ///
    /// # Errors
    ///
    /// Returns invalid-state or OS signalling/wait errors. Drop still attempts
    /// unconditional cleanup after an error.
    pub fn shutdown(
        &mut self,
        grace: Duration,
    ) -> Result<Option<ProcessTerminal>, RuntimeSupervisorError> {
        if self.state == RuntimeState::Idle {
            return Err(RuntimeSupervisorError::InvalidState {
                operation: "shutdown",
                state: self.state,
            });
        }
        if self.state == RuntimeState::Exited {
            return Ok(self.take_terminal());
        }
        self.state = RuntimeState::Stopping;
        if let Some(stdin) = self.stdin.take() {
            stdin.finish();
        }

        if let Some(process_group_id) = self.process_group_id {
            if let Err(source) = self.process_group.terminate(process_group_id) {
                let status = self.kill_direct_child_and_reap()?;
                self.settle(status);
                return Err(RuntimeSupervisorError::ProcessGroupSignal {
                    signal: "TERM",
                    source,
                });
            }
        }
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "runtime child unavailable"))?;
        let status = match child.wait_timeout(grace)? {
            Some(status) => status,
            None => {
                if let Some(process_group_id) = self.process_group_id {
                    if let Err(group_error) = self.process_group.kill(process_group_id) {
                        let status = self.kill_direct_child_and_reap()?;
                        self.settle(status);
                        return Err(RuntimeSupervisorError::ProcessGroupSignal {
                            signal: "KILL",
                            source: group_error,
                        });
                    }
                } else {
                    child.kill()?;
                }
                child.wait()?
            }
        };
        self.settle(status);
        Ok(self.take_terminal())
    }

    fn kill_direct_child_and_reap(&mut self) -> Result<ExitStatus, RuntimeSupervisorError> {
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "runtime child unavailable"))?;
        match child.kill() {
            Ok(()) => child.wait().map_err(Into::into),
            Err(kill_error) => match child.try_wait()? {
                Some(status) => Ok(status),
                None => Err(kill_error.into()),
            },
        }
    }

    fn settle(&mut self, status: ExitStatus) {
        // A runtime may leave descendants after its leader exits. The group is
        // still the supervisor's ownership boundary, so settle it before
        // publishing the only terminal observation.
        if let Some(process_group_id) = self.process_group_id {
            let _ = self.process_group.kill(process_group_id);
        }
        if let Some(stdin) = self.stdin.take() {
            stdin.finish();
        }
        if let Some(stdout) = self.stdout.take() {
            stdout.finish();
        }
        self.child.take();
        let stderr = self
            .stderr
            .take()
            .map(StderrCollector::finish)
            .unwrap_or_else(empty_stderr);
        self.terminal = Some(ProcessTerminal {
            exit: classify_exit(status),
            stderr,
        });
        self.state = RuntimeState::Exited;
    }

    fn take_terminal(&mut self) -> Option<ProcessTerminal> {
        if self.terminal_delivered {
            return None;
        }
        let terminal = self.terminal.clone()?;
        self.terminal_delivered = true;
        Some(terminal)
    }
}

impl Drop for RuntimeSupervisor {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        if let Some(process_group_id) = self.process_group_id {
            let _ = self.process_group.kill(process_group_id);
        }
        let _ = child.kill();
        let _ = child.wait();
        if let Some(stdin) = self.stdin.take() {
            stdin.finish();
        }
        if let Some(stdout) = self.stdout.take() {
            stdout.finish();
        }
        if let Some(stderr) = self.stderr.take() {
            let _ = stderr.finish();
        }
    }
}

fn empty_stderr() -> StderrSnapshot {
    StderrSnapshot {
        tail: String::new(),
        discarded_bytes: 0,
        read_error: None,
    }
}

fn classify_exit(status: ExitStatus) -> ProcessExit {
    if let Some(code) = status.code() {
        return ProcessExit::Code(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return ProcessExit::Signal(signal);
        }
    }
    ProcessExit::Unknown
}

fn cleanup_partial_child(
    process_group: &Arc<dyn ProcessGroupLifecycle>,
    child: &mut Child,
    process_group_id: u32,
) {
    let _ = process_group.kill(process_group_id);
    let _ = child.kill();
    let _ = child.wait();
}

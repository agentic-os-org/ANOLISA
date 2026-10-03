/// Configuration used to start a supervised runtime without invoking a shell.
#[derive(Debug, Clone)]
pub struct RuntimeLaunchSpec {
    program: LaunchProgram,
    /// Arguments passed directly to the executable.
    pub arguments: Vec<OsString>,
    working_directory: LaunchDirectory,
    /// Explicit child environment after the inherited environment is cleared.
    pub environment: BTreeMap<OsString, OsString>,
    /// Maximum retained stderr tail in bytes.
    pub stderr_capacity: usize,
    /// Maximum accepted stdout JSONL frame in bytes.
    pub stdout_line_limit: usize,
    /// Maximum time to enqueue and flush one stdin frame.
    pub stdin_write_timeout: Duration,
}

#[derive(Debug, Clone)]
enum LaunchProgram {
    Pinned {
        executable: PinnedExecutable,
        inherit_for_script: bool,
    },
    Invalid {
        path: PathBuf,
        relative: bool,
    },
}

#[derive(Debug, Clone)]
enum LaunchDirectory {
    Pinned(PinnedDirectory),
    Invalid { path: PathBuf, relative: bool },
}

impl RuntimeLaunchSpec {
    /// Builds a launch specification with conservative I/O bounds.
    pub fn new(program: impl Into<PathBuf>, working_directory: impl Into<PathBuf>) -> Self {
        let program = program.into();
        let working_directory = working_directory.into();
        let program = if program.is_absolute() {
            PinnedExecutable::pin(&program)
                .map(|executable| LaunchProgram::Pinned {
                    executable,
                    inherit_for_script: false,
                })
                .unwrap_or_else(|_| LaunchProgram::Invalid {
                    path: program,
                    relative: false,
                })
        } else {
            LaunchProgram::Invalid {
                path: program,
                relative: true,
            }
        };
        let working_directory = if working_directory.is_absolute() {
            PinnedDirectory::pin(&working_directory)
                .map(LaunchDirectory::Pinned)
                .unwrap_or_else(|_| LaunchDirectory::Invalid {
                    path: working_directory,
                    relative: false,
                })
        } else {
            LaunchDirectory::Invalid {
                path: working_directory,
                relative: true,
            }
        };
        Self {
            program,
            arguments: Vec::new(),
            working_directory,
            environment: BTreeMap::new(),
            stderr_capacity: 64 * 1024,
            stdout_line_limit: 256 * 1024,
            stdin_write_timeout: Duration::from_secs(5),
        }
    }

    /// Builds a launch specification from handles pinned by trusted admission.
    #[must_use]
    pub fn from_pinned(program: PinnedExecutable, working_directory: PinnedDirectory) -> Self {
        Self {
            program: LaunchProgram::Pinned {
                executable: program,
                inherit_for_script: false,
            },
            arguments: Vec::new(),
            working_directory: LaunchDirectory::Pinned(working_directory),
            environment: BTreeMap::new(),
            stderr_capacity: 64 * 1024,
            stdout_line_limit: 256 * 1024,
            stdin_write_timeout: Duration::from_secs(5),
        }
    }

    /// Builds a descriptor-pinned launch for an installed shebang adapter.
    ///
    /// The executable descriptor is inherited only by the forked child so the
    /// interpreter can reopen `/proc/self/fd/N`; the parent keeps `FD_CLOEXEC`.
    pub(crate) fn from_pinned_script(
        program: PinnedExecutable,
        working_directory: PinnedDirectory,
    ) -> Self {
        Self {
            program: LaunchProgram::Pinned {
                executable: program,
                inherit_for_script: true,
            },
            arguments: Vec::new(),
            working_directory: LaunchDirectory::Pinned(working_directory),
            environment: BTreeMap::new(),
            stderr_capacity: 64 * 1024,
            stdout_line_limit: 256 * 1024,
            stdin_write_timeout: Duration::from_secs(5),
        }
    }

    /// Validates fields that must be settled before any child is created.
    ///
    /// # Errors
    ///
    /// Rejects non-absolute executables/workspaces, unsafe workspaces, invalid
    /// environment entries, and unbounded I/O settings.
    pub fn validate(&self) -> Result<(), RuntimeLaunchError> {
        match &self.program {
            LaunchProgram::Pinned { .. } => {}
            LaunchProgram::Invalid {
                path,
                relative: true,
            } => return Err(RuntimeLaunchError::ProgramNotAbsolute(path.clone())),
            LaunchProgram::Invalid { path, .. } => {
                return Err(RuntimeLaunchError::ProgramUnavailable(path.clone()));
            }
        }
        match &self.working_directory {
            LaunchDirectory::Pinned(_) => {}
            LaunchDirectory::Invalid {
                path,
                relative: true,
            } => return Err(RuntimeLaunchError::WorkspaceNotAbsolute(path.clone())),
            LaunchDirectory::Invalid { path, .. } => {
                return Err(RuntimeLaunchError::WorkspaceUnavailable {
                    path: path.clone(),
                    source: io::Error::new(
                        io::ErrorKind::NotFound,
                        "Runtime workspace could not be pinned",
                    ),
                });
            }
        }

        validate_bound("stderr_capacity", self.stderr_capacity, MAX_STDERR_CAPACITY)?;
        validate_bound(
            "stdout_line_limit",
            self.stdout_line_limit,
            MAX_STDOUT_LINE_BYTES,
        )?;
        if self.stdin_write_timeout.is_zero() || self.stdin_write_timeout > MAX_STDIN_WRITE_TIMEOUT
        {
            return Err(RuntimeLaunchError::InvalidWriteTimeout {
                actual: self.stdin_write_timeout,
                maximum: MAX_STDIN_WRITE_TIMEOUT,
            });
        }
        if self.environment.len() > MAX_ENVIRONMENT_ENTRIES {
            return Err(RuntimeLaunchError::TooManyEnvironmentEntries {
                actual: self.environment.len(),
                maximum: MAX_ENVIRONMENT_ENTRIES,
            });
        }
        for (name, value) in &self.environment {
            validate_environment_name(name)?;
            if os_str_bytes(value) > MAX_ENVIRONMENT_VALUE_BYTES {
                return Err(RuntimeLaunchError::EnvironmentValueTooLarge {
                    name: name.to_string_lossy().into_owned(),
                    maximum: MAX_ENVIRONMENT_VALUE_BYTES,
                });
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn configure_script_descriptor_inheritance(command: &mut Command, descriptor: i32) {
    // SAFETY: The closure runs after fork and invokes only `fcntl`, which is
    // async-signal-safe. The descriptor belongs to the borrowed launch spec
    // and remains open through `spawn`; changing its child copy cannot alter
    // the parent's `FD_CLOEXEC` flag.
    unsafe {
        command.pre_exec(move || {
            // `open_pinned` creates this descriptor with the only defined file
            // descriptor flag, `FD_CLOEXEC`, so zero clears exactly that flag.
            let result = nix::libc::fcntl(descriptor, nix::libc::F_SETFD, 0);
            if result == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn configure_script_descriptor_inheritance(_command: &mut Command, _descriptor: i32) {}

fn validate_bound(
    field: &'static str,
    actual: usize,
    maximum: usize,
) -> Result<(), RuntimeLaunchError> {
    if actual == 0 || actual > maximum {
        return Err(RuntimeLaunchError::InvalidBound {
            field,
            actual,
            maximum,
        });
    }
    Ok(())
}

fn validate_environment_name(name: &OsStr) -> Result<(), RuntimeLaunchError> {
    let rendered = name.to_string_lossy();
    if rendered.is_empty() || rendered.contains('=') || rendered.contains('\0') {
        return Err(RuntimeLaunchError::InvalidEnvironmentName(
            rendered.into_owned(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn os_str_bytes(value: &OsStr) -> usize {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().len()
}

#[cfg(not(unix))]
fn os_str_bytes(value: &OsStr) -> usize {
    value.to_string_lossy().len()
}

/// Launch validation failure detected before spawn.
#[derive(Debug, Error)]
pub enum RuntimeLaunchError {
    /// Executables are resolved by policy before they reach the supervisor.
    #[error("runtime program must be an absolute path: {0}")]
    ProgramNotAbsolute(PathBuf),
    /// The executable could not be pinned during launch admission.
    #[error("runtime program is unavailable or unsafe: {0}")]
    ProgramUnavailable(PathBuf),
    /// Runtime workspaces must be pinned, not dependent on daemon cwd.
    #[error("runtime workspace must be an absolute path: {0}")]
    WorkspaceNotAbsolute(PathBuf),
    /// Workspace metadata could not be read.
    #[error("runtime workspace is unavailable at {path}: {source}")]
    WorkspaceUnavailable {
        /// Requested workspace.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// Workspace exists but is not a directory.
    #[error("runtime workspace is not a directory: {0}")]
    WorkspaceNotDirectory(PathBuf),
    /// An I/O bound was zero or exceeded the hard safety ceiling.
    #[error("invalid {field} {actual}; expected 1..={maximum}")]
    InvalidBound {
        /// Configuration field.
        field: &'static str,
        /// Rejected value.
        actual: usize,
        /// Hard safety ceiling.
        maximum: usize,
    },
    /// The explicit environment exceeded its entry budget.
    #[error("runtime environment has {actual} entries; maximum is {maximum}")]
    TooManyEnvironmentEntries {
        /// Rejected entry count.
        actual: usize,
        /// Maximum allowed entry count.
        maximum: usize,
    },
    /// An environment name could not be passed safely to `Command`.
    #[error("invalid runtime environment name: {0:?}")]
    InvalidEnvironmentName(String),
    /// One environment value exceeded its bound.
    #[error("runtime environment value for {name:?} exceeds {maximum} bytes")]
    EnvironmentValueTooLarge {
        /// Environment key associated with the rejected value.
        name: String,
        /// Maximum allowed value size.
        maximum: usize,
    },
    /// Stdin writes need a finite non-zero deadline.
    #[error("invalid stdin write timeout {actual:?}; expected 0 < timeout <= {maximum:?}")]
    InvalidWriteTimeout {
        /// Rejected timeout.
        actual: Duration,
        /// Hard safety ceiling.
        maximum: Duration,
    },
}

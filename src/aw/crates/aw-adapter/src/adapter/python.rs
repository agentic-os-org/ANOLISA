//! Private result channel for Python preflight subprocesses.

use super::Probe;
use crate::Result;
use aw_exec::{CleanupMode, CommandSpec, Limits, Output};
use std::{
    env,
    ffi::{OsStr, OsString},
    fs::Permissions,
    os::unix::fs::PermissionsExt,
    time::{Duration, Instant},
};

pub(crate) const PYTHON_PROBE_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) fn isolate_python_environment(command: &mut CommandSpec) -> Result<()> {
    command
        .environment
        .insert("OTEL_SDK_DISABLED".into(), "true".into());
    // aw-exec clears the inherited environment before applying this complete map.
    if let Some(value) = command.environment.remove(OsStr::new("PYTHONPATH")) {
        let entries: Vec<_> = env::split_paths(&value)
            .filter(|path| !path.ends_with("opentelemetry/instrumentation/auto_instrumentation"))
            .collect();
        if !entries.is_empty() {
            command
                .environment
                .insert("PYTHONPATH".into(), env::join_paths(entries)?);
        }
    }
    Ok(())
}

impl Probe<'_> {
    pub(crate) fn run_python(
        &self,
        mut command: CommandSpec,
        input: &[u8],
        limits: Limits,
        deadline: Instant,
        cleanup: CleanupMode,
    ) -> Result<(Output, Vec<u8>)> {
        let directory = tempfile::Builder::new()
            .permissions(Permissions::from_mode(0o700))
            .tempdir()?;
        let path = std::path::absolute(directory.path())?.join("result");
        command.args.push(path.as_os_str().to_owned());
        Ok(aw_exec::run_with_result_pipe(
            &command, input, limits, deadline, self.0, cleanup, &path,
        )?)
    }

    pub(crate) fn python_text(&self, command: CommandSpec, deadline: Instant) -> Result<String> {
        let (output, bytes) = self.run_python(
            command,
            &[],
            super::PROBE_LIMITS,
            deadline,
            CleanupMode::Immediate,
        )?;
        if !output.status.success() {
            return Err("Python preflight probe exited unsuccessfully".into());
        }
        if bytes.is_empty() {
            return Err("Python probe produced an empty result".into());
        }
        Ok(String::from_utf8(bytes)?)
    }

    pub(crate) fn python_output(
        &self,
        command: &CommandSpec,
        args: &[OsString],
        timeout: Duration,
    ) -> Result<String> {
        let deadline = Instant::now() + timeout;
        let mut probe = command.clone();
        probe.args = args.to_vec();
        isolate_python_environment(&mut probe)?;
        self.python_text(probe, deadline)
    }
}

#[cfg(test)]
mod tests;

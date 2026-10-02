//! Build the one-shot cosh-core JSONL transport without exposing prompt text in argv.

use std::io::{BufWriter, Write};
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use super::super::{
    control_protocol, spawn_provider_child, terminate_and_reap_process, AdapterError,
    PreparedInvocation, ProviderPromptArgMode, ProviderStdinMode,
};

/// A spawned cosh-core child and its asynchronous stdin writer.
pub(super) struct SyncCoshCoreChild {
    child: Child,
    writer: SyncCoshCoreWriter,
}

/// Tracks completion and failure of the one-shot cosh-core stdin writer.
pub(super) struct SyncCoshCoreWriter {
    failure: Arc<Mutex<Option<AdapterError>>>,
    thread: JoinHandle<()>,
}

impl SyncCoshCoreWriter {
    pub(super) fn failure_state(&self) -> Arc<Mutex<Option<AdapterError>>> {
        Arc::clone(&self.failure)
    }

    pub(super) fn finish(self) -> Option<AdapterError> {
        if self.thread.join().is_err() {
            return Some(AdapterError {
                message: "cosh-core stdin writer thread panicked".to_string(),
            });
        }
        self.failure
            .lock()
            .ok()
            .and_then(|mut failure| failure.take())
    }
}

impl SyncCoshCoreChild {
    pub(super) fn into_parts(
        self,
    ) -> (Child, Arc<Mutex<Option<AdapterError>>>, SyncCoshCoreWriter) {
        let failure = self.writer.failure_state();
        (self.child, failure, self.writer)
    }
}

/// Returns the writer failure once so the process loop can terminate the child.
pub(super) fn check_writer_failure(
    failure: &Arc<Mutex<Option<AdapterError>>>,
) -> Result<(), AdapterError> {
    failure
        .lock()
        .ok()
        .and_then(|mut failure| failure.take())
        .map_or(Ok(()), Err)
}

/// Spawns cosh-core and starts sending initialize followed by the user message.
pub(super) fn spawn_sync_cosh_core_child(
    prepared: &PreparedInvocation,
    raw_user_input: Option<&str>,
) -> Result<SyncCoshCoreChild, AdapterError> {
    let mut child = spawn_provider_child(
        prepared,
        "cosh-core",
        ProviderStdinMode::Piped,
        ProviderPromptArgMode::None,
    )?;
    // Keep the envelope and hook-only raw input structured on stdin. This
    // avoids exposing prompt contents through argv or hitting ARG_MAX.
    let stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => {
            terminate_and_reap_process(&mut child);
            return Err(AdapterError {
                message: "failed to capture cosh-core stdin".to_string(),
            });
        }
    };
    let initialize = control_protocol::serialize_initialize_without_session_start("init-1");
    let user_message =
        control_protocol::serialize_cosh_core_user_message(&prepared.prompt, raw_user_input, None);
    let failure = Arc::new(Mutex::new(None));
    let failure_for_thread = Arc::clone(&failure);
    let thread = thread::Builder::new()
        .name("cosh-core-stdin".to_string())
        .spawn(move || {
            let mut stdin = BufWriter::new(stdin);
            if let Err(error) =
                write_sync_cosh_core_messages(&mut stdin, &initialize, &user_message)
            {
                if let Ok(mut failure) = failure_for_thread.lock() {
                    *failure = Some(AdapterError {
                        message: format!("failed to write cosh-core user message: {error}"),
                    });
                }
            }
            drop(stdin);
        })
        .map_err(|error| {
            terminate_and_reap_process(&mut child);
            AdapterError {
                message: format!("failed to start cosh-core stdin writer: {error}"),
            }
        })?;
    Ok(SyncCoshCoreChild {
        child,
        writer: SyncCoshCoreWriter { failure, thread },
    })
}

fn write_sync_cosh_core_messages<W: Write>(
    stdin: &mut W,
    initialize: &str,
    user_message: &str,
) -> std::io::Result<()> {
    writeln!(stdin, "{initialize}")
        .and_then(|()| writeln!(stdin, "{user_message}"))
        .and_then(|()| stdin.flush())
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use super::write_sync_cosh_core_messages;

    struct FailsOnSecondWrite {
        writes: usize,
    }

    impl Write for FailsOnSecondWrite {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.writes == 1 {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"));
            }
            self.writes += 1;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn sync_stdin_write_failure_is_propagated() {
        let mut writer = FailsOnSecondWrite { writes: 0 };
        let error = write_sync_cosh_core_messages(&mut writer, "initialize", "user")
            .expect_err("second JSONL write should fail");
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }
}

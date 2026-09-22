//! Fallible standard-output handling for pipeline-safe CLI rendering.

use std::fmt;
use std::io::{self, Write};
use std::sync::{Mutex, MutexGuard, OnceLock};

macro_rules! print {
    ($($arg:tt)*) => {{
        crate::output::write_stdout(format_args!($($arg)*), false);
    }};
}

macro_rules! println {
    () => {{
        crate::output::write_stdout(format_args!(""), true);
    }};
    ($($arg:tt)*) => {{
        crate::output::write_stdout(format_args!($($arg)*), true);
    }};
}

#[derive(Debug, Eq, PartialEq)]
enum WriteStatus {
    Written,
    Closed,
}

enum OutputState {
    Connected,
    Closed,
    Failed(io::Error),
}

struct Output<W> {
    writer: W,
    state: OutputState,
}

impl<W: Write> Output<W> {
    fn new(writer: W) -> Self {
        Self {
            writer,
            state: OutputState::Connected,
        }
    }

    fn write(&mut self, rendered: &str, newline: bool) {
        if !matches!(self.state, OutputState::Connected) {
            return;
        }
        self.state = match write_to(&mut self.writer, rendered, newline) {
            Ok(WriteStatus::Written) => OutputState::Connected,
            Ok(WriteStatus::Closed) => OutputState::Closed,
            Err(error) => OutputState::Failed(error),
        };
    }

    fn take_error(&mut self) -> Option<io::Error> {
        if !matches!(self.state, OutputState::Failed(_)) {
            return None;
        }
        match std::mem::replace(&mut self.state, OutputState::Closed) {
            OutputState::Failed(error) => Some(error),
            OutputState::Connected | OutputState::Closed => None,
        }
    }

    fn flush(&mut self) {
        if !matches!(self.state, OutputState::Connected) {
            return;
        }
        self.state = match self.writer.flush() {
            Ok(()) => OutputState::Connected,
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => OutputState::Closed,
            Err(error) => OutputState::Failed(error),
        };
    }

    fn finish(&mut self) -> Option<io::Error> {
        self.flush();
        self.take_error()
    }
}

static STDOUT: OnceLock<Mutex<Output<io::Stdout>>> = OnceLock::new();

fn lock_stdout() -> MutexGuard<'static, Output<io::Stdout>> {
    let output = STDOUT.get_or_init(|| Mutex::new(Output::new(io::stdout())));
    match output.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn write_to(writer: &mut impl Write, rendered: &str, newline: bool) -> io::Result<WriteStatus> {
    let result = writer.write_all(rendered.as_bytes()).and_then(|()| {
        if newline {
            writer.write_all(b"\n")
        } else {
            Ok(())
        }
    });
    match result {
        Ok(()) => Ok(WriteStatus::Written),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(WriteStatus::Closed),
        Err(error) => Err(error),
    }
}

pub(crate) fn write_stdout(args: fmt::Arguments<'_>, newline: bool) {
    let rendered = args.to_string();
    lock_stdout().write(&rendered, newline);
}

pub(crate) fn flush_stdout() {
    lock_stdout().flush();
}

pub(crate) fn finish_stdout() -> Option<io::Error> {
    lock_stdout().finish()
}

#[cfg(test)]
mod tests {
    use std::fmt;
    use std::io::{self, Write};
    use std::sync::mpsc;
    use std::time::Duration;

    use super::{Output, WriteStatus, write_to};

    struct BrokenPipeWriter;

    impl Write for BrokenPipeWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "consumer closed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct FailOnceWriter {
        writes: usize,
        kind: io::ErrorKind,
    }

    impl Write for FailOnceWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            Err(io::Error::new(self.kind, "injected write failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct RecursiveDisplay;

    impl fmt::Display for RecursiveDisplay {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            super::flush_stdout();
            formatter.write_str("")
        }
    }

    struct FlushErrorWriter;

    impl Write for FlushErrorWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected flush failure",
            ))
        }
    }

    struct BrokenPipeOnFlushWriter;

    impl Write for BrokenPipeOnFlushWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "consumer closed during flush",
            ))
        }
    }

    #[test]
    fn write_reports_closed_when_consumer_closes_pipe() {
        // Given a downstream consumer that has closed its pipe.
        let mut writer = BrokenPipeWriter;

        // When CLI output writes another line.
        let status = write_to(&mut writer, "row", true)
            .expect("BrokenPipe must be a graceful output condition");

        // Then rendering reports the closed channel without panicking.
        assert_eq!(WriteStatus::Closed, status);
    }

    #[test]
    fn output_stops_writing_after_broken_pipe() {
        // Given stdout fails with BrokenPipe on its first write.
        let writer = FailOnceWriter {
            writes: 0,
            kind: io::ErrorKind::BrokenPipe,
        };
        let mut output = Output::new(writer);

        // When two lines are rendered.
        output.write("first", true);
        output.write("second", true);
        assert!(output.take_error().is_none());
        output.write("third", true);

        // Then only the first write reaches the disconnected consumer.
        assert_eq!(1, output.writer.writes);
    }

    #[test]
    fn output_defers_non_pipe_error_until_command_finishes() {
        // Given stdout fails for a reason other than a closed consumer.
        let writer = FailOnceWriter {
            writes: 0,
            kind: io::ErrorKind::PermissionDenied,
        };
        let mut output = Output::new(writer);

        // When a command renders output and later inspects the channel.
        output.write("result", true);
        let error = output
            .take_error()
            .expect("non-pipe output failures must remain actionable");

        // Then the original error classification is preserved.
        assert_eq!(io::ErrorKind::PermissionDenied, error.kind());
    }

    #[test]
    fn formatting_can_write_output_without_deadlocking() {
        // Given a Display implementation that emits nested CLI output.
        let (sender, receiver) = mpsc::channel();

        // When the value is rendered through the global output path.
        std::thread::spawn(move || {
            super::write_stdout(format_args!("{RecursiveDisplay}"), false);
            let _ = sender.send(());
        });

        // Then formatting completes without recursively locking stdout.
        assert!(receiver.recv_timeout(Duration::from_secs(1)).is_ok());
    }

    #[test]
    fn finalization_surfaces_non_pipe_flush_error() {
        // Given a prompt writer that fails only when flushed.
        let mut output = Output::new(FlushErrorWriter);
        output.write("prompt", false);

        // When the process boundary finalizes stdout.
        let error = output
            .finish()
            .expect("non-pipe flush failures must remain actionable");

        // Then the flush error remains available to the process boundary.
        assert_eq!(io::ErrorKind::PermissionDenied, error.kind());
    }

    #[test]
    fn finalization_is_safe_to_repeat() {
        // Given stdout whose terminal flush fails once finalization begins.
        let mut output = Output::new(FlushErrorWriter);
        output.write("result", false);

        // When stdout is finalized twice.
        let first = output.finish();
        let second = output.finish();

        // Then the error is reported once and the terminal state remains stable.
        assert_eq!(
            Some(io::ErrorKind::PermissionDenied),
            first.as_ref().map(io::Error::kind)
        );
        assert!(second.is_none());
    }

    #[test]
    fn finalization_ignores_broken_pipe_from_flush() {
        // Given writes succeed before the downstream consumer closes.
        let mut output = Output::new(BrokenPipeOnFlushWriter);
        output.write("result", false);

        // When the process boundary performs the terminal flush.
        let error = output.finish();

        // Then the closed consumer is not reported as an output failure.
        assert!(error.is_none());
    }

    #[test]
    fn finalization_succeeds_for_healthy_writer() {
        // Given a connected writer containing rendered output.
        let mut output = Output::new(Vec::new());
        output.write("result", true);

        // When the process boundary finalizes stdout.
        let error = output.finish();

        // Then finalization succeeds and preserves the rendered bytes.
        assert!(error.is_none());
        assert_eq!(b"result\n", output.writer.as_slice());
    }
}

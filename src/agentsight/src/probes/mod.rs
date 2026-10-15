#![allow(clippy::module_inception)]
pub mod filewatch;
pub mod filewrite;
pub mod pidns;
pub mod probes;
pub mod procmon;
pub mod proctrace;
pub mod shared_maps;
pub mod sslsniff;
pub mod tcpsniff;
pub mod udpdns;

mod codex_offsets;
mod elf_buildid;

/// Why a ring-buffer poll loop ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PollEnd {
    /// The stop flag was set.
    Stopped,
    /// The ring buffer failed; the message is for the caller to log.
    Failed(String),
}

/// How one poll error should be handled.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PollFailure {
    /// `EINTR`: the syscall was cut short by a signal, so the loop retries.
    Interrupted,
    /// Anything else.
    Fatal(String),
}

/// Drive one ring buffer until the stop flag is set or the buffer fails.
///
/// `Interrupted` is EINTR — a signal delivered to this thread cut the poll
/// short — and the correct handling is to retry it, which is what every other
/// syscall loop in this crate does. Treating it as a shutdown made the collector
/// go permanently deaf: the thread exits, nothing sets the stop flag, and the
/// owner still holds a sender clone, so `recv` reports an idle machine instead
/// of a dead collector. Every other error ends the loop and is handed back with
/// its message, because only the caller can log it.
pub(crate) fn drive_poll_loop(
    timeout: std::time::Duration,
    stop: &std::sync::atomic::AtomicBool,
    mut poll: impl FnMut(std::time::Duration) -> Result<(), PollFailure>,
) -> PollEnd {
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        match poll(timeout) {
            Ok(()) => {}
            Err(PollFailure::Interrupted) => continue,
            Err(PollFailure::Fatal(message)) => return PollEnd::Failed(message),
        }
    }
    PollEnd::Stopped
}

// Re-export commonly used types
pub use filewatch::{FileWatch, FileWatchEvent};
pub use filewrite::{FileWrite as FileWriteProbe, FileWriteEvent};
pub use pidns::proc_root_is_init_pidns;
pub use probes::{ChannelWatermarks, Probes, ProbesPoller};
pub use procmon::{Event as ProcMonEventExt, ProcMon, ProcMonEvent};
pub use proctrace::{ProcPoller, ProcTrace, VariableEvent as ProcEvent};
pub use shared_maps::{MapKind, SharedMaps};
pub use sslsniff::{SslEvent, SslPoller, SslSniff};
pub use tcpsniff::TcpSniff;
pub use udpdns::{UdpDns, UdpDnsEvent};

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::{PollEnd, PollFailure, drive_poll_loop};

    /// EINTR is routine — any signal delivered to the poll thread cuts the
    /// syscall short — so the loop must keep polling. Breaking out instead left
    /// the collector deaf for the rest of the process's life, with the stop flag
    /// still clear and the owner still holding a sender clone.
    #[test]
    fn an_interrupted_poll_is_retried() {
        let stop = AtomicBool::new(false);
        let mut polls = 0usize;
        let outcome = drive_poll_loop(Duration::from_millis(1), &stop, |_| {
            polls += 1;
            if polls < 3 {
                return Err(PollFailure::Interrupted);
            }
            stop.store(true, Ordering::Relaxed);
            Ok(())
        });

        assert_eq!(outcome, PollEnd::Stopped);
        assert_eq!(polls, 3, "the loop must keep polling after EINTR");
    }

    #[test]
    fn a_fatal_poll_error_ends_the_loop_with_its_message() {
        let stop = AtomicBool::new(false);
        let outcome = drive_poll_loop(Duration::from_millis(1), &stop, |_| {
            Err(PollFailure::Fatal("boom".to_string()))
        });

        assert_eq!(outcome, PollEnd::Failed("boom".to_string()));
    }

    #[test]
    fn a_set_stop_flag_ends_the_loop_before_polling() {
        let stop = AtomicBool::new(true);
        let mut polls = 0usize;
        let outcome = drive_poll_loop(Duration::from_millis(1), &stop, |_| {
            polls += 1;
            Ok(())
        });

        assert_eq!(outcome, PollEnd::Stopped);
        assert_eq!(polls, 0);
    }
}

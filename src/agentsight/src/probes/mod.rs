#![allow(clippy::module_inception)]
pub(crate) mod budget;
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

/// Raises `RLIMIT_MEMLOCK` before any BPF object is loaded and returns the
/// soft limit left in effect, or `None` when it is unlimited.
///
/// libbpf only bumps the limit when its memcg-accounting probe fails, and that
/// probe keys on the `bpf_ktime_get_coarse_ns` helper. Vendor 5.10 kernels
/// (Alibaba Cloud Linux) backport the helper without memcg accounting, so libbpf
/// skips the bump while maps are still charged to memlock — under the 64 KiB
/// container default every map then fails with EPERM. The bump is harmless on
/// kernels that do use memcg accounting, which ignore this limit for BPF.
pub(crate) fn raise_memlock_limit() -> Option<u64> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit only writes into the rlimit we pass by pointer.
    if unsafe { libc::getrlimit(libc::RLIMIT_MEMLOCK, &mut limit) } != 0 {
        log::warn!(
            "getrlimit(RLIMIT_MEMLOCK) failed: {}",
            std::io::Error::last_os_error()
        );
        return None;
    }
    for (cur, max) in memlock_raise_attempts(limit.rlim_cur, limit.rlim_max) {
        let target = libc::rlimit {
            rlim_cur: cur,
            rlim_max: max,
        };
        // SAFETY: setrlimit only reads the rlimit we pass by pointer.
        if unsafe { libc::setrlimit(libc::RLIMIT_MEMLOCK, &target) } == 0 {
            limit = target;
            break;
        }
    }
    if limit.rlim_cur == libc::RLIM_INFINITY {
        return None;
    }
    // Info, not warn: memcg-accounting kernels work fine under a finite limit;
    // probe creation attaches `memlock_hint` to its error when it does matter.
    log::info!(
        "RLIMIT_MEMLOCK stays at {} KiB (raising it needs CAP_SYS_RESOURCE)",
        limit.rlim_cur / 1024
    );
    Some(limit.rlim_cur)
}

/// Limits to try in order: unlimited first (needs `CAP_SYS_RESOURCE` unless the
/// hard limit is already unlimited), then soft up to hard, which any process may do.
fn memlock_raise_attempts(cur: u64, max: u64) -> Vec<(u64, u64)> {
    if cur == libc::RLIM_INFINITY {
        return Vec::new();
    }
    let mut attempts = vec![(libc::RLIM_INFINITY, libc::RLIM_INFINITY)];
    if max > cur && max != libc::RLIM_INFINITY {
        attempts.push((max, max));
    }
    attempts
}

/// Operator-facing explanation for a finite memlock limit, attached to probe
/// creation errors so an EPERM points at its likely cause.
pub(crate) fn memlock_hint(limit_bytes: u64) -> String {
    format!(
        "RLIMIT_MEMLOCK is {} KiB and could not be raised to unlimited (that needs \
         CAP_SYS_RESOURCE); kernels that charge BPF maps to memlock reject probe creation \
         with EPERM under this limit. Grant CAP_SYS_RESOURCE, or raise the limit before start \
         (`ulimit -l unlimited`, `docker run --ulimit memlock=-1`, systemd \
         `LimitMEMLOCK=infinity`)",
        limit_bytes / 1024
    )
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

    use super::{PollEnd, PollFailure, drive_poll_loop, memlock_hint, memlock_raise_attempts};

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

    #[test]
    fn an_unlimited_memlock_is_left_alone() {
        assert!(memlock_raise_attempts(libc::RLIM_INFINITY, libc::RLIM_INFINITY).is_empty());
    }

    /// The container default: soft == hard == 64 KiB. Only the privileged
    /// unlimited attempt can help; there is no headroom to fall back on.
    #[test]
    fn a_capped_memlock_tries_only_unlimited() {
        assert_eq!(
            memlock_raise_attempts(65536, 65536),
            vec![(libc::RLIM_INFINITY, libc::RLIM_INFINITY)]
        );
    }

    /// Without CAP_SYS_RESOURCE the unlimited attempt fails, but raising the
    /// soft limit to the hard one never needs privilege.
    #[test]
    fn a_soft_memlock_below_hard_falls_back_to_hard() {
        assert_eq!(
            memlock_raise_attempts(65536, 8 << 20),
            vec![
                (libc::RLIM_INFINITY, libc::RLIM_INFINITY),
                (8 << 20, 8 << 20)
            ]
        );
        assert_eq!(
            memlock_raise_attempts(65536, libc::RLIM_INFINITY),
            vec![(libc::RLIM_INFINITY, libc::RLIM_INFINITY)]
        );
    }

    #[test]
    fn the_memlock_hint_names_the_limit_and_the_remedy() {
        let hint = memlock_hint(65536);
        assert!(hint.contains("64 KiB"), "{hint}");
        assert!(hint.contains("CAP_SYS_RESOURCE"), "{hint}");
        assert!(hint.contains("--ulimit memlock=-1"), "{hint}");
    }
}

//! Inherited terminal execution reusing the bounded command process-group owner.

use super::{child::OwnedChild, io_error};
use crate::{CommandSpec, Error};
use std::{
    io,
    process::ExitStatus,
    sync::atomic::{AtomicI32, Ordering},
    thread,
    time::{Duration, Instant},
};

struct Terminal(Option<i32>);

impl Terminal {
    fn transfer(group: u32) -> io::Result<Self> {
        // SAFETY: terminal queries use a valid standard descriptor; a closed or
        // non-terminal stdin simply has no foreground ownership to transfer.
        if unsafe { libc::isatty(libc::STDIN_FILENO) } == 0 {
            return Ok(Self(None));
        }
        let previous = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
        if previous < 0 {
            return Err(io::Error::last_os_error());
        }
        // A background caller must never take another job's foreground terminal.
        if previous != unsafe { libc::getpgrp() } {
            return Ok(Self(None));
        }
        Self::set(group as i32)?;
        Ok(Self(Some(previous)))
    }

    fn set(group: i32) -> io::Result<()> {
        // SAFETY: restoration runs while this process is in the background, so
        // block SIGTTOU on this thread for the terminal operation, then restore.
        unsafe {
            let mut set = std::mem::zeroed::<libc::sigset_t>();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGTTOU);
            let mut previous = std::mem::zeroed::<libc::sigset_t>();
            let code = libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut previous);
            if code != 0 {
                return Err(io::Error::from_raw_os_error(code));
            }
            let result = libc::tcsetpgrp(libc::STDIN_FILENO, group);
            let error = io::Error::last_os_error();
            let restored =
                libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
            if result < 0 {
                return Err(error);
            }
            if restored != 0 {
                return Err(io::Error::from_raw_os_error(restored));
            }
            Ok(())
        }
    }

    fn restore(&mut self) -> io::Result<()> {
        if let Some(group) = self.0 {
            Self::set(group)?;
            self.0 = None;
        }
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

pub(crate) fn run(spec: &CommandSpec, signal: &AtomicI32) -> Result<ExitStatus, Error> {
    let mut child = OwnedChild::spawn_foreground(spec)?;
    let mut terminal = match Terminal::transfer(child.id()) {
        Ok(terminal) => terminal,
        Err(error) => {
            child.cleanup().map_err(|source| Error::Cleanup {
                pid: child.id(),
                source,
            })?;
            return Err(io_error("terminal transfer", error));
        }
    };
    let waited = (|| {
        if terminal.0.is_some() {
            // A fast child may have read stdin before the foreground handoff.
            child.signal(libc::SIGCONT)?;
        }
        let mut stopping = None;
        while !child.observe_exit()? {
            let pending = signal.swap(0, Ordering::AcqRel);
            if pending != 0 {
                child.signal(pending)?;
                stopping.get_or_insert(Instant::now() + Duration::from_secs(2));
            }
            if stopping.is_some_and(|deadline| Instant::now() >= deadline) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok::<(), io::Error>(())
    })();
    let cleaned = child.cleanup().map_err(|source| Error::Cleanup {
        pid: child.id(),
        source,
    });
    let restored = terminal.restore();
    let status = cleaned?;
    restored.map_err(|e| io_error("terminal restore", e))?;
    waited.map_err(|e| io_error("foreground wait", e))?;
    Ok(status)
}

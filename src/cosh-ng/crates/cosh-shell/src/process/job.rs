use std::io;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::{Duration, Instant};

use nix::libc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct JobDeadline {
    pub(crate) started_ns: u64,
    pub(crate) expires_ns: u64,
}

impl JobDeadline {
    pub(crate) fn at_instant(deadline: Instant, ceiling: Duration) -> io::Result<Self> {
        let started_ns = monotonic_ns()?;
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .min(ceiling);
        let budget_ns = u64::try_from(remaining.as_nanos())
            .map_err(|_| io::Error::from_raw_os_error(libc::EOVERFLOW))?;
        Ok(Self {
            started_ns,
            expires_ns: started_ns
                .checked_add(budget_ns)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EOVERFLOW))?,
        })
    }

    pub(crate) fn after(budget: Duration) -> io::Result<Self> {
        let budget_ns = u64::try_from(budget.as_nanos())
            .map_err(|_| io::Error::from_raw_os_error(libc::EOVERFLOW))?;
        let started_ns = monotonic_ns()?;
        Ok(Self {
            started_ns,
            expires_ns: started_ns
                .checked_add(budget_ns)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EOVERFLOW))?,
        })
    }

    pub(crate) fn shorten_within(&mut self, budget: Duration) -> io::Result<()> {
        let earlier = Self::after(budget)?;
        self.expires_ns = self.expires_ns.min(earlier.expires_ns);
        Ok(())
    }

    pub(crate) fn elapsed(self) -> io::Result<Duration> {
        Ok(Duration::from_nanos(
            monotonic_ns()?.saturating_sub(self.started_ns),
        ))
    }

    pub(crate) fn remaining(self) -> io::Result<Duration> {
        Ok(Duration::from_nanos(
            self.expires_ns.saturating_sub(monotonic_ns()?),
        ))
    }

    pub(crate) fn instant(self) -> io::Result<Instant> {
        let before = Instant::now();
        before
            .checked_add(self.remaining()?)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EOVERFLOW))
    }
}

pub(crate) fn configure_isolated_worker(command: &mut Command, deadline: JobDeadline) {
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = libc::SIG_DFL;
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(libc::SIGALRM, &action, std::ptr::null_mut()) < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut mask: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            libc::sigaddset(&mut mask, libc::SIGALRM);
            if libc::sigprocmask(libc::SIG_UNBLOCK, &mask, std::ptr::null_mut()) < 0 {
                return Err(io::Error::last_os_error());
            }
            arm_deadline(deadline)
        });
    }
}

pub(crate) fn install_worker_timeout_handler() -> io::Result<()> {
    let pid = unsafe { libc::getpid() };
    if unsafe { libc::getsid(0) } != pid || unsafe { libc::getpgrp() } != pid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "health worker is not isolated",
        ));
    }
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = abort_worker_job as *const () as usize;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
    }
    if unsafe { libc::sigaction(libc::SIGALRM, &action, std::ptr::null_mut()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

extern "C" fn abort_worker_job(_: libc::c_int) {
    unsafe {
        libc::kill(-libc::getpid(), libc::SIGKILL);
        libc::_exit(124);
    }
}

pub(crate) fn timer_remaining() -> io::Result<Duration> {
    let mut timer: libc::itimerval = unsafe { std::mem::zeroed() };
    #[cfg(target_os = "linux")]
    let result = unsafe { libc::syscall(libc::SYS_getitimer, 0, &mut timer) };
    #[cfg(not(target_os = "linux"))]
    let result = unsafe { libc::getitimer(libc::ITIMER_REAL, &mut timer) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    if timer.it_value.tv_sec < 0 || timer.it_value.tv_usec < 0 {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    Ok(Duration::from_secs(timer.it_value.tv_sec as u64)
        + Duration::from_micros(timer.it_value.tv_usec as u64))
}

fn arm_deadline(deadline: JobDeadline) -> io::Result<()> {
    let remaining = deadline.remaining()?;
    if remaining < Duration::from_micros(1) {
        unsafe {
            libc::_exit(124);
        }
    }
    let timer = libc::itimerval {
        it_interval: libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
        it_value: libc::timeval {
            tv_sec: remaining.as_secs() as libc::time_t,
            tv_usec: remaining.subsec_micros() as _,
        },
    };
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::syscall(
            libc::SYS_setitimer,
            0,
            &timer,
            std::ptr::null::<libc::itimerval>(),
        )
    };
    #[cfg(not(target_os = "linux"))]
    let result = unsafe { libc::setitimer(libc::ITIMER_REAL, &timer, std::ptr::null_mut()) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn configure_cloexec(command: &mut Command, first: i32) -> io::Result<()> {
    #[cfg(not(target_os = "linux"))]
    let limit = {
        let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } < 0 {
            return Err(io::Error::last_os_error());
        }
        limit.rlim_cur.min(i32::MAX as _) as i32
    };
    unsafe {
        command.pre_exec(move || {
            #[cfg(target_os = "linux")]
            {
                if libc::syscall(libc::SYS_close_range, first as u32, u32::MAX, 4u32) >= 0 {
                    return Ok(());
                }
                let error = io::Error::last_os_error();
                if !matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOSYS)) {
                    return Err(error);
                }
                cloexec_open_descriptors(first)
            }
            #[cfg(not(target_os = "linux"))]
            {
                for fd in first..limit {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags >= 0 && libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            }
        });
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn cloexec_open_descriptors(first: i32) -> io::Result<()> {
    // pre_exec cannot allocate; scan actual FDs rather than a potentially million-entry rlimit.
    let directory = unsafe {
        libc::open(
            c"/proc/self/fd".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if directory < 0 {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        let mut buffer = [0u8; 4096];
        loop {
            let length = unsafe {
                libc::syscall(
                    libc::SYS_getdents64,
                    directory,
                    buffer.as_mut_ptr(),
                    buffer.len(),
                )
            };
            if length < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(error);
            }
            if length == 0 {
                return Ok(());
            }
            let mut records = buffer
                .get(..length as usize)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EIO))?;
            while !records.is_empty() {
                if records.len() < 20 {
                    return Err(io::Error::from_raw_os_error(libc::EIO));
                }
                let size = u16::from_ne_bytes([records[16], records[17]]) as usize;
                if size < 20 || size > records.len() {
                    return Err(io::Error::from_raw_os_error(libc::EIO));
                }
                let name = &records[19..size];
                let end = name
                    .iter()
                    .position(|byte| *byte == 0)
                    .ok_or_else(|| io::Error::from_raw_os_error(libc::EIO))?;
                let name = &name[..end];
                if name != b"." && name != b".." {
                    let fd = name
                        .iter()
                        .try_fold(0i32, |fd, byte| {
                            byte.is_ascii_digit().then_some(())?;
                            fd.checked_mul(10)?.checked_add(i32::from(*byte - b'0'))
                        })
                        .filter(|_| !name.is_empty())
                        .ok_or_else(|| io::Error::from_raw_os_error(libc::EIO))?;
                    if fd >= first {
                        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
                        if flags < 0
                            || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) }
                                < 0
                        {
                            return Err(io::Error::last_os_error());
                        }
                    }
                }
                records = &records[size..];
            }
        }
    })();
    let closed = unsafe { libc::close(directory) };
    if closed < 0 && result.is_ok() {
        return Err(io::Error::last_os_error());
    }
    result
}

pub(crate) fn monotonic_ns() -> io::Result<u64> {
    let mut value: libc::timespec = unsafe { std::mem::zeroed() };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut value) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let seconds =
        u64::try_from(value.tv_sec).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|seconds| seconds.checked_add(value.tv_nsec as u64))
        .ok_or_else(|| io::Error::from_raw_os_error(libc::EOVERFLOW))
}

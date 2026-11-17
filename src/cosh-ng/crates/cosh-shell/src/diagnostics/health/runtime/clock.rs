//! Health-owned BOOTTIME deadline clock; generic waits receive conservative local conversions.

use std::io;
use std::time::{Duration, Instant};

use nix::libc;
use serde::{Deserialize, Serialize};

use super::job::JobDeadline;

#[cfg(target_os = "linux")]
thread_local! {
    static ALARM_TIMER: std::cell::Cell<Option<libc::timer_t>> = const { std::cell::Cell::new(None) };
}

pub(crate) fn arm_alarm(deadline: HealthDeadline) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        ALARM_TIMER.with(|slot| {
            let timer = if let Some(timer) = slot.get() {
                timer
            } else {
                let mut event: libc::sigevent = unsafe { std::mem::zeroed() };
                event.sigev_notify = libc::SIGEV_SIGNAL;
                event.sigev_signo = libc::SIGALRM;
                let mut timer = std::ptr::null_mut();
                if unsafe { libc::timer_create(libc::CLOCK_BOOTTIME, &mut event, &mut timer) } < 0 {
                    return Err(io::Error::last_os_error());
                }
                slot.set(Some(timer));
                timer
            };
            let timer_spec = libc::itimerspec {
                it_interval: libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                },
                it_value: libc::timespec {
                    tv_sec: (deadline.expires_ns / 1_000_000_000) as _,
                    tv_nsec: (deadline.expires_ns % 1_000_000_000) as _,
                },
            };
            if unsafe {
                libc::timer_settime(
                    timer,
                    libc::TIMER_ABSTIME,
                    &timer_spec,
                    std::ptr::null_mut(),
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            // BOOTTIME now owns the deadline; the pre-exec timer must not fire later.
            let disabled: libc::itimerval = unsafe { std::mem::zeroed() };
            if unsafe {
                libc::syscall(
                    libc::SYS_setitimer,
                    libc::ITIMER_REAL,
                    &disabled,
                    std::ptr::null_mut::<libc::itimerval>(),
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        })
    }
    #[cfg(not(target_os = "linux"))]
    super::job::arm_deadline(deadline.job_deadline()?)
}

pub(crate) fn arm_banner_exit(deadline: HealthDeadline) -> io::Result<()> {
    extern "C" fn exit_banner(_: libc::c_int) {
        unsafe { libc::_exit(0) }
    }
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = exit_banner as *const () as usize;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
    }
    if unsafe { libc::sigaction(libc::SIGALRM, &action, std::ptr::null_mut()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut mask: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigemptyset(&mut mask);
        libc::sigaddset(&mut mask, libc::SIGALRM);
    }
    if unsafe { libc::sigprocmask(libc::SIG_UNBLOCK, &mask, std::ptr::null_mut()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    arm_alarm(deadline)
}

pub(crate) fn boottime_ns() -> io::Result<u64> {
    #[cfg(target_os = "linux")]
    let clock = libc::CLOCK_BOOTTIME;
    #[cfg(not(target_os = "linux"))]
    let clock = libc::CLOCK_MONOTONIC;
    let mut value: libc::timespec = unsafe { std::mem::zeroed() };
    if unsafe { libc::clock_gettime(clock, &mut value) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let seconds =
        u64::try_from(value.tv_sec).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|seconds| seconds.checked_add(value.tv_nsec as u64))
        .ok_or_else(|| io::Error::from_raw_os_error(libc::EOVERFLOW))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct HealthDeadline {
    pub(crate) started_ns: u64,
    pub(crate) expires_ns: u64,
}

impl HealthDeadline {
    pub(crate) fn after(budget: Duration) -> io::Result<Self> {
        let budget_ns = u64::try_from(budget.as_nanos())
            .map_err(|_| io::Error::from_raw_os_error(libc::EOVERFLOW))?;
        let started_ns = boottime_ns()?;
        Ok(Self {
            started_ns,
            expires_ns: started_ns
                .checked_add(budget_ns)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EOVERFLOW))?,
        })
    }

    pub(crate) fn at_boottime(expires_ns: u64) -> io::Result<Self> {
        Ok(Self {
            started_ns: boottime_ns()?,
            expires_ns,
        })
    }

    pub(crate) fn remaining(self) -> io::Result<Duration> {
        Ok(Duration::from_nanos(
            self.expires_ns.saturating_sub(boottime_ns()?),
        ))
    }

    pub(crate) fn elapsed(self) -> io::Result<Duration> {
        Ok(Duration::from_nanos(
            boottime_ns()?.saturating_sub(self.started_ns),
        ))
    }

    pub(crate) fn shorten_within(&mut self, budget: Duration) -> io::Result<()> {
        let earlier = Self::after(budget)?;
        self.expires_ns = self.expires_ns.min(earlier.expires_ns);
        Ok(())
    }

    pub(crate) fn clamp_remaining(&mut self, budget: Duration) -> io::Result<()> {
        self.shorten_within(budget)
    }

    pub(crate) fn shorten_by(&mut self, reservation: Duration) {
        let reservation_ns = u64::try_from(reservation.as_nanos()).unwrap_or(u64::MAX);
        self.expires_ns = self.expires_ns.saturating_sub(reservation_ns);
    }

    pub(crate) fn local_instant(self) -> io::Result<Instant> {
        let before = Instant::now();
        before
            .checked_add(self.remaining()?)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EOVERFLOW))
    }

    pub(crate) fn job_deadline(self) -> io::Result<JobDeadline> {
        JobDeadline::after(self.remaining()?)
    }
}

#[cfg(test)]
library_tests! {
mod tests {
    use super::*;

    #[test]
    fn boottime_clock_is_monotonic_within_a_call() {
        let first = boottime_ns().expect("boottime clock");
        let second = boottime_ns().expect("boottime clock");
        assert!(second >= first);
    }

    #[test]
    fn deadline_remaining_and_elapsed_track_the_boottime_domain() {
        let deadline = HealthDeadline::after(Duration::from_millis(50)).expect("deadline");
        let remaining = deadline.remaining().expect("remaining");
        assert!(remaining <= Duration::from_millis(50));
        assert!(remaining > Duration::from_millis(10));
        assert!(deadline.elapsed().expect("elapsed") < Duration::from_millis(50));
    }

    #[test]
    fn shorten_and_clamp_only_move_the_deadline_earlier() {
        let mut deadline = HealthDeadline::after(Duration::from_millis(100)).expect("deadline");
        let original = deadline.expires_ns;
        deadline
            .shorten_within(Duration::from_millis(20))
            .expect("shorten");
        assert!(deadline.expires_ns < original);
        let shortened = deadline.expires_ns;
        deadline
            .clamp_remaining(Duration::from_secs(5))
            .expect("clamp");
        assert_eq!(deadline.expires_ns, shortened);
        deadline.shorten_by(Duration::from_millis(5));
        assert!(deadline.expires_ns < shortened);
    }

    #[test]
    fn expired_absolute_deadline_has_zero_remaining() {
        let deadline = HealthDeadline::at_boottime(0).expect("deadline");
        assert!(deadline.remaining().expect("remaining").is_zero());
    }

    #[test]
    fn local_conversions_are_conservative() {
        let deadline = HealthDeadline::after(Duration::from_millis(80)).expect("deadline");
        let instant = deadline.local_instant().expect("instant");
        let wait = instant.saturating_duration_since(Instant::now());
        assert!(wait <= Duration::from_millis(80));
        let job = deadline.job_deadline().expect("job deadline");
        assert!(job.remaining().expect("job remaining") <= Duration::from_millis(80));
    }
}
}

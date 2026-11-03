//! Cooperative CPU pacing shared by capture delivery and SQLite maintenance.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

const BURST_CREDIT_NS: i128 = 2_000_000;

/// Background work yields between indivisible operations to queued captures.
#[derive(Clone, Copy)]
pub(crate) enum WorkClass {
    /// Analyze and persist an owned capture before accepting another heavy task.
    Capture,
    /// Perform one resumable retention or checkpoint operation.
    Maintenance,
}

impl WorkClass {
    fn label(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::Maintenance => "maintenance",
        }
    }
}

struct State {
    wall: Instant,
    cpu: Duration,
    credit_ns: i128,
    active: bool,
    owner: Option<std::thread::ThreadId>,
}

impl State {
    fn update(&mut self, wall: Instant, cpu: Duration, percent: u8) {
        let earned = wall.duration_since(self.wall).as_nanos() as i128 * percent as i128 / 100;
        let spent = cpu.saturating_sub(self.cpu).as_nanos() as i128;
        self.credit_ns = (self.credit_ns + earned - spent).min(BURST_CREDIT_NS);
        self.wall = wall;
        self.cpu = cpu;
    }
}

/// Counts all process threads, carries CPU debt forward, and serializes heavy jobs.
pub(crate) struct CpuBudget {
    percent: u8,
    state: Mutex<State>,
}

impl CpuBudget {
    /// Wait between jobs while allowing the probe poller and shared worker to progress.
    pub(crate) fn acquire(&self, class: WorkClass) -> Result<CpuPermit<'_>> {
        self.acquire_while(class, || true)?
            .context("CPU admission cancelled")
    }

    /// Allow bounded waits and shutdown cancellation, including nested admission.
    pub(crate) fn acquire_while(
        &self,
        class: WorkClass,
        keep_waiting: impl Fn() -> bool,
    ) -> Result<Option<CpuPermit<'_>>> {
        if !keep_waiting() {
            return Ok(None);
        }
        // Forced session eviction can finish delivery inside an already admitted
        // capture. Its CPU remains charged to the outer permit.
        {
            let state = self
                .state
                .lock()
                .map_err(|error| anyhow::anyhow!("CPU budget mutex poisoned: {error}"))?;
            if state.owner == Some(std::thread::current().id()) {
                return Ok(Some(CpuPermit {
                    budget: self,
                    class,
                    started_cpu: state.cpu,
                    nested: true,
                }));
            }
        }
        while keep_waiting() {
            if let Some(permit) = self.try_acquire(class)? {
                return Ok(Some(permit));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(None)
    }
    /// Construct an enabled budget; zero is represented by omitting the budget.
    pub(crate) fn new(percent: u8) -> Result<Self> {
        anyhow::ensure!(
            (1..=100).contains(&percent),
            "CPU budget must be between 1 and 100 percent"
        );
        Ok(Self {
            percent,
            state: Mutex::new(State {
                wall: Instant::now(),
                cpu: process_cpu_time()?,
                credit_ns: 0,
                active: false,
                owner: None,
            }),
        })
    }

    /// Admit one non-preemptible job without sleeping or holding a SQLite lock.
    pub(crate) fn try_acquire(&self, class: WorkClass) -> Result<Option<CpuPermit<'_>>> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow::anyhow!("CPU budget mutex poisoned: {error}"))?;
        state.update(Instant::now(), process_cpu_time()?, self.percent);
        if state.active || state.credit_ns < 0 {
            metrics::counter!("agentsight_cpu_budget_deferred_total", "work" => class.label())
                .increment(1);
            return Ok(None);
        }
        state.active = true;
        state.owner = Some(std::thread::current().id());
        Ok(Some(CpuPermit {
            budget: self,
            class,
            started_cpu: state.cpu,
            nested: false,
        }))
    }
}

/// Release admission and charge the actual process CPU cost, including other threads.
pub(crate) struct CpuPermit<'a> {
    budget: &'a CpuBudget,
    class: WorkClass,
    started_cpu: Duration,
    nested: bool,
}

impl Drop for CpuPermit<'_> {
    fn drop(&mut self) {
        if self.nested {
            return;
        }
        if let Ok(mut state) = self.budget.state.lock() {
            let cpu = process_cpu_time();
            if let Ok(cpu) = cpu {
                state.update(Instant::now(), cpu, self.budget.percent);
                metrics::counter!("agentsight_cpu_budget_work_nanoseconds_total", "work" => self.class.label())
                    .increment(cpu.saturating_sub(self.started_cpu).as_nanos().min(u64::MAX as u128) as u64);
            }
            if let Err(error) = cpu {
                log::error!("Failed to account CPU budget work: {error}");
            }
            state.active = false;
            state.owner = None;
        }
    }
}

#[cfg(unix)]
fn process_cpu_time() -> Result<Duration> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: the clock does not retain the valid, writable timespec pointer.
    if unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) } != 0 {
        return Err(std::io::Error::last_os_error()).context("read process CPU clock");
    }
    Ok(Duration::new(time.tv_sec as u64, time.tv_nsec as u32))
}

#[cfg(not(unix))]
fn process_cpu_time() -> Result<Duration> {
    anyhow::bail!("process CPU pacing requires a Unix process CPU clock")
}

#[cfg(test)]
mod tests {
    use super::*;

    impl CpuBudget {
        /// Exercise capture preservation when accounting fails through mutex poisoning.
        pub(crate) fn poison_for_test(&self) {
            assert!(
                std::panic::catch_unwind(|| {
                    let _guard = self.state.lock().unwrap();
                    panic!("CPU budget poison fixture");
                })
                .is_err()
            );
        }
    }

    #[test]
    fn invalid_targets_and_poisoned_accounting_are_errors() {
        assert!(CpuBudget::new(0).is_err());
        assert!(CpuBudget::new(101).is_err());
        let budget = CpuBudget::new(25).unwrap();
        budget.poison_for_test();
        assert!(budget.try_acquire(WorkClass::Capture).is_err());
        assert!(budget.acquire(WorkClass::Maintenance).is_err());
    }

    #[test]
    fn idle_credit_is_bounded_and_overspending_is_repaid() {
        let now = Instant::now();
        let mut state = State {
            wall: now,
            cpu: Duration::ZERO,
            credit_ns: 0,
            active: false,
            owner: None,
        };
        state.update(now + Duration::from_secs(100), Duration::ZERO, 25);
        assert_eq!(state.credit_ns, BURST_CREDIT_NS);
        state.update(
            now + Duration::from_secs(100),
            Duration::from_millis(22),
            25,
        );
        assert_eq!(state.credit_ns, -20_000_000);
        state.update(
            now + Duration::from_millis(100_100),
            Duration::from_millis(32),
            25,
        );
        assert_eq!(state.credit_ns, -5_000_000);
        state.update(
            now + Duration::from_millis(100_120),
            Duration::from_millis(32),
            25,
        );
        assert_eq!(state.credit_ns, 0);
    }

    #[test]
    fn capture_and_maintenance_cannot_run_heavy_jobs_concurrently() {
        let budget = CpuBudget::new(25).unwrap();
        budget.state.lock().unwrap().credit_ns = i128::MAX / 4;
        let permit = budget.try_acquire(WorkClass::Capture).unwrap().unwrap();
        assert!(
            budget
                .try_acquire(WorkClass::Maintenance)
                .unwrap()
                .is_none()
        );
        drop(permit);
        assert!(!budget.state.lock().unwrap().active);
    }

    #[test]
    fn nested_delivery_retains_the_outer_admission() {
        let budget = CpuBudget::new(25).unwrap();
        budget.state.lock().unwrap().credit_ns = i128::MAX / 4;
        let outer = budget.try_acquire(WorkClass::Capture).unwrap().unwrap();
        let nested = budget.acquire(WorkClass::Capture).unwrap();
        drop(nested);
        assert!(budget.state.lock().unwrap().active);
        assert!(
            budget
                .try_acquire(WorkClass::Maintenance)
                .unwrap()
                .is_none()
        );
        drop(outer);
        assert!(budget.state.lock().unwrap().owner.is_none());
    }

    #[test]
    fn cancelled_admission_does_not_wait_for_cpu_credit_or_nested_work() {
        let budget = CpuBudget::new(25).unwrap();
        budget.state.lock().unwrap().credit_ns = -1_000_000_000;
        let checks = std::cell::Cell::new(0);
        assert!(
            budget
                .acquire_while(WorkClass::Capture, || {
                    let count = checks.get();
                    checks.set(count + 1);
                    count < 2
                })
                .unwrap()
                .is_none()
        );
        budget.state.lock().unwrap().credit_ns = i128::MAX / 4;
        let _outer = budget.acquire(WorkClass::Capture).unwrap();
        assert!(
            budget
                .acquire_while(WorkClass::Capture, || false)
                .unwrap()
                .is_none()
        );
    }
}

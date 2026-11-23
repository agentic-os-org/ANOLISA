use super::job::{monotonic_ns, JobDeadline};
use nix::libc;
use std::io;
use std::process::{Child, ExitStatus};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

pub(crate) const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(1);
pub(crate) const COMMAND_CLEANUP_BUDGET: Duration = Duration::from_millis(25);
pub(crate) const HEALTH_COMMAND_CHILD_CAP: usize = 8;
static REAP_OWNER: OnceLock<ReapOwner> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CleanupScope {
    OwnedGroup,
    DirectChild,
}

struct ChildState {
    child: Option<Child>,
    scope: CleanupScope,
    signalled: bool,
    outcome: Option<io::Result<ExitStatus>>,
    error: Option<i32>,
}

pub(crate) struct ReapSlot {
    pub(crate) reserved: AtomicBool,
    ready: AtomicBool,
    lease_ended: AtomicBool,
    cancel: AtomicBool,
    deadline_ns: AtomicU64,
    state: Mutex<Option<ChildState>>,
}

pub(crate) struct ReapOwner {
    pub(crate) slots: [ReapSlot; HEALTH_COMMAND_CHILD_CAP],
    thread: OnceLock<io::Result<thread::JoinHandle<()>>>,
}

pub(crate) struct ReapPermit<'a> {
    owner: &'a ReapOwner,
    pub(crate) index: usize,
    registered: bool,
}

pub(crate) struct CommandChild<'a> {
    permit: ReapPermit<'a>,
    pub(crate) deadline: Instant,
    released: bool,
}

fn try_state(slot: &ReapSlot) -> Option<MutexGuard<'_, Option<ChildState>>> {
    match slot.state.try_lock() {
        Ok(state) => Some(state),
        Err(TryLockError::Poisoned(error)) => Some(error.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}

impl ReapOwner {
    pub(crate) fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| ReapSlot {
                reserved: AtomicBool::new(false),
                ready: AtomicBool::new(false),
                lease_ended: AtomicBool::new(false),
                cancel: AtomicBool::new(false),
                deadline_ns: AtomicU64::new(u64::MAX),
                state: Mutex::new(None),
            }),
            thread: OnceLock::new(),
        }
    }

    pub(crate) fn global() -> io::Result<&'static Self> {
        let owner = REAP_OWNER.get_or_init(Self::new);
        let thread = owner.thread.get_or_init(|| {
            thread::Builder::new()
                .name("cosh-child-owner".into())
                .spawn(move || loop {
                    if owner.reap_pending() {
                        thread::park_timeout(COMMAND_POLL_INTERVAL);
                    } else {
                        thread::park();
                    }
                })
        });
        if thread.as_ref().map_or(true, |handle| handle.is_finished()) {
            return Err(io::Error::other("process child owner is unavailable"));
        }
        Ok(owner)
    }

    pub(crate) fn reserve(&self) -> Option<ReapPermit<'_>> {
        self.slots
            .iter()
            .position(|slot| {
                slot.reserved
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                    .is_ok()
            })
            .map(|index| {
                let slot = &self.slots[index];
                slot.ready.store(false, Ordering::Relaxed);
                slot.lease_ended.store(false, Ordering::Relaxed);
                slot.cancel.store(false, Ordering::Relaxed);
                ReapPermit {
                    owner: self,
                    index,
                    registered: false,
                }
            })
    }

    fn wake(&self) {
        if let Some(Ok(handle)) = self.thread.get() {
            handle.thread().unpark();
        }
    }

    pub(crate) fn reap_pending(&self) -> bool {
        let mut active = false;
        for slot in &self.slots {
            if !slot.ready.load(Ordering::Acquire) {
                continue;
            }
            let Some(mut guard) = try_state(slot) else {
                active = true;
                continue;
            };
            if let Some(state) = guard.as_mut() {
                maintain(state, slot, true, Instant::now() + COMMAND_CLEANUP_BUDGET);
                active |= state.child.is_some();
                if state.child.is_none() && slot.lease_ended.load(Ordering::Acquire) {
                    *guard = None;
                    slot.ready.store(false, Ordering::Release);
                    drop(guard);
                    slot.reserved.store(false, Ordering::Release);
                }
            }
        }
        active
    }

    #[cfg(test)]
    pub(crate) fn slot_has_child(&self, index: usize) -> bool {
        self.slots[index]
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .is_some_and(|state| state.child.is_some())
    }
}

impl<'a> ReapPermit<'a> {
    pub(crate) fn register(
        mut self,
        child: Child,
        scope: CleanupScope,
        deadline: Instant,
    ) -> CommandChild<'a> {
        let slot = &self.owner.slots[self.index];
        let expires =
            JobDeadline::at_instant(deadline, Duration::MAX).map(|deadline| deadline.expires_ns);
        slot.deadline_ns
            .store(expires.unwrap_or(0), Ordering::Release);
        *slot.state.lock().unwrap_or_else(|error| error.into_inner()) = Some(ChildState {
            child: Some(child),
            scope,
            signalled: false,
            outcome: None,
            error: None,
        });
        self.registered = true;
        slot.ready.store(true, Ordering::Release);
        self.owner.wake();
        CommandChild {
            permit: self,
            deadline,
            released: false,
        }
    }
}

impl Drop for ReapPermit<'_> {
    fn drop(&mut self) {
        let slot = &self.owner.slots[self.index];
        if self.registered {
            slot.cancel.store(true, Ordering::Release);
            slot.lease_ended.store(true, Ordering::Release);
            self.owner.wake();
        } else {
            slot.reserved.store(false, Ordering::Release);
        }
    }
}

impl CommandChild<'_> {
    pub(crate) fn shorten_deadline(&mut self, deadline: Instant) {
        self.deadline = self.deadline.min(deadline);
        let expires = JobDeadline::at_instant(self.deadline, Duration::MAX)
            .map(|deadline| deadline.expires_ns)
            .unwrap_or(0);
        self.permit.owner.slots[self.permit.index]
            .deadline_ns
            .fetch_min(expires, Ordering::AcqRel);
        self.permit.owner.wake();
    }

    pub(crate) fn has_exited(&self) -> io::Result<bool> {
        let slot = &self.permit.owner.slots[self.permit.index];
        let Some(state) = try_state(slot) else {
            return Ok(false);
        };
        match state.as_ref().and_then(|state| state.child.as_ref()) {
            Some(child) => child_has_exited(child.id()),
            None => Ok(true),
        }
    }

    #[cfg(test)]
    pub(crate) fn is_released(&self) -> bool {
        self.released
    }

    pub(crate) fn cleanup(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.released {
            return Ok(None);
        }
        self.released = true;
        self.shorten_deadline(self.deadline);
        let slot = &self.permit.owner.slots[self.permit.index];
        slot.cancel.store(true, Ordering::Release);
        self.permit.owner.wake();
        let limit = self.deadline.min(Instant::now() + COMMAND_CLEANUP_BUDGET);
        let mut last_error = None;
        loop {
            if let Some(mut guard) = try_state(slot) {
                if let Some(state) = guard.as_mut() {
                    maintain(state, slot, Instant::now() < limit, limit);
                    if let Some(outcome) = state.outcome.take() {
                        return outcome.map(Some);
                    }
                    last_error = state.error;
                }
            }
            if Instant::now() >= limit {
                return last_error
                    .map_or(Ok(None), |error| Err(io::Error::from_raw_os_error(error)));
            }
            thread::sleep(
                COMMAND_POLL_INTERVAL.min(limit.saturating_duration_since(Instant::now())),
            );
        }
    }
}

impl Drop for CommandChild<'_> {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn maintain(state: &mut ChildState, slot: &ReapSlot, reap: bool, query_deadline: Instant) {
    let Some(child) = state.child.as_mut() else {
        return;
    };
    let exited = match child_has_exited(child.id()) {
        Ok(exited) => exited,
        Err(error) if error.raw_os_error() == Some(libc::ECHILD) => {
            state.child.take();
            state.outcome = Some(Err(error));
            return;
        }
        Err(error) => {
            state.error = Some(error.raw_os_error().unwrap_or(libc::EIO));
            false
        }
    };
    let expired =
        monotonic_ns().map_or(true, |now| now >= slot.deadline_ns.load(Ordering::Acquire));
    if !state.signalled && (exited || expired || slot.cancel.load(Ordering::Acquire)) {
        let result = match state.scope {
            CleanupScope::OwnedGroup => signal_command_group(child, query_deadline),
            CleanupScope::DirectChild if exited => Ok(()),
            CleanupScope::DirectChild => child.kill(),
        };
        if let Err(error) = result {
            state.error = Some(error.raw_os_error().unwrap_or(libc::EIO));
            return;
        }
        state.signalled = true;
        state.error = None;
    }
    if state.signalled && reap {
        match child.try_wait() {
            Ok(Some(status)) => {
                state.child.take();
                state.outcome = Some(Ok(status));
                state.error = None;
            }
            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => {
                state.child.take();
                state.outcome = Some(Err(error));
            }
            Err(error) if error.kind() != io::ErrorKind::Interrupted => {
                state.error = Some(error.raw_os_error().unwrap_or(libc::EIO));
            }
            _ => {}
        }
    }
}

pub(crate) fn ensure_waitable_children() -> io::Result<()> {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut action) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // Cosh must retain targeted wait ownership and never install a wildcard child reaper.
    if action.sa_sigaction == libc::SIG_IGN || action.sa_flags & libc::SA_NOCLDWAIT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process jobs require waitable children",
        ));
    }
    Ok(())
}

pub(crate) fn child_has_exited(pid: u32) -> io::Result<bool> {
    let mut information: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut information,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { information.si_pid() } != 0)
}

fn signal_command_group(child: &mut Child, _deadline: Instant) -> io::Result<()> {
    if unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    if error.raw_os_error() == Some(libc::EPERM)
        && Instant::now() < _deadline
        && child_has_exited(child.id())?
        && darwin_group_contains_only_leader(child.id())
    {
        return Ok(());
    }
    let _ = child.kill();
    Err(error)
}

#[cfg(target_os = "macos")]
pub(crate) fn darwin_group_contains_only_leader(pid: u32) -> bool {
    const PROC_PGRP_ONLY: u32 = 2;
    let mut members = [0i32; 2];
    // The unreaped zombie cannot fork; reject any second member or incomplete query.
    let bytes = unsafe {
        libc::proc_listpids(
            PROC_PGRP_ONLY,
            pid,
            members.as_mut_ptr().cast(),
            std::mem::size_of_val(&members) as i32,
        )
    };
    bytes == std::mem::size_of::<i32>() as i32 && members[0] == pid as i32
}

pub(crate) fn set_nonblocking(fd: i32) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

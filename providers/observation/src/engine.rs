//! Per-thread hardware execution breakpoints, with clone-stop readiness and bounded detach.

use crate::{
    events::{observation, Sink},
    fault,
    identity::ProcessRef,
    ptrace,
    symbols::{self, Symbol},
};
use anyhow::{ensure, Context, Result};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::File,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::fs::FileExt,
    },
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub(crate) static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}

pub(crate) fn install_handlers() {
    // SAFETY: handlers only set an atomic flag; no process-wide timer is installed.
    unsafe {
        libc::signal(libc::SIGINT, stop as *const () as usize);
        libc::signal(libc::SIGTERM, stop as *const () as usize);
    }
}

struct Thread {
    stopped: bool,
    armed: bool,
    initialized: bool,
    signal: i32,
}

pub(crate) struct Engine {
    identity: ProcessRef,
    _pidfd: OwnedFd,
    threads: BTreeMap<i32, Thread>,
    symbols: Vec<Symbol>,
    memory: Option<File>,
    hits: u64,
    pauses: Vec<u64>,
    pending: VecDeque<(i32, i32, Instant)>,
}

impl Engine {
    pub fn attach(identity: ProcessRef) -> Result<Self> {
        identity.check()?;
        // SAFETY: pidfd_open takes a checked PID and zero flags; ownership is transferred below.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, identity.pid, 0) } as i32;
        ensure!(
            fd >= 0,
            "pidfd_unavailable: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: fd is a newly opened, uniquely owned descriptor.
        let pidfd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut engine = Self {
            identity,
            _pidfd: pidfd,
            threads: BTreeMap::new(),
            symbols: vec![],
            memory: None,
            hits: 0,
            pauses: Vec::new(),
            pending: VecDeque::new(),
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            ensure!(Instant::now() < deadline, "attach_deadline");
            engine.identity.check()?;
            let tids: Vec<i32> = std::fs::read_dir(format!("/proc/{}/task", engine.identity.pid))?
                .map(|entry| {
                    Ok(entry?
                        .file_name()
                        .to_str()
                        .context("invalid_tid")?
                        .parse()?)
                })
                .collect::<Result<_>>()?;
            ensure!(tids.len() <= 256, "thread_limit");
            for tid in tids {
                if engine.threads.contains_key(&tid) {
                    continue;
                }
                let options = libc::PTRACE_O_TRACECLONE
                    | libc::PTRACE_O_TRACEEXEC
                    | libc::PTRACE_O_TRACEFORK
                    | libc::PTRACE_O_TRACEVFORK
                    | libc::PTRACE_O_TRACEEXIT;
                ptrace::call(libc::PTRACE_SEIZE, tid, 0, options as usize)?;
                engine.threads.insert(
                    tid,
                    Thread {
                        stopped: false,
                        armed: false,
                        initialized: false,
                        signal: 0,
                    },
                );
                ptrace::call(libc::PTRACE_INTERRUPT, tid, 0, 0)?;
            }
            while engine.threads.values().any(|t| !t.stopped) {
                let (tid, status) =
                    ptrace::wait(-1, Duration::from_millis(100))?.context("attach_stop_timeout")?;
                engine.capture(tid, status)?;
            }
            let current = std::fs::read_dir(format!("/proc/{}/task", engine.identity.pid))?
                .map(|entry| {
                    Ok(entry?
                        .file_name()
                        .to_str()
                        .context("invalid_tid")?
                        .parse::<i32>()?)
                })
                .collect::<Result<BTreeSet<_>>>()?;
            ensure!(!current.is_empty(), "target_has_no_threads");
            if current == engine.threads.keys().copied().collect() {
                break;
            }
        }
        engine.identity.check()?;
        fault::checkpoint("seized", engine.identity.pid, true)?;
        engine.symbols = symbols::resolve(engine.identity.pid)?;
        engine.memory = Some(File::open(format!("/proc/{}/mem", engine.identity.pid))?);
        for symbol in &engine.symbols {
            let mut first = [0];
            engine
                .memory
                .as_ref()
                .context("missing_memory")?
                .read_exact_at(&mut first, symbol.address)?;
            // Preserve an existing uProbe/debugger's patch. No instruction guessing or relocation.
            ensure!(first[0] != 0xcc, "unsupported_live_uprobe_or_int3");
        }
        let addresses: Vec<_> = engine.symbols.iter().map(|s| s.address).collect();
        for (&tid, thread) in &mut engine.threads {
            ptrace::check_available(tid, &addresses, false)?;
            // Own cleanup before the first write; arm can fail after partial installation.
            thread.armed = true;
            ptrace::arm(tid, &addresses)?;
            thread.initialized = true;
        }
        fault::checkpoint("armed", engine.identity.pid, true)?;
        Ok(engine)
    }

    fn capture(&mut self, tid: i32, status: i32) -> Result<()> {
        if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
            self.threads.remove(&tid);
            return Ok(());
        }
        ensure!(libc::WIFSTOPPED(status), "unexpected_wait_status");
        let signal = libc::WSTOPSIG(status);
        let event = status >> 16;
        let thread = self.threads.entry(tid).or_insert(Thread {
            stopped: true,
            armed: false,
            initialized: false,
            signal: 0,
        });
        thread.stopped = true;
        thread.signal = if event != 0 { 0 } else { signal };
        if matches!(
            event,
            libc::PTRACE_EVENT_CLONE | libc::PTRACE_EVENT_FORK | libc::PTRACE_EVENT_VFORK
        ) {
            let mut child: libc::c_ulong = 0;
            ptrace::call(
                libc::PTRACE_GETEVENTMSG,
                tid,
                0,
                &mut child as *mut _ as usize,
            )?;
            let inherited = self.threads[&tid].armed;
            let child = self.threads.entry(child as i32).or_insert(Thread {
                stopped: false,
                armed: false,
                initialized: false,
                signal: 0,
            });
            // A child can inherit debug state before its first stop is consumed.
            // It still needs address initialization on the normal clone path.
            child.armed |= inherited;
            ensure!(self.threads.len() <= 256, "thread_limit");
            ensure!(
                event == libc::PTRACE_EVENT_CLONE,
                "unsupported_fork_detaching"
            );
        }
        ensure!(
            event != libc::PTRACE_EVENT_EXEC,
            "exec_identity_changed_detaching"
        );
        Ok(())
    }

    fn check_identity(&self) -> Result<()> {
        let mut poll = libc::pollfd {
            fd: self._pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd is supplied with a nonblocking timeout.
        let result = unsafe { libc::poll(&mut poll, 1, 0) };
        ensure!(result == 0, "pidfd_process_exited");
        self.identity.check()
    }

    fn resume(&mut self, tid: i32, signal: i32) -> Result<()> {
        ptrace::call(libc::PTRACE_CONT, tid, 0, signal as usize)?;
        if let Some(t) = self.threads.get_mut(&tid) {
            t.stopped = false;
            t.signal = 0;
        }
        Ok(())
    }

    pub fn run(&mut self, sink: &mut Sink, seconds: u64) -> Result<()> {
        sink.control(json!({"kind":"source_status","state":"observing","mode":"advise","breakpoints":"x86_hardware",
            "safety":"unsafe_disposable_lab","crash_isolation":"unproven",
            "process":self.identity,"symbols":self.symbols.iter().map(|s| &s.name).collect::<Vec<_>>(),
            "coverage":{"status":"unknown","reason":"transport_write_only"}}))?;
        for tid in self.threads.keys().copied().collect::<Vec<_>>() {
            self.resume(tid, self.threads[&tid].signal)?;
        }
        let started = Instant::now();
        while !self.threads.is_empty()
            && started.elapsed().as_secs() < seconds
            && !STOP.load(Ordering::Relaxed)
        {
            if self.pending.is_empty() {
                let Some((tid, status)) = ptrace::wait(-1, Duration::from_millis(10))? else {
                    self.check_identity()?;
                    continue;
                };
                self.pending.push_back((tid, status, Instant::now()));
                // Drain already stopped threads before resuming any. Repeated waitpid(-1)
                // followed by immediate resume otherwise starves higher TIDs under load.
                while self.pending.len() < 256 {
                    match ptrace::wait(-1, Duration::ZERO) {
                        Ok(Some((tid, status))) => {
                            self.pending.push_back((tid, status, Instant::now()))
                        }
                        Ok(None) => break,
                        Err(error) if self.pending.is_empty() => return Err(error),
                        Err(_) => break,
                    }
                }
            }
            let (tid, status, t0) = self.pending.pop_front().context("missing_wait_status")?;
            self.capture(tid, status)?;
            if !self.threads.contains_key(&tid) {
                continue;
            }
            let event = status >> 16;
            if event == libc::PTRACE_EVENT_EXIT {
                self.resume(tid, 0)?;
                continue;
            }
            self.check_identity()?;
            if !self.threads[&tid].initialized {
                let status = std::fs::read_to_string(format!("/proc/{tid}/status"))?;
                let tgid = status
                    .lines()
                    .find_map(|l| l.strip_prefix("Tgid:"))
                    .context("missing_tgid")?
                    .trim()
                    .parse::<i32>()?;
                ensure!(tgid == self.identity.pid, "unsupported_non_thread_clone");
                // Linux clone threads can inherit exactly the parent's hardware state.
                let addresses = self.symbols.iter().map(|s| s.address).collect::<Vec<_>>();
                ptrace::check_available(tid, &addresses, true)?;
                self.threads.get_mut(&tid).context("missing_thread")?.armed = true;
                fault::checkpoint("clone_stop", tid, true)?;
                ptrace::arm(tid, &addresses)?;
                self.threads
                    .get_mut(&tid)
                    .context("missing_thread")?
                    .initialized = true;
            }
            if event == libc::PTRACE_EVENT_STOP
                && matches!(
                    libc::WSTOPSIG(status),
                    libc::SIGSTOP | libc::SIGTSTP | libc::SIGTTIN | libc::SIGTTOU
                )
            {
                ptrace::call(libc::PTRACE_LISTEN, tid, 0, 0)?;
                self.threads
                    .get_mut(&tid)
                    .context("missing_thread")?
                    .stopped = false;
                continue;
            }
            if event == 0 && libc::WSTOPSIG(status) == libc::SIGTRAP && ptrace::hardware_trap(tid)?
            {
                let registers = ptrace::regs(tid)?;
                let dr6 = ptrace::debug_read(tid, 6)?;
                if let Some(symbol) = self.symbols.iter().enumerate().find_map(|(i, s)| {
                    (registers.rip == s.address && dr6 & (1 << i) != 0).then_some(s)
                }) {
                    fault::checkpoint("trap_stop", tid, true)?;
                    ptrace::resume_breakpoint(tid, registers)?;
                    fault::checkpoint("after_rf", tid, true)?;
                    self.threads.get_mut(&tid).context("missing_thread")?.signal = 0;
                    let length = if symbol.name == "SSL_write" {
                        registers.rdx as u32 as i32 as i64 as u64
                    } else {
                        registers.rdx
                    };
                    let mut buffer = vec![0; length.min(4096) as usize];
                    let read_ok = length <= 4096
                        && self
                            .memory
                            .as_ref()
                            .context("missing_memory")?
                            .read_exact_at(&mut buffer, registers.rsi)
                            .is_ok();
                    let value = observation(
                        &self.identity,
                        tid,
                        &symbol.name,
                        length,
                        read_ok.then_some(buffer.as_slice()),
                    );
                    self.hits += 1;
                    ptrace::debug_write(tid, 6, 0)?;
                    fault::checkpoint("before_continue", tid, true)?;
                    self.resume(tid, 0)?;
                    fault::checkpoint("after_continue", tid, false)?;
                    if self.pauses.len() < 65536 {
                        self.pauses.push(t0.elapsed().as_micros() as u64);
                    }
                    sink.emit(value)?;
                    continue;
                }
            }
            // Preserve unrelated signal delivery; SEIZE group stops use LISTEN above.
            self.resume(tid, self.threads[&tid].signal)?;
        }
        self.detach()?;
        self.pauses.sort_unstable();
        let percentile = |p: usize| {
            self.pauses
                .get(self.pauses.len().saturating_sub(1) * p / 100)
                .copied()
                .unwrap_or(0)
        };
        sink.control(json!({"kind":"source_summary","hits":self.hits,"emitted":sink.emitted,"dropped":sink.dropped,
            "elapsed_ms":started.elapsed().as_millis(),"pause_us":{"p50":percentile(50),"p95":percentile(95),"p99":percentile(99),"max":self.pauses.last().copied().unwrap_or(0)},
            "pause_measurement":"wait_received_to_continue; excludes kernel-to-tracer scheduling",
            "pause_samples":self.pauses.len(),"detach":"complete","coverage":{"status":"unknown","reason":if sink.dropped > 0 {"event_limit_or_rate_drop"} else {"transport_write_only"}}}))
    }

    fn still_owned(tid: i32) -> Result<bool> {
        match std::fs::read_to_string(format!("/proc/{tid}/status")) {
            Ok(status) => Ok(status
                .lines()
                .any(|line| line == format!("TracerPid:\t{}", std::process::id()))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    fn detach_thread(&mut self, tid: i32) -> Result<bool> {
        let thread = &self.threads[&tid];
        let mut signal = thread.signal;
        fault::cleanup_snapshot(tid, signal);
        if thread.armed {
            if signal == libc::SIGTRAP && ptrace::hardware_trap(tid)? {
                let rip = ptrace::regs(tid)?.rip;
                if self.symbols.iter().any(|symbol| rip == symbol.address) {
                    signal = 0;
                }
            }
            ptrace::clear(tid)?;
            // INTERRUPT can win over delivery of a queued hardware SIGTRAP. Clearing
            // DR7 cannot remove that signal: consume its delivery stop before detach.
            if ptrace::pending_hardware_trap(tid)? {
                self.resume(tid, signal)?;
                return Ok(false);
            }
        }
        ptrace::call(libc::PTRACE_DETACH, tid, 0, signal as usize)?;
        Ok(true)
    }

    fn detach(&mut self) -> Result<()> {
        // Every consumed stop remains owned even when an error interrupts queue processing.
        for (tid, status, _) in std::mem::take(&mut self.pending) {
            let _ = self.capture(tid, status);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut last_error = None;
        for (&tid, thread) in &self.threads {
            if !thread.stopped {
                let _ = ptrace::call(libc::PTRACE_INTERRUPT, tid, 0, 0);
            }
        }
        while !self.threads.is_empty() && Instant::now() < deadline {
            for tid in self.threads.keys().copied().collect::<Vec<_>>() {
                let thread = &self.threads[&tid];
                if !thread.stopped {
                    continue;
                }
                let result = self.detach_thread(tid);
                match result {
                    Ok(true) => {
                        self.threads.remove(&tid);
                    }
                    Ok(false) => (),
                    Err(error) => {
                        // Exiting threads can reject register operations before wait reports death.
                        // Keep draining every other thread; one ESRCH must not abort global cleanup.
                        if matches!(Self::still_owned(tid), Ok(false)) {
                            self.threads.remove(&tid);
                        } else if error.chain().any(|e| {
                            e.downcast_ref::<std::io::Error>()
                                .is_some_and(|e| e.raw_os_error() == Some(libc::ESRCH))
                        }) {
                            self.threads
                                .get_mut(&tid)
                                .context("missing_thread")?
                                .stopped = false;
                            let _ = ptrace::call(libc::PTRACE_INTERRUPT, tid, 0, 0);
                        }
                        last_error = Some(error);
                    }
                }
            }
            if self.threads.is_empty() {
                break;
            }
            match ptrace::wait(-1, Duration::from_millis(10)) {
                Ok(Some((tid, status))) => {
                    let _ = self.capture(tid, status);
                }
                Ok(None) => (),
                Err(error) => {
                    for tid in self.threads.keys().copied().collect::<Vec<_>>() {
                        if matches!(Self::still_owned(tid), Ok(false)) {
                            self.threads.remove(&tid);
                        }
                    }
                    last_error = Some(error.context("detach_wait_failed"));
                    if !self.threads.is_empty() {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            }
        }
        if !self.threads.is_empty() {
            return Err(last_error.unwrap_or_else(|| anyhow::anyhow!("threads_still_owned")))
                .context("detach_deadline_incomplete");
        }
        Ok(())
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Err(error) = self.detach() {
            eprintln!("cleanup_incomplete: {error:#}");
        }
    }
}

//! Bounded deterministic fault rendezvous for explicitly compiled disposable experiments.

use anyhow::Result;

pub(crate) fn checkpoint(phase: &str, tid: i32, stopped: bool) -> Result<()> {
    #[cfg(feature = "unsafe-lab")]
    {
        use anyhow::ensure;
        use std::{
            io::{Read, Write},
            sync::atomic::{AtomicBool, Ordering},
        };
        static USED: AtomicBool = AtomicBool::new(false);
        if std::env::var("AW_PTRACE_TEST_PHASE").ok().as_deref() != Some(phase)
            || USED.swap(true, Ordering::Relaxed)
        {
            return Ok(());
        }
        let registers = if stopped {
            Some(crate::ptrace::regs(tid)?)
        } else {
            None
        };
        let dr6 = if stopped {
            Some(crate::ptrace::debug_read(tid, 6)?)
        } else {
            None
        };
        let dr7 = if stopped {
            Some(crate::ptrace::debug_read(tid, 7)?)
        } else {
            None
        };
        println!(
            "{}",
            serde_json::json!({"kind":"lab_checkpoint", "phase":phase, "tid":tid,
            "stopped":stopped, "rip":registers.as_ref().map(|r| r.rip),
            "eflags":registers.as_ref().map(|r| r.eflags), "dr6":dr6, "dr7":dr7})
        );
        std::io::stdout().flush()?;
        let mut input = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized descriptor is polled with a bounded timeout.
        let ready = unsafe { libc::poll(&mut input, 1, 5000) };
        ensure!(
            ready == 1 && input.revents & libc::POLLIN != 0,
            "lab_checkpoint_timeout"
        );
        let mut command = [0];
        std::io::stdin().read_exact(&mut command)?;
        ensure!(command == *b"c", "lab_injected_failure: {phase}");
    }
    #[cfg(not(feature = "unsafe-lab"))]
    let _ = (phase, tid, stopped);
    Ok(())
}

pub(crate) fn cleanup_snapshot(tid: i32, signal: i32) {
    #[cfg(feature = "unsafe-lab")]
    if std::env::var("AW_PTRACE_TEST_TRACE_CLEANUP").as_deref() == Ok("1") {
        let regs = crate::ptrace::regs(tid).ok();
        eprintln!(
            "{}",
            serde_json::json!({"kind":"lab_cleanup", "tid":tid, "signal":signal,
            "rip":regs.map(|r| r.rip), "dr6":crate::ptrace::debug_read(tid, 6).ok(),
            "hardware_trap":crate::ptrace::hardware_trap(tid).ok(),
            "pending_hardware_trap":crate::ptrace::pending_hardware_trap(tid).ok()})
        );
    }
    #[cfg(not(feature = "unsafe-lab"))]
    let _ = (tid, signal);
}

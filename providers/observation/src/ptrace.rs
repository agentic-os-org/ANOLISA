//! Linux x86-64 ptrace primitives derived from agentsight-rewrite (see SOURCE.md).

use anyhow::{Context, Result};
use std::{
    io, mem,
    time::{Duration, Instant},
};

pub(crate) fn call(
    request: libc::c_uint,
    tid: i32,
    addr: usize,
    data: usize,
) -> Result<libc::c_long> {
    // SAFETY: request-specific callers provide valid pointers and Linux ABI arguments.
    let result = unsafe {
        libc::ptrace(
            request,
            tid,
            addr as *mut libc::c_void,
            data as *mut libc::c_void,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error())
            .context(format!("ptrace request {request} tid {tid}"));
    }
    Ok(result)
}

pub(crate) fn regs(tid: i32) -> Result<libc::user_regs_struct> {
    // SAFETY: this integer-only kernel ABI structure accepts zero initialization.
    let mut registers = unsafe { mem::zeroed::<libc::user_regs_struct>() };
    call(
        libc::PTRACE_GETREGS,
        tid,
        0,
        &mut registers as *mut _ as usize,
    )?;
    Ok(registers)
}

pub(crate) fn resume_breakpoint(tid: i32, mut registers: libc::user_regs_struct) -> Result<()> {
    // x86 RF suppresses the execution breakpoint for exactly this instruction.
    // No shared text patch or single-step/disarm window exists between threads.
    registers.eflags |= 1 << 16;
    call(
        libc::PTRACE_SETREGS,
        tid,
        0,
        &registers as *const _ as usize,
    )?;
    Ok(())
}

fn debug_offset(index: usize) -> usize {
    mem::offset_of!(libc::user, u_debugreg) + index * mem::size_of::<u64>()
}

pub(crate) fn debug_read(tid: i32, index: usize) -> Result<u64> {
    Ok(call(libc::PTRACE_PEEKUSER, tid, debug_offset(index), 0)? as u64)
}

pub(crate) fn debug_write(tid: i32, index: usize, value: u64) -> Result<()> {
    call(
        libc::PTRACE_POKEUSER,
        tid,
        debug_offset(index),
        value as usize,
    )?;
    Ok(())
}

pub(crate) fn check_available(tid: i32, addresses: &[u64], inherited: bool) -> Result<()> {
    let enabled = (0..addresses.len()).fold(0, |mask, i| mask | 1 << (i * 2));
    let active = debug_read(tid, 7)? & 0xff;
    if active != 0 {
        anyhow::ensure!(
            inherited && active == enabled,
            "hardware_breakpoint_conflict"
        );
        for (index, address) in addresses.iter().enumerate() {
            anyhow::ensure!(
                matches!(debug_read(tid, index)?, 0) || debug_read(tid, index)? == *address,
                "inherited_breakpoint_mismatch"
            );
        }
    }
    Ok(())
}

pub(crate) fn arm(tid: i32, addresses: &[u64]) -> Result<()> {
    for (index, address) in addresses.iter().enumerate() {
        debug_write(tid, index, *address)?;
        if index == 0 {
            crate::fault::checkpoint("partial_arm", tid, true)?;
        }
    }
    debug_write(tid, 6, 0)?;
    let enabled = (0..addresses.len()).fold(0, |mask, i| mask | 1 << (i * 2));
    debug_write(tid, 7, enabled)?;
    crate::fault::checkpoint("enabled_arm", tid, true)
}

pub(crate) fn clear(tid: i32) -> Result<()> {
    debug_write(tid, 7, 0)?;
    for index in [0, 1, 6] {
        debug_write(tid, index, 0)?;
    }
    Ok(())
}

pub(crate) fn wait(tid: i32, timeout: Duration) -> Result<Option<(i32, i32)>> {
    let deadline = Instant::now() + timeout;
    loop {
        let mut status = 0;
        // SAFETY: status points to a live integer; __WALL includes traced threads.
        let found = unsafe { libc::waitpid(tid, &mut status, libc::__WALL | libc::WNOHANG) };
        if found > 0 {
            return Ok(Some((found, status)));
        }
        if found < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(e.into());
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_micros(100));
    }
}

/// Require the kernel hardware-breakpoint signal provenance before suppressing a trap.
pub(crate) fn hardware_trap(tid: i32) -> Result<bool> {
    // SAFETY: siginfo_t accepts zero initialization and the kernel fills this live value.
    let mut info = unsafe { mem::zeroed::<libc::siginfo_t>() };
    call(
        libc::PTRACE_GETSIGINFO,
        tid,
        0,
        &mut info as *mut _ as usize,
    )?;
    Ok(info.si_signo == libc::SIGTRAP && info.si_code == libc::TRAP_HWBKPT)
}

pub(crate) fn pending_hardware_trap(tid: i32) -> Result<bool> {
    #[repr(C)]
    struct PeekArgs {
        offset: u64,
        flags: u32,
        count: i32,
    }
    let args = PeekArgs {
        offset: 0,
        flags: 0,
        count: 64,
    };
    // SAFETY: siginfo_t is a kernel ABI value accepting zero initialization.
    let mut pending = unsafe { mem::zeroed::<[libc::siginfo_t; 64]>() };
    let count = call(
        libc::PTRACE_PEEKSIGINFO,
        tid,
        &args as *const _ as usize,
        pending.as_mut_ptr() as usize,
    )? as usize;
    anyhow::ensure!(count < pending.len(), "pending_signal_limit");
    Ok(pending[..count]
        .iter()
        .any(|info| info.si_signo == libc::SIGTRAP && info.si_code == libc::TRAP_HWBKPT))
}

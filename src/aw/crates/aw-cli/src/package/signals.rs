//! Cooperatively cancel package mutations while preserving owned-file rollback.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// Cancellation shared by confirmation and package transaction handlers.
pub(crate) static CANCEL: AtomicBool = AtomicBool::new(false);
static SIGNAL: AtomicI32 = AtomicI32::new(0);

extern "C" fn interrupt(signal: libc::c_int) {
    let _ = SIGNAL.compare_exchange(0, signal, Ordering::Relaxed, Ordering::Relaxed);
    CANCEL.store(true, Ordering::Relaxed);
}

pub(crate) fn interrupted() -> Option<i32> {
    match SIGNAL.load(Ordering::Relaxed) {
        0 => None,
        signal => Some(signal),
    }
}

pub(crate) fn register() -> std::io::Result<()> {
    // SAFETY: handlers only update a lock-free atomic; cleanup runs on the main thread.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = interrupt as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        for signal in [libc::SIGINT, libc::SIGTERM] {
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

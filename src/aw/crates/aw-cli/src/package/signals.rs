//! Cooperatively cancel package mutations while preserving owned-file rollback.

use std::sync::atomic::{AtomicBool, Ordering};

pub(super) static CANCEL: AtomicBool = AtomicBool::new(false);

extern "C" fn interrupt(_: libc::c_int) {
    CANCEL.store(true, Ordering::Relaxed);
}

pub(super) fn register() -> std::io::Result<()> {
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

//! Platform-specific RTK process ownership and deadline-bound output collection.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub(super) use unix::run;
#[cfg(windows)]
pub(super) use windows::run;

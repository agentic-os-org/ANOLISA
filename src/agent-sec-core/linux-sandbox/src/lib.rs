//! Linux sandbox helper entry point.
//!
//! On Linux, `linux-sandbox` applies:
//! - in-process restrictions (`no_new_privs` + seccomp), and
//! - bubblewrap for filesystem isolation.

pub mod error;
pub mod path;
pub mod policy;

#[cfg(target_os = "linux")]
mod bwrap_args;
#[cfg(target_os = "linux")]
mod cli;
#[cfg(target_os = "linux")]
mod proxy;
#[cfg(target_os = "linux")]
mod seccomp;

#[cfg(target_os = "linux")]
pub fn run_main() -> ! {
    cli::run_main();
}

#[cfg(not(target_os = "linux"))]
pub fn run_main() -> ! {
    panic!("linux-sandbox is only supported on Linux");
}

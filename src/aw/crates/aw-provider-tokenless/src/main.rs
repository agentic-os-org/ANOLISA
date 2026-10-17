//! One-request tool lifecycle bridge; Tokenless owns optimization policy and state.

use std::{
    path::PathBuf,
    process::ExitCode,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

static CANCELLED: AtomicBool = AtomicBool::new(false);
extern "C" fn cancel(_: libc::c_int) {
    CANCELLED.store(true, Ordering::Release);
}

fn main() -> ExitCode {
    match execute() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn execute() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
    const USAGE: &str =
        "usage: aw-provider-tokenless [--view] --cli ABSOLUTE_PATH --data-dir ABSOLUTE_DIR [--rtk ABSOLUTE_PATH --rtk-sha256 HEX]";
    if args.len() == 1 && args[0] == "--help" {
        println!("{USAGE}");
        return Ok(());
    }
    let view = args.first().is_some_and(|arg| arg == "--view");
    if view {
        args.remove(0);
    }
    if !matches!(args.len(), 4 | 8) || args[0] != "--cli" || args[2] != "--data-dir" {
        return Err(USAGE.into());
    }
    let cli = PathBuf::from(&args[1]);
    let data = PathBuf::from(&args[3]);
    if !cli.is_absolute() || !data.is_absolute() {
        return Err("--cli and --data-dir must be absolute paths".into());
    }
    let rtk = if args.len() == 8 {
        if view || args[4] != "--rtk" || args[6] != "--rtk-sha256" {
            return Err(USAGE.into());
        }
        let path = PathBuf::from(&args[5]);
        let checksum = args[7].to_str().ok_or(USAGE)?.to_owned();
        if !path.is_absolute()
            || path.file_name().is_none_or(|name| name != "rtk")
            || checksum.len() != 64
            || !checksum
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err("managed RTK requires absolute rtk path and lowercase SHA-256".into());
        }
        Some((path, checksum))
    } else {
        None
    };
    if view {
        // SAFETY: the handler only stores an atomic flag; no child reaping or
        // allocation occurs in signal context. The bounded executor owns cleanup.
        unsafe {
            let mut action = std::mem::zeroed::<libc::sigaction>();
            action.sa_sigaction = cancel as *const () as usize;
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut()) != 0
                || libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut()) != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        aw_provider_tokenless::view::run(
            &mut std::io::stdin().lock(),
            &mut std::io::stdout().lock(),
            &cli,
            &data,
            &CANCELLED,
        )?;
        return Ok(());
    }
    aw_provider_tokenless::run_with_rtk(
        &mut std::io::stdin().lock(),
        &mut std::io::stdout().lock(),
        &cli,
        &data,
        Duration::from_secs(60),
        rtk.as_ref()
            .map(|(path, checksum)| (path.as_path(), checksum.as_str())),
    )?;
    Ok(())
}

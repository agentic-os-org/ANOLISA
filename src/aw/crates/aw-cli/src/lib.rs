//! Unified AW command presentation and orchestration over reusable framework libraries.

mod cli;
mod command;
mod output;
mod package;

use aw_service::{Operation, Server};
use std::{
    io::{Read, Write},
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Release);
}

/// Run the `aw` frontend and preserve native Agent exit codes and signals.
pub fn main() {
    match run() {
        Ok(exit) => cli::finish(exit),
        Err(error) => {
            // A closed diagnostic stream must not replace exit 1 with a panic.
            let _ = writeln!(std::io::stderr().lock(), "aw: {error}");
            std::process::exit(1);
        }
    }
}

fn run() -> cli::Result<cli::Exit> {
    let arguments = package::arguments::decode_all(std::env::args_os().skip(1))?;
    command::run(arguments)
}

fn runtime(command: &str, args: &cli::Arguments) -> cli::Result<cli::Exit> {
    if command == "policy" || command == "inspect" {
        install_stop_signals()?;
    }
    if command == "inspect" {
        args.check(&["--config", "--json"], false)?;
        let config = PathBuf::from(args.required("--config")?);
        if !config.is_absolute() || !args.flags.contains_key("--json") {
            return Err("inspect requires --config ABS_FILE --json".into());
        }
        let bytes = cli::read_file(config, aw_config::MAX_DOCUMENT_BYTES)?;
        let context = aw_host::ProcessContext {
            cwd: std::env::current_dir()?,
            environment: std::env::vars_os().collect(),
            stderr_bytes: 65536,
        };
        let snapshot = aw_service::inspect::snapshot(
            &bytes,
            &context,
            Instant::now() + Duration::from_secs(3),
            &STOP,
        )?;
        println!("{}", serde_json::to_string(&snapshot)?);
        return Ok(cli::Exit::Code(0));
    }
    if matches!(
        command,
        "run" | "hook" | "install" | "policy" | "update" | "bridge"
    ) {
        return cli::dispatch(command, args);
    }
    let flags = &args.flags;
    let allowed: &[&str] = match command {
        "validate" => &["--config", "--against"],
        "serve" => &["--config", "--state-dir"],
        "status" | "stop" => &["--socket", "--config"],
        "request" => &["--socket", "--timeout-ms"],
        _ => return Err("unknown command".into()),
    };
    args.check(allowed, false)?;
    let required = |name: &str| args.required(name);
    match command {
        "validate" if flags.contains_key("--against") => {
            let validator = aw_config::Validator::new()?;
            let load = |path: &str| -> cli::Result<_> {
                let bytes = cli::read_file(path, aw_config::MAX_DOCUMENT_BYTES)?;
                Ok(validator.parse(&bytes)?)
            };
            let candidate =
                load(required("--config")?).map_err(|error| format!("candidate: {error}"))?;
            let baseline =
                load(required("--against")?).map_err(|error| format!("baseline: {error}"))?;
            baseline.validate_policy_update(&candidate)?;
            println!("policy update scope valid; runtime activation is not checked");
        }
        "validate" | "serve" => {
            let mut bytes = Vec::new();
            std::fs::File::open(required("--config")?)?
                .take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            if command == "validate" {
                aw_config::Validator::new()?.parse(&bytes)?;
                println!("configuration valid");
            } else {
                let server = Server::bind(bytes, PathBuf::from(required("--state-dir")?))?;
                install_stop_signals()?;
                eprintln!("AW service listening on {}", server.socket_path().display());
                server.run(&STOP)?;
            }
        }
        _ => {
            let budget: u64 = flags
                .get("--timeout-ms")
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(5000);
            if !(1..=60000).contains(&budget) {
                return Err("timeout must be 1..60000 ms".into());
            }
            let operation = match command {
                "status" => Operation::Status,
                "stop" => Operation::Stop,
                _ => {
                    let mut input = Vec::new();
                    std::io::stdin()
                        .take(8 * 1024 * 1024 + 1)
                        .read_to_end(&mut input)?;
                    if input.len() > 8 * 1024 * 1024 {
                        return Err("operation exceeds 8 MiB".into());
                    }
                    Operation::from_json(&input)?
                }
            };
            let expires = Instant::now() + Duration::from_millis(budget);
            let client = cli::connect_service(args, expires)?;
            let result = client.call(operation, expires)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"identity": client.identity(), "result": result})
                )?
            );
        }
    }
    Ok(cli::Exit::Code(0))
}

fn install_stop_signals() -> cli::Result<()> {
    // SAFETY: handlers only set a lock-free atomic; no child reaping is installed.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = stop as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        if libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut()) != 0
            || libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut()) != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

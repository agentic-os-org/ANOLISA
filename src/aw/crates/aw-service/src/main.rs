//! Standalone AW service, native Agent launcher and explicit local protocol.

mod cli;

use aw_service::{Client, Operation, Server};
use std::{
    io::Read,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Release);
}

fn main() {
    match run() {
        Ok(exit) => exit.finish(),
        Err(error) => {
            eprintln!("aw: {error}");
            std::process::exit(1);
        }
    }
}

fn run() -> cli::Result<cli::Exit> {
    let mut args = std::env::args().skip(1);
    let command = args
        .next()
        .ok_or("expected run, install, validate, serve, status, request or stop")?;
    if command == "--help" {
        println!("aw run --config FILE --agent TARGET [--native-settings JSON_FILE | --native-profile ABSOLUTE_DIR] -- [AGENT_ARGS]\naw install --config FILE --agent TARGET --native-profile ABSOLUTE_DIR\naw validate --config FILE\naw serve --config FILE --state-dir ABSOLUTE_DIR\naw status|stop (--config FILE | --socket ABSOLUTE_PATH)\naw request --socket ABSOLUTE_PATH [--timeout-ms 1..60000] < operation.json");
        return Ok(cli::Exit::Code(0));
    }
    let args = cli::Arguments::parse(args)?;
    if matches!(command.as_str(), "run" | "hook" | "install") {
        return cli::dispatch(&command, &args);
    }
    let flags = &args.flags;
    let allowed: &[&str] = match command.as_str() {
        "validate" => &["--config"],
        "serve" => &["--config", "--state-dir"],
        "status" | "stop" => &["--socket", "--config"],
        "request" => &["--socket", "--timeout-ms"],
        _ => return Err("unknown command".into()),
    };
    args.check(allowed, false)?;
    let required = |name: &str| args.required(name);
    match command.as_str() {
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
            let operation = match command.as_str() {
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
                    serde_json::from_slice(&input)?
                }
            };
            let expires = Instant::now() + Duration::from_millis(budget);
            let paths = if let Some(config) = flags.get("--config") {
                if flags.contains_key("--socket") {
                    return Err("choose --config or --socket".into());
                }
                let bytes = cli::read_file(config, 4 * 1024 * 1024)?;
                let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty());
                Some(aw_service::launch_service::resolve(
                    &bytes,
                    runtime.as_deref().map(std::path::Path::new),
                )?)
            } else {
                None
            };
            let socket = match &paths {
                Some(paths) => paths.socket.clone(),
                None => PathBuf::from(required("--socket")?),
            };
            let client = Client::connect(socket, expires)?;
            if paths
                .as_ref()
                .is_some_and(|paths| paths.config_revision != client.identity().config_revision)
            {
                return Err("service configuration does not match the supplied file".into());
            }
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

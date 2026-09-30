//! Developer CLI for the standalone service and its explicit local protocol.

use aw_service::{Client, Operation, Server};
use std::{
    collections::BTreeMap,
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
    if let Err(error) = run() {
        eprintln!("aw: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let command = args
        .next()
        .ok_or("expected validate, serve, status, request or stop")?;
    if command == "--help" {
        println!("aw validate --config FILE\naw serve --config FILE --state-dir ABSOLUTE_DIR\naw status|stop --socket ABSOLUTE_PATH\naw request --socket ABSOLUTE_PATH [--timeout-ms 1..60000] < operation.json");
        return Ok(());
    }
    let mut flags = BTreeMap::new();
    while let Some(flag) = args.next() {
        if !flag.starts_with("--") {
            return Err("expected a named option".into());
        }
        let value = args.next().ok_or("option requires a value")?;
        if flags.insert(flag, value).is_some() {
            return Err("duplicate option".into());
        }
    }
    let allowed: &[&str] = match command.as_str() {
        "validate" => &["--config"],
        "serve" => &["--config", "--state-dir"],
        "status" | "stop" => &["--socket"],
        "request" => &["--socket", "--timeout-ms"],
        _ => return Err("unknown command".into()),
    };
    if flags.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("unknown option".into());
    }
    let required = |name: &str| flags.get(name).ok_or_else(|| format!("missing {name}"));
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
                        .take(2 * 1024 * 1024 + 1)
                        .read_to_end(&mut input)?;
                    if input.len() > 2 * 1024 * 1024 {
                        return Err("operation exceeds 2 MiB".into());
                    }
                    Operation::from_json(&input)?
                }
            };
            let expires = Instant::now() + Duration::from_millis(budget);
            let client = Client::connect(required("--socket")?, expires)?;
            let result = client.call(operation, expires)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"identity": client.identity(), "result": result})
                )?
            );
        }
    }
    Ok(())
}

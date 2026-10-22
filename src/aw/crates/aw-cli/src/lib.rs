//! Unified AW command presentation and orchestration over reusable framework libraries.

mod cli;
mod package;
mod routing;

use aw_service::{Operation, Server};
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

/// Run the `aw` frontend and preserve native Agent exit codes and signals.
pub fn main() {
    match run() {
        Ok(exit) => cli::finish(exit),
        Err(error) => {
            eprintln!("aw: {error}");
            std::process::exit(1);
        }
    }
}

/// Run the legacy package frontend using the same implementation as `aw`.
pub fn package_main() {
    package::compatibility_main();
}

fn run() -> cli::Result<cli::Exit> {
    let arguments = routing::decode(std::env::args_os().skip(1))?;
    match routing::select(&arguments)? {
        routing::Route::Help => {
            help();
            Ok(cli::Exit::Code(0))
        }
        routing::Route::Runtime { command, arguments } => runtime(command, arguments),
        routing::Route::Package { command, arguments } => {
            package::dispatch(command, arguments)?;
            Ok(cli::Exit::Code(0))
        }
    }
}

fn help() {
    println!(
            "aw run --config FILE --agent TARGET [--reloadable] [--native-settings JSON_FILE] [--native-profile PROFILE] [--native-state-dir DIRECTORY] -- [AGENT_ARGS]\n\
aw install --prefix ABSOLUTE_DIR --bundle ABSOLUTE_DIR\n\
aw uninstall --prefix ABSOLUTE_DIR\n\
aw agent connect --config FILE --agent TARGET [--native-profile PROFILE]\n\
aw config init --prefix ABSOLUTE_DIR --config ABSOLUTE_FILE --state-dir ABSOLUTE_DIR [OPTIONS]\n\
aw config show|add-provider|remove-provider|add-event|add-hook|remove-hook --config ABSOLUTE_FILE [OPTIONS]\n\
aw config validate --config FILE [--against BASELINE_FILE]\n\
aw update (--config CURRENT_FILE | --socket ABSOLUTE_PATH) (--agent TARGET | --instance ID) --policy NEXT_FILE [--expected-revision SHA256]\n\
aw inspect --config ABS_FILE --json\n\
aw policy < provider-request.json\n\
aw serve --config FILE --state-dir ABSOLUTE_DIR\n\
aw status|stop (--config FILE | --socket ABSOLUTE_PATH)\n\
aw request --socket ABSOLUTE_PATH [--timeout-ms 1..60000] < operation.json\n\n\
Pi Coding Agent/Core 0.84.2: run requires a Node SDK application module.\n\
Qoder CLI >=1.1.64: run accepts optional --native-settings; records the detected version.\n\
OpenClaw 2026.9.6: run requires --native-settings and --native-state-dir.\n\
AgentScope 2.0.6: run requires Python script/-m entrypoint and explicit AwBridge.create_agent.\n\
QwenPaw 2.2.2b4: run requires QWENPAW_WORKING_DIR.\n\
Hermes 952c941e: run and agent connect require --native-profile; run requires the installed AW plugin.\n\
Claude SDK 0.2.88: run requires a foreground Python worker using aw_claude.create_client; bundled CLI 2.1.161.\n\
Package installation currently accepts local unpacked Preview bundles only.\n\
Compatibility: aw install --config FILE --agent TARGET retains native Hook installation;\n\
aw validate retains the config validate behavior. Agent connect does not install an Agent."
        );
}

fn runtime(command: &str, arguments: &[String]) -> cli::Result<cli::Exit> {
    let args = cli::Arguments::parse(arguments.iter().cloned())?;
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
        return cli::dispatch(command, &args);
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
            let client = cli::connect_service(&args, expires)?;
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

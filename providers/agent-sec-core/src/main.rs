//! Bounded stdio entry point for the sec-core AW Provider.

use aw_provider_sec_core::Transport;
use std::{path::PathBuf, time::Duration};

fn main() {
    if let Err(error) = execute() {
        eprintln!("aw-provider-sec-core: {error}");
        std::process::exit(1);
    }
}

fn execute() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    const USAGE: &str = "usage: aw-provider-sec-core --cli ABSOLUTE_PATH --socket ABSOLUTE_PATH [--transport cli|daemon] [--wait-for-backend]
       aw-provider-sec-core --transport daemon --socket ABSOLUTE_PATH [--wait-for-backend]
       aw-provider-sec-core --health --helper ABSOLUTE_PATH --socket ABSOLUTE_PATH";
    if args.len() == 1 && args[0] == "--help" {
        println!("{USAGE}");
        return Ok(());
    }
    if args.len() == 5 && args[0] == "--health" && args[1] == "--helper" && args[3] == "--socket" {
        let result = aw_provider_sec_core::health::query(
            &PathBuf::from(&args[2]),
            &PathBuf::from(&args[4]),
            Duration::from_secs(1),
        )?;
        println!("{}", serde_json::to_string(&result)?);
        return Ok(());
    }

    let mut cli: Option<PathBuf> = None;
    let mut socket: Option<PathBuf> = None;
    let mut transport = Transport::Cli;
    let mut transport_seen = false;
    let mut wait_for_backend = false;
    let mut idx = 0;
    while idx < args.len() {
        let flag = args[idx].to_str().ok_or(USAGE)?;
        match flag {
            "--cli" => {
                if cli.is_some() || idx + 1 >= args.len() {
                    return Err(USAGE.into());
                }
                cli = Some(PathBuf::from(&args[idx + 1]));
                idx += 2;
            }
            "--socket" => {
                if socket.is_some() || idx + 1 >= args.len() {
                    return Err(USAGE.into());
                }
                socket = Some(PathBuf::from(&args[idx + 1]));
                idx += 2;
            }
            "--transport" => {
                if transport_seen || idx + 1 >= args.len() {
                    return Err(USAGE.into());
                }
                let val = args[idx + 1].to_str().ok_or(USAGE)?;
                transport = match val {
                    "cli" => Transport::Cli,
                    "daemon" => Transport::Daemon,
                    _ => return Err(USAGE.into()),
                };
                transport_seen = true;
                idx += 2;
            }
            "--wait-for-backend" => {
                if wait_for_backend {
                    return Err(USAGE.into());
                }
                wait_for_backend = true;
                idx += 1;
            }
            _ => return Err(USAGE.into()),
        }
    }

    let socket = socket.ok_or(USAGE)?;
    if !socket.is_absolute() {
        return Err("--cli and --socket must be absolute paths".into());
    }
    let cli = match cli {
        Some(path) => {
            if !path.is_absolute() {
                return Err("--cli and --socket must be absolute paths".into());
            }
            path
        }
        None if transport == Transport::Daemon => PathBuf::new(),
        None => return Err(USAGE.into()),
    };

    aw_provider_sec_core::run_with_transport(
        &mut std::io::stdin().lock(),
        &mut std::io::stdout().lock(),
        &cli,
        &socket,
        // Cap the execution budget, not blocking stdio. The caller must bound
        // the entire process lifetime, including input before EOF and output.
        Duration::from_secs(60),
        wait_for_backend,
        transport,
    )?;
    Ok(())
}

//! User-facing native installation, configuration and removal commands.

use aw_package::{Error, Policy, Result, Settings};
use std::{collections::BTreeMap, path::Path};

#[path = "support/signals.rs"]
mod signals;

fn main() {
    if let Err(error) = run() {
        eprintln!("aw-package: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    let command = arguments.next().unwrap_or_else(|| "--help".into());
    if command == "--help" {
        println!("aw-package install --prefix ABS_DIR [--bundle ABS_DIR]\naw-package configure --prefix ABS_DIR --config ABS_FILE --state-dir ABS_DIR [--qoder ABS_FILE] [--node ABS_FILE --openclaw ABS_FILE] [--provider sec-core --socket ABS_FILE | --provider command --check ABS_PROGRAM --effect block|observe --reason-code CODE -- [CHECK_ARGS]]\naw-package uninstall --prefix ABS_DIR\nSelect at least one Agent. No Provider is enabled by default.\nInstall immutable Preview packages; configuration and Agent state remain external.");
        return Ok(());
    }
    let allowed: &[&str] = match command.as_str() {
        "install" => &["--prefix", "--bundle"],
        "configure" => &[
            "--prefix",
            "--config",
            "--state-dir",
            "--socket",
            "--qoder",
            "--node",
            "--openclaw",
            "--provider",
            "--check",
            "--effect",
            "--reason-code",
        ],
        "uninstall" => &["--prefix"],
        _ => return Err(Error::Invalid(format!("unknown command: {command}"))),
    };
    let mut flags = BTreeMap::new();
    let mut check_args = Vec::new();
    while let Some(flag) = arguments.next() {
        if flag == "--" && command == "configure" {
            check_args.extend(arguments);
            break;
        }
        if !allowed.contains(&flag.as_str()) || flags.contains_key(&flag) {
            return Err(Error::Invalid(format!("unknown or duplicate flag: {flag}")));
        }
        let value = arguments
            .next()
            .ok_or_else(|| Error::Invalid(format!("missing value for {flag}")))?;
        flags.insert(flag, value);
    }
    let required = |name: &str| -> Result<&Path> {
        flags
            .get(name)
            .map(Path::new)
            .ok_or_else(|| Error::Invalid(format!("missing {name}")))
    };
    let prefix = required("--prefix")?;
    match command.as_str() {
        "install" => {
            let executable = std::env::current_exe()?;
            let bundle = flags
                .get("--bundle")
                .map(Path::new)
                .or(executable.parent())
                .ok_or_else(|| Error::Invalid("missing package directory".into()))?;
            signals::register()?;
            aw_package::install_cancellable(bundle, prefix, &signals::CANCEL)?;
            println!("Installed verified AW components in {}", prefix.display());
        }
        "configure" => {
            let optional = |name: &str| flags.get(name).map(Path::new);
            let selection = flags
                .get("--provider")
                .map(String::as_str)
                .unwrap_or("none");
            let command_flags = ["--check", "--effect", "--reason-code"];
            if (flags.contains_key("--socket") && selection != "sec-core")
                || ((command_flags.iter().any(|name| flags.contains_key(*name))
                    || !check_args.is_empty())
                    && selection != "command")
            {
                return Err(Error::Invalid("Provider options require an explicit matching --provider; use --provider sec-core with --socket".into()));
            }
            let mut argv = Vec::new();
            let policy = match selection {
                "none" => Policy::None,
                "sec-core" => Policy::SecCore {
                    socket: required("--socket")?,
                },
                "command" => {
                    argv.push(required("--check")?.to_string_lossy().into_owned());
                    argv.extend(check_args);
                    Policy::Command {
                        argv: &argv,
                        effect: flags
                            .get("--effect")
                            .ok_or_else(|| Error::Invalid("missing --effect".into()))?,
                        reason_code: flags
                            .get("--reason-code")
                            .ok_or_else(|| Error::Invalid("missing --reason-code".into()))?,
                    }
                }
                _ => {
                    return Err(Error::Invalid(
                        "--provider must be none, sec-core or command".into(),
                    ))
                }
            };
            aw_package::configure(&Settings {
                prefix,
                config: required("--config")?,
                state: required("--state-dir")?,
                policy,
                qoder: optional("--qoder"),
                node: optional("--node"),
                openclaw: optional("--openclaw"),
            })?;
            println!(
                "Created {}; Provider template: {selection}",
                required("--config")?.display()
            );
        }
        "uninstall" => {
            signals::register()?;
            aw_package::uninstall_cancellable(prefix, &signals::CANCEL)?;
            println!("Removed AW package files; external configuration and state retained");
        }
        _ => unreachable!("command was validated before parsing flags"),
    }
    Ok(())
}

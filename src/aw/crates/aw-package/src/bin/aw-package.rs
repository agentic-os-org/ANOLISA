//! User-facing native installation, configuration and removal commands.

use aw_package::{Error, Result, Settings};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

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
        println!("aw-package install --prefix ABS_DIR [--bundle ABS_DIR]\naw-package configure --prefix ABS_DIR --config ABS_FILE --state-dir ABS_DIR --socket ABS_FILE --qoder ABS_FILE --node ABS_FILE --openclaw ABS_FILE\naw-package uninstall --prefix ABS_DIR\nInstall immutable Preview packages; configuration and Agent state remain external.");
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
        ],
        "uninstall" => &["--prefix"],
        _ => return Err(Error::Invalid(format!("unknown command: {command}"))),
    };
    let mut flags = BTreeMap::new();
    while let Some(flag) = arguments.next() {
        if !allowed.contains(&flag.as_str()) || flags.contains_key(&flag) {
            return Err(Error::Invalid(format!("unknown or duplicate flag: {flag}")));
        }
        let value = arguments
            .next()
            .ok_or_else(|| Error::Invalid(format!("missing value for {flag}")))?;
        flags.insert(flag, PathBuf::from(value));
    }
    let required = |name: &str| -> Result<&Path> {
        flags
            .get(name)
            .map(PathBuf::as_path)
            .ok_or_else(|| Error::Invalid(format!("missing {name}")))
    };
    let prefix = required("--prefix")?;
    match command.as_str() {
        "install" => {
            let executable = std::env::current_exe()?;
            let bundle = flags
                .get("--bundle")
                .map(PathBuf::as_path)
                .or(executable.parent())
                .ok_or_else(|| Error::Invalid("missing package directory".into()))?;
            signals::register()?;
            aw_package::install_cancellable(bundle, prefix, &signals::CANCEL)?;
            println!("Installed verified AW components in {}", prefix.display());
        }
        "configure" => {
            aw_package::configure(&Settings {
                prefix,
                config: required("--config")?,
                state: required("--state-dir")?,
                socket: required("--socket")?,
                qoder: required("--qoder")?,
                node: required("--node")?,
                openclaw: required("--openclaw")?,
            })?;
            println!(
                "Created {}; use the same file for qoder and openclaw",
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

//! Package installation and configuration actions for the unified AW frontend.

use crate::output::Outcome;
use aw_package::{Error, Policy, Result, Settings};
use std::{collections::BTreeMap, path::Path};

pub(crate) mod arguments;
pub(crate) mod config;
pub(crate) mod signals;

fn configure(flags: &BTreeMap<String, String>, check_args: Vec<String>) -> Result<Outcome> {
    let required = |name: &str| -> Result<&Path> {
        flags
            .get(name)
            .map(Path::new)
            .ok_or_else(|| Error::Invalid(format!("missing {name}")))
    };
    let prefix = required("--prefix")?;
    let optional = |name: &str| flags.get(name).map(Path::new);
    let selection = flags
        .get("--provider")
        .map(String::as_str)
        .unwrap_or("none");
    let command_flags = ["--check", "--effect", "--reason-code"];
    if (flags.contains_key("--socket") && !matches!(selection, "sec-core" | "sec-core+tokenless"))
        || ((command_flags.iter().any(|name| flags.contains_key(*name)) || !check_args.is_empty())
            && selection != "command")
    {
        return Err(Error::Invalid("Provider options require an explicit matching --provider; use --provider sec-core with --socket".into()));
    }
    let mut argv = Vec::new();
    let policy = match selection {
        "none" => Policy::None,
        "tokenless" => Policy::Tokenless,
        "sec-core+tokenless" => Policy::SecCoreTokenless {
            socket: required("--socket")?,
        },
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
                "--provider must be none, sec-core, tokenless, sec-core+tokenless or command"
                    .into(),
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
    Ok(Outcome::Configured {
        config: required("--config")?.into(),
        provider_template: selection.into(),
    })
}

pub(crate) fn managed(
    label: &'static str,
    command: &str,
    flags: &BTreeMap<String, String>,
    arguments: Vec<String>,
    options: crate::output::Options,
) -> crate::cli::Result<crate::cli::Exit> {
    if !matches!(command, "install" | "uninstall") {
        return crate::output::complete(label, options, configure(flags, arguments));
    }
    let prefix = Path::new(flags.get("--prefix").ok_or("missing --prefix")?);
    let outcome = if command == "install" {
        Outcome::Installed {
            prefix: prefix.into(),
        }
    } else {
        Outcome::Uninstalled
    };
    let result = (|| -> aw_package::MutationResult {
        signals::register().map_err(|error| aw_package::MutationFailure {
            error: Error::Io(error),
            report: aw_package::MutationReport::default(),
        })?;
        if command == "install" {
            let bundle = flags
                .get("--bundle")
                .ok_or_else(|| aw_package::MutationFailure {
                    error: Error::Invalid("missing --bundle".into()),
                    report: aw_package::MutationReport::default(),
                })?;
            aw_package::install_reported(Path::new(bundle), prefix, &signals::CANCEL)
        } else {
            aw_package::uninstall_reported(prefix, &signals::CANCEL)
        }
    })();
    crate::output::complete_mutation(label, options, outcome, result)
}

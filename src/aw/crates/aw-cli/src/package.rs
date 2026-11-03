//! Offline configuration actions for the unified AW frontend.

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
    if (command_flags.iter().any(|name| flags.contains_key(*name)) || !check_args.is_empty())
        && selection != "command"
    {
        return Err(Error::Invalid(
            "command options require --provider command".into(),
        ));
    }
    let template = flags
        .get("--template")
        .map(|file| -> Result<_> {
            let path = Path::new(file);
            if !path.is_absolute() {
                return Err(Error::Invalid("template must be an absolute file".into()));
            }
            let definition = config::definition(file)?;
            let values: Vec<String> = flags
                .get("--template-vars")
                .map(|value| serde_json::from_str(value))
                .transpose()?
                .unwrap_or_default();
            let mut variables = BTreeMap::new();
            for item in values {
                let (name, value) = item.split_once('=').ok_or_else(|| {
                    Error::Invalid("template variables must be NAME=VALUE".into())
                })?;
                if variables
                    .insert(name.to_owned(), value.to_owned())
                    .is_some()
                {
                    return Err(Error::Invalid(format!(
                        "duplicate template variable: {name}"
                    )));
                }
            }
            Ok(Policy::Template {
                definition,
                variables,
            })
        })
        .transpose()?;
    let mut argv = Vec::new();
    let policy =
        match selection {
            "none" => template.unwrap_or(Policy::None),
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
            _ => return Err(Error::Invalid(
                "--provider must be none or command; use --template for supplied Provider presets"
                    .into(),
            )),
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
        provider_template: flags
            .get("--template")
            .cloned()
            .unwrap_or_else(|| selection.into()),
    })
}

pub(crate) fn configure_output(
    label: &'static str,
    flags: &BTreeMap<String, String>,
    arguments: Vec<String>,
    options: crate::output::Options,
) -> crate::cli::Result<crate::cli::Exit> {
    crate::output::complete(label, options, configure(flags, arguments))
}

//! Offline edits reuse the same parser and publication boundary as the library.

use aw_package::{config_edit::ConfigurationEditor, Error, Result};
use std::{collections::BTreeMap, fs::File, io::Read, os::unix::fs::OpenOptionsExt, path::Path};

/// Command reference also printed by the packaging CLI's top-level help.
pub(super) const HELP: &str =
    "aw-package config show --config ABS_FILE [--provider NAME | --event EVENT [--id ID]]
aw-package config validate --config ABS_FILE
aw-package config add-provider --config ABS_FILE --name NAME --definition FILE
aw-package config remove-provider --config ABS_FILE --name NAME
aw-package config add-event --config ABS_FILE --event EVENT --definition FILE
aw-package config add-hook --config ABS_FILE --event EVENT --definition FILE
aw-package config remove-hook --config ABS_FILE --event EVENT --id ID
Definitions are complete YAML/JSON objects. Show includes private Provider values.
Edits are offline; changed files are rewritten as YAML. No service reload.";

/// Parses offline editor options and publishes at most one validated edit.
pub(super) fn run(mut arguments: impl Iterator<Item = String>) -> Result<()> {
    let action = arguments.next().unwrap_or_else(|| "--help".into());
    if action == "--help" {
        if arguments.next().is_some() {
            return Err(Error::Invalid("unexpected arguments after --help".into()));
        }
        println!("{HELP}");
        return Ok(());
    }
    let allowed: &[&str] = match action.as_str() {
        "show" => &["--config", "--provider", "--event", "--id"],
        "validate" => &["--config"],
        "add-provider" => &["--config", "--name", "--definition"],
        "remove-provider" => &["--config", "--name"],
        "add-event" | "add-hook" => &["--config", "--event", "--definition"],
        "remove-hook" => &["--config", "--event", "--id"],
        _ => return Err(Error::Invalid(format!("unknown config action: {action}"))),
    };
    let mut flags = BTreeMap::new();
    while let Some(flag) = arguments.next() {
        if !allowed.contains(&flag.as_str()) || flags.contains_key(&flag) {
            return Err(Error::Invalid(format!("unknown or duplicate flag: {flag}")));
        }
        let value = arguments
            .next()
            .filter(|value| !value.starts_with("--"))
            .ok_or_else(|| Error::Invalid(format!("missing value for {flag}")))?;
        flags.insert(flag, value);
    }
    let required = |name: &str| -> Result<&str> {
        flags
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| Error::Invalid(format!("missing {name}")))
    };
    let path = Path::new(required("--config")?);
    // Validate every action's options before opening any user files.
    match action.as_str() {
        "show" => {
            if (flags.contains_key("--provider") && flags.contains_key("--event"))
                || (flags.contains_key("--id") && !flags.contains_key("--event"))
            {
                return Err(Error::Invalid(
                    "select --provider or --event; --id requires --event".into(),
                ));
            }
        }
        "add-provider" | "remove-provider" => {
            required("--name")?;
        }
        "add-event" | "add-hook" | "remove-hook" => {
            required("--event")?;
        }
        _ => {}
    }
    if action.starts_with("add-") {
        required("--definition")?;
    }
    if action == "remove-hook" {
        required("--id")?;
    }
    let mut editor = ConfigurationEditor::open(path)?;
    match action.as_str() {
        "show" => {
            let value = if let Some(name) = flags.get("--provider") {
                editor.provider(name)
            } else if let Some(event) = flags.get("--event") {
                if let Some(id) = flags.get("--id") {
                    editor.hook(event, id)
                } else {
                    editor.as_value()["spec"]["events"].get(event)
                }
            } else {
                Some(editor.as_value())
            }
            .ok_or_else(|| Error::Invalid("selected Provider, event or Hook is absent".into()))?;
            println!("{}", serde_json::to_string_pretty(value)?);
            return Ok(());
        }
        "validate" => {
            editor.validate()?;
            println!("Valid configuration");
            return Ok(());
        }
        "add-provider" => {
            editor.add_provider(required("--name")?, definition(required("--definition")?)?)?;
        }
        "remove-provider" => {
            editor.remove_provider(required("--name")?)?;
        }
        "add-event" => {
            editor.add_event(required("--event")?, definition(required("--definition")?)?)?;
        }
        "add-hook" => {
            editor.add_hook(required("--event")?, definition(required("--definition")?)?)?;
        }
        "remove-hook" => {
            editor.remove_hook(required("--event")?, required("--id")?)?;
        }
        _ => unreachable!("action was validated before parsing flags"),
    }
    if editor.save()? {
        println!("Updated configuration");
    } else {
        println!("Configuration unchanged");
    }
    Ok(())
}

fn definition(path: &str) -> Result<serde_json::Value> {
    // A FIFO must not block before the regular-file check can reject it.
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(Error::Invalid("definition must be a regular file".into()));
    }
    let mut bytes = Vec::new();
    file.take(aw_config::MAX_DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    let value =
        aw_config::parse_value(&bytes).map_err(|error| Error::Invalid(error.to_string()))?;
    if !value.is_object() {
        return Err(Error::Invalid(
            "definition must be a YAML/JSON object".into(),
        ));
    }
    Ok(value)
}

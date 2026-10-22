//! Offline edits reuse the same parser and publication boundary as the library.

use crate::output::Outcome;
use aw_package::{config_edit::ConfigurationEditor, Error, Result};
use std::{collections::BTreeMap, fs::File, io::Read, os::unix::fs::OpenOptionsExt, path::Path};

pub(crate) fn execute(action: &str, flags: &BTreeMap<String, String>) -> Result<Option<Outcome>> {
    let required = |name: &str| -> Result<&str> {
        flags
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| Error::Invalid(format!("missing {name}")))
    };
    let path = Path::new(required("--config")?);
    // Validate every action's options before opening any user files.
    match action {
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
    match action {
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
            return Ok(None);
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
        _ => unreachable!("the typed command selects an existing config action"),
    }
    Ok(Some(Outcome::ConfigurationEdited {
        changed: editor.save()?,
    }))
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

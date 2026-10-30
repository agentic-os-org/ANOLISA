//! Stable native commands are independent of policy step IDs and budgets.

use crate::{HookEvent, Result};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path};

pub(super) fn generate(
    mut settings: Value,
    executable: &Path,
    binding: &Path,
    sequential: Option<bool>,
) -> Result<(Value, BTreeMap<String, HookEvent>)> {
    let hooks = settings
        .as_object_mut()
        .ok_or("invalid settings")?
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("invalid hooks")?;
    for (native, event) in [("PreToolUse", "tool.before"), ("PostToolUse", "tool.after")] {
        let action = if event == "tool.before" {
            "block"
        } else {
            "report"
        };
        let mut group = json!({"matcher":"*","hooks":[{"type":"command","command":executable,
            "args":["hook","--adapter","qoder","--binding",binding,"--event",event,"--step",crate::reload::DISPATCH,"--on-error",action],"timeout":60}]});
        if let Some(sequential) = sequential {
            group["sequential"] = json!(sequential);
        }
        hooks
            .entry(native)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("invalid native event")?
            .push(group);
    }
    for native in [
        "SessionStart",
        "SessionEnd",
        "UserPromptSubmit",
        "Notification",
        "PostToolUseFailure",
        "PermissionDenied",
    ] {
        let mut group = json!({"hooks":[{"type":"command","command":executable,
            "args":["bridge","--adapter","qoder","--binding",binding],"timeout":4}]});
        if native == "Notification" {
            group["matcher"] = json!("idle_prompt");
        }
        hooks
            .entry(native)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("invalid native lifecycle event")?
            .push(group);
    }
    Ok((settings, crate::reload::routes()))
}

pub(super) fn check_sources(
    cwd: &Path,
    environment: &BTreeMap<std::ffi::OsString, std::ffi::OsString>,
    flags: &Value,
    sequential: bool,
) -> Result<()> {
    check_scheduling(flags, sequential)?;
    let home = environment
        .get(std::ffi::OsStr::new("HOME"))
        .ok_or("HOME is required for Qoder settings")?;
    for path in [
        std::path::PathBuf::from(home).join(".qoder/settings.json"),
        cwd.join(".qoder/settings.json"),
        cwd.join(".qoder/settings.local.json"),
    ] {
        match std::fs::symlink_metadata(&path) {
            Ok(_) => check_scheduling(
                &serde_json::from_slice(&crate::read_file(path, 1024 * 1024)?)?,
                sequential,
            )?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn check_scheduling(settings: &Value, sequential: bool) -> Result<()> {
    if !sequential
        && ["PreToolUse", "PostToolUse"].iter().any(|event| {
            settings["hooks"][event]
                .as_array()
                .is_some_and(|groups| groups.iter().any(|group| group["sequential"] == true))
        })
    {
        return Err("reloadable Qoder requires qoder.sequential=true when native settings select sequential tool hooks".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stable_group_preserves_native_scheduling_and_rejects_ambiguous_parallelism() {
        let native = json!({"hooks":{"PreToolUse":[{"sequential":true,"hooks":[]}]}});
        assert!(check_scheduling(&native, false).is_err());
        check_scheduling(&native, true).unwrap();
        let (settings, _) =
            generate(native, Path::new("/aw"), Path::new("/binding"), Some(true)).unwrap();
        assert_eq!(settings["hooks"]["PreToolUse"][1]["sequential"], true);
        assert_eq!(settings["hooks"]["PostToolUse"][0]["sequential"], true);
    }
}

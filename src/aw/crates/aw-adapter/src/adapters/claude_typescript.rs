//! Explicit TypeScript Query factory using the shared Claude native runtime gate.

use crate::{
    adapter::{self, LaunchContext, LaunchInput, LaunchPlan, PreparedLaunch, Readiness},
    Result,
};
use aw_core::admission::{AdapterCapabilities, AdmittedStep, StepExecution};
use serde_json::{json, Value};
use std::{fs, path::PathBuf, sync::atomic::AtomicBool, time::Duration};

struct Prepared {
    input: LaunchInput,
    sdk: Value,
    profile: Value,
}

pub(super) fn prepare(
    input: LaunchInput,
    cancelled: &AtomicBool,
) -> Result<Box<dyn PreparedLaunch>> {
    let application = PathBuf::from(&input.command.args[0]);
    if !application.is_absolute() || !application.is_file() {
        return Err(
            "Claude TypeScript SDK requires an existing absolute .mjs or .js worker".into(),
        );
    }
    let script = format!(
        "{}\nconst {{files, ...sdk}} = inspect(process.argv.at(-1)); console.log(JSON.stringify(sdk));",
        include_str!("../../native/claude-sdk/typescript-probe.mjs")
    );
    let output = adapter::Probe(cancelled).version_output(
        &input.command,
        &[
            "--input-type=module".into(),
            "--eval".into(),
            script.into(),
            "--".into(),
            application.canonicalize()?.into_os_string(),
        ],
    )?;
    let mut sdk: Value = serde_json::from_str(&output)?;
    let profiles: Vec<Value> = serde_json::from_str(include_str!(
        "../../native/claude-sdk/typescript-profiles.json"
    ))?;
    let profile = profiles
        .into_iter()
        .find(|profile| {
            ["sdk", "cli", "platform", "sha256"]
                .iter()
                .all(|field| profile[field] == sdk[field])
        })
        .ok_or("Claude TypeScript SDK runtime fingerprint is unsupported")?;
    let root = PathBuf::from(sdk["entry"].as_str().ok_or("Missing SDK entry")?);
    let root = root.parent().ok_or("Missing SDK directory")?;
    let mut files = serde_json::Map::new();
    for name in profile["sha256"]
        .as_object()
        .ok_or("Missing SDK profile hashes")?
        .keys()
    {
        let path = if name == "_bundled/claude" {
            sdk["binary"].clone()
        } else {
            json!(root.join(
                name.strip_prefix("sdk/")
                    .ok_or("Invalid SDK profile path")?
            ))
        };
        files.insert(name.clone(), path);
    }
    sdk["files"] = Value::Object(files);
    Ok(Box::new(Prepared {
        input,
        sdk,
        profile,
    }))
}

impl PreparedLaunch for Prepared {
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            adapter: "claude-sdk".into(),
            version: self.profile["sdk"].as_str().unwrap_or_default().into(),
            entrypoint: "typescript-sdk-worker".into(),
            events: [
                ("tool.before".into(), vec!["observe".into(), "block".into()]),
                ("tool.after".into(), vec!["observe".into()]),
                (
                    "security.violation".into(),
                    vec!["observe".into(), "block".into()],
                ),
            ]
            .into(),
            failure_actions: Default::default(),
        }
    }

    fn validate_steps(&self, steps: &[AdmittedStep]) -> Result<()> {
        super::claude_final::validate(&self.input.document, steps)?;
        if steps
            .iter()
            .any(|step| matches!(step.execution, StepExecution::Native))
        {
            return Err("Claude TypeScript SDK requires structured Provider steps".into());
        }
        Ok(())
    }

    fn configure(&mut self, context: &LaunchContext<'_>) -> Result<LaunchPlan> {
        let (events, hooks) =
            super::sdk_tools::routes(&self.input.document, context.steps, "claude-sdk")?;
        let token = fs::read_to_string("/proc/sys/kernel/random/uuid")?
            .trim()
            .to_owned();
        let ready = context.files.directory().join("claude-sdk-ready.json");
        let bridge = context.files.write(
            "claude-typescript-bridge.mjs",
            include_bytes!("../../native/claude-sdk/typescript-bridge.mjs"),
        )?;
        fs::create_dir_all(context.files.directory().join("shared"))?;
        context.files.write(
            "shared/transport.mjs",
            include_bytes!("../../native/shared/transport.mjs"),
        )?;
        let final_profile = events
            .contains_key("security.violation")
            .then_some(&self.profile);
        if final_profile.is_some() {
            super::claude_final::configure(context)?;
        }
        let config = context.files.write(
            "claude-sdk-hooks.json",
            &serde_json::to_vec(&json!({
                "executable": context.executable, "binding": context.binding_path,
                "ready_path": ready, "ready_token": token, "hooks": hooks,
                "final_profile": final_profile, "final_enforcing": super::claude_final::enforcing(&hooks),
                "typescript": self.sdk,
            }))?,
        )?;
        let mut command = self.input.command.clone();
        command
            .environment
            .insert("AW_CLAUDE_CONFIG".into(), config.into_os_string());
        command
            .environment
            .insert("AW_CLAUDE_SDK_BRIDGE".into(), bridge.into_os_string());
        Ok(LaunchPlan {
            provider_environment: command.environment.clone(),
            command,
            events,
            readiness: Some(Readiness {
                path: ready,
                token,
                adapter: "claude-sdk".into(),
                hooks: hooks.len(),
                timeout: Duration::from_secs(30),
            }),
        })
    }
}

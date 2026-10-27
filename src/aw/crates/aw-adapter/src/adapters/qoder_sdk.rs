//! Explicit per-Query Hook registration for the pinned Qoder JavaScript SDK.

use crate::{
    adapter::{
        self, Adapter, HookOutput, LaunchContext, LaunchInput, LaunchPlan, NativeContext,
        PreparedLaunch, Readiness,
    },
    Exit, Result,
};
use aw_provider::admission::{AdapterCapabilities, AdmittedStep};
use serde_json::{json, Value};
use std::{fs, path::PathBuf, time::Duration};

mod assets;

#[cfg(test)]
mod tests;

pub(crate) struct QoderSdk;
struct Prepared {
    input: LaunchInput,
    sdks: std::collections::BTreeMap<String, PathBuf>,
    policy: assets::Policy,
}

impl Adapter for QoderSdk {
    fn prepare(
        &self,
        input: LaunchInput,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<Box<dyn PreparedLaunch>> {
        if !input.options.reloadable && adapter::observation_enabled(&input.document, &input.target)
        {
            return Err("AW resident Qoder SDK does not support AgentSight evidence; set evidence.mode to off".into());
        }
        if input.options.settings.is_some()
            || input.options.profile.is_some()
            || input.options.state_dir.is_some()
        {
            return Err(
                "Qoder SDK uses explicit Query options; native overrides are unsupported".into(),
            );
        }
        let application = input
            .command
            .args
            .first()
            .map(PathBuf::from)
            .ok_or("Qoder SDK requires node and an absolute .mjs worker")?;
        if !application.is_absolute()
            || !application.is_file()
            || application.extension().and_then(|s| s.to_str()) != Some("mjs")
        {
            return Err("Qoder SDK requires an existing absolute .mjs worker".into());
        }
        let agent = &input.document["spec"]["agents"][&input.target];
        if input.options.reloadable
            && (agent["qoder_sdk"]["declarative_plugins"] == true
                || agent["qoder_sdk"]["command_wrappers"]
                    .as_array()
                    .is_some_and(|grants| !grants.is_empty()))
        {
            return Err(
                "Qoder SDK declarative plugins and command wrappers require ordinary resident mode"
                    .into(),
            );
        }
        let policy = assets::Policy::read(agent)?;
        let sdk = adapter::Probe(cancelled).version_output(
            &input.command,
            &[
                "--input-type=module".into(),
                "--eval".into(),
                include_str!("../../native/qoder-sdk/probe.mjs").into(),
                "--".into(),
                application.canonicalize()?.into_os_string(),
                serde_json::to_string(&policy.sites)?.into(),
            ],
        )?;
        let sdks: std::collections::BTreeMap<String, PathBuf> = serde_json::from_str(&sdk)?;
        if sdks.len() != policy.sites.len()
            || policy.sites.iter().any(|site| !sdks.contains_key(site))
            || sdks
                .values()
                .any(|sdk| !sdk.is_absolute() || !sdk.is_file())
        {
            return Err("Qoder SDK probe did not resolve the declared installed packages".into());
        }
        Ok(Box::new(Prepared {
            input,
            sdks,
            policy,
        }))
    }
    fn normalize(&self, binding: &NativeContext, event: &str, native: &Value) -> Result<Value> {
        super::sdk_tools::normalize_tools(binding, event, native, "qoder-sdk")
    }
    fn reply(&self, blocked: bool) -> HookOutput {
        HookOutput {
            exit: Exit::Code(if blocked { 2 } else { 0 }),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }
}
impl PreparedLaunch for Prepared {
    fn capabilities(&self) -> AdapterCapabilities {
        let mut capabilities = adapter::catalog::QODER_SDK.capabilities("1.0.56", "sdk-worker");
        if !self.input.options.reloadable {
            capabilities
                .failure_actions
                .insert("tool.after".into(), vec!["withhold_result".into()]);
        }
        capabilities
    }
    fn validate_steps(&self, steps: &[AdmittedStep]) -> Result<()> {
        if self.input.options.reloadable {
            crate::reload::validate(
                &self.input.document,
                &self.input.target,
                &self.capabilities(),
            )
        } else {
            super::sdk_tools::routes(&self.input.document, steps).map(|_| ())
        }
    }
    fn configure(&mut self, context: &LaunchContext<'_>) -> Result<LaunchPlan> {
        let (events, hooks) = if self.input.options.reloadable {
            (
                crate::reload::routes(),
                ["tool.before", "tool.after"]
                    .map(|event| {
                        json!({
                            "event":event,"step":crate::reload::DISPATCH,
                            "on_error":if event == "tool.before" { "block" } else { "report" },
                            "budget_ms":55000,"replace_result":false
                        })
                    })
                    .to_vec(),
            )
        } else {
            super::sdk_tools::routes(&self.input.document, context.steps)?
        };
        let token = fs::read_to_string("/proc/sys/kernel/random/uuid")?
            .trim()
            .to_owned();
        let ready = context.files.directory().join("qoder-sdk-ready.json");
        let bridge = context.files.write(
            "qoder-sdk.mjs",
            include_bytes!("../../native/qoder-sdk/bridge.mjs"),
        )?;
        context.files.write(
            "callback.mjs",
            include_bytes!("../../native/qoder-sdk/callback.mjs"),
        )?;
        context.files.write(
            "reload.mjs",
            include_bytes!("../../native/qoder-sdk/reload.mjs"),
        )?;
        for (name, bytes) in [
            (
                "post.mjs",
                include_bytes!("../../native/qoder-sdk/post.mjs").as_slice(),
            ),
            (
                "mcp.mjs",
                include_bytes!("../../native/qoder-sdk/mcp.mjs").as_slice(),
            ),
            (
                "daemon.mjs",
                include_bytes!("../../native/qoder-sdk/daemon.mjs").as_slice(),
            ),
            (
                "command-wrapper.mjs",
                include_bytes!("../../native/qoder-sdk/command-wrapper.mjs").as_slice(),
            ),
            (
                "native-options.mjs",
                include_bytes!("../../native/qoder-sdk/native-options.mjs").as_slice(),
            ),
            (
                "private.mjs",
                include_bytes!("../../native/qoder-sdk/private.mjs").as_slice(),
            ),
            (
                "resident.mjs",
                include_bytes!("../../native/qoder-sdk/resident.mjs").as_slice(),
            ),
            (
                "identity.mjs",
                include_bytes!("../../native/qoder-sdk/identity.mjs").as_slice(),
            ),
            (
                "scope.mjs",
                include_bytes!("../../native/shared/scope.mjs").as_slice(),
            ),
            (
                "transport.mjs",
                include_bytes!("../../native/shared/transport.mjs").as_slice(),
            ),
        ] {
            context.files.write(name, bytes)?;
        }
        let config = context.files.write(
            "qoder-sdk.json",
            &serde_json::to_vec(&json!({
                "executable":context.executable, "binding":context.binding_path, "sdk":self.sdks[&self.policy.sites[0]],
                "sdks":self.sdks, "default_site":self.policy.sites[0],
                "native_command_wrappers":self.policy.wrappers(), "native_policy":self.policy,
                "reloadable":self.input.options.reloadable, "resident":!self.input.options.reloadable,
                "resident_policy_reload": !self.input.options.reloadable,
                "cwd":self.input.command.cwd, "ready_path":ready, "ready_token":token, "hooks":hooks
            }))?,
        )?;
        let mut command = self.input.command.clone();
        command
            .environment
            .insert("AW_QODER_SDK_BRIDGE".into(), bridge.into_os_string());
        command
            .environment
            .insert("AW_QODER_SDK_CONFIG".into(), config.into_os_string());
        let provider_environment = command.environment.clone();
        Ok(LaunchPlan {
            command,
            provider_environment,
            events,
            readiness: Some(Readiness {
                path: ready,
                token,
                adapter: "qoder-sdk".into(),
                hooks: hooks.len(),
                timeout: Duration::from_secs(30),
            }),
        })
    }
}

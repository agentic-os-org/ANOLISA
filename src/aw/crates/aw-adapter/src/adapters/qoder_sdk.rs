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

pub(crate) struct QoderSdk;
struct Prepared {
    input: LaunchInput,
    sdk: PathBuf,
}

impl Adapter for QoderSdk {
    fn prepare(
        &self,
        input: LaunchInput,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<Box<dyn PreparedLaunch>> {
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
        let sdk = adapter::Probe(cancelled).version_output(
            &input.command,
            &[
                "--input-type=module".into(),
                "--eval".into(),
                include_str!("../../native/qoder-sdk/probe.mjs").into(),
                "--".into(),
                application.canonicalize()?.into_os_string(),
            ],
        )?;
        let sdk = PathBuf::from(sdk);
        if !sdk.is_absolute() || !sdk.is_file() {
            return Err("Qoder SDK probe did not resolve the installed package".into());
        }
        Ok(Box::new(Prepared { input, sdk }))
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
        adapter::catalog::QODER_SDK.capabilities("1.0.54", "sdk-worker")
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
        let config = context.files.write(
            "qoder-sdk.json",
            &serde_json::to_vec(&json!({
                "executable":context.executable, "binding":context.binding_path, "sdk":self.sdk,
                "reloadable":self.input.options.reloadable, "cwd":self.input.command.cwd, "ready_path":ready, "ready_token":token, "hooks":hooks
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

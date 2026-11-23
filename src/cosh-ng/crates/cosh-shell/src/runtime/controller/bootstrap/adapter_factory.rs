use crate::adapter::{adapter_for_kind, AdapterInstance, AdapterKind};

#[cfg(all(test, target_os = "linux"))]
#[path = "../../../../tests/protocol/r03_session_assets.rs"]
mod r03_session_assets_tests;

pub(super) fn build_adapter(kind: AdapterKind) -> AdapterInstance {
    let adapter = match adapter_for_kind(kind) {
        AdapterInstance::ClaudeCode(adapter) => {
            AdapterInstance::ClaudeCode(adapter.with_model_call(true))
        }
        AdapterInstance::QwenCli(adapter) => {
            AdapterInstance::QwenCli(adapter.with_model_call(true))
        }
        AdapterInstance::CoshCore(adapter) => {
            AdapterInstance::CoshCore(adapter.with_model_call(true))
        }
        other => other,
    };
    crate::adapter::admit_session_assets(match &adapter {
        AdapterInstance::CoshCore(core) => Some(&core.program),
        _ => None,
    });
    adapter
}

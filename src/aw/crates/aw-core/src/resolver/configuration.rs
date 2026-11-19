//! Validate protocol-specific requirements while the configured Providers are available.

use super::{resolve, ResolveError, ResolvedHookPlan, SourceCapability};
use aw_config::{requirements::StepRequirement, Configuration};
use serde_json::Value;
use ResolveError::Target as BadTarget;

/// Projects desired requirements from `config` for `target` and resolves them against `sources`.
///
/// # Errors
/// Rejects unknown targets, mismatched adapters, invalid requirements (including boolean Hook
/// replacement effects), or unsatisfied required events.
pub fn resolve_configuration(
    config: &Configuration,
    target: &str,
    sources: &[SourceCapability],
) -> Result<ResolvedHookPlan, ResolveError> {
    let spec = &config.as_value()["spec"];
    let agent = spec["agents"]
        .get(target)
        .ok_or(BadTarget("unknown configured Agent target"))?;
    let adapter = agent["adapter"]
        .as_str()
        .ok_or(BadTarget("configured Agent target lacks adapter"))?;
    let reqs = aw_config::requirements::project(config, target)?;
    for step in reqs.iter().flat_map(|req| &req.steps) {
        let provider = &spec["providers"][step.provider];
        if provider_protocol(provider) == aw_config::HOOK_PROTOCOL {
            validate_boolean_effects(step).map_err(ResolveError::Requirement)?;
        }
    }
    resolve(target, adapter, &reqs, sources)
}

pub(super) fn provider_protocol(provider: &Value) -> &str {
    provider["protocol"]
        .as_str()
        .unwrap_or(aw_config::HOOK_PROTOCOL)
}

pub(super) fn validate_boolean_effects(step: &StepRequirement<'_>) -> Result<(), &'static str> {
    if step
        .effects
        .iter()
        .any(|effect| matches!(*effect, "replace_input" | "replace_result"))
    {
        return Err("boolean Hooks cannot replace input or results");
    }
    Ok(())
}

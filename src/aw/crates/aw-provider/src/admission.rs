//! Pure capability admission for tool events; no discovery or runtime binding.

use crate::{Description, Error, ValidatedConfig};
use serde_json::Value;
use std::collections::BTreeMap;

/// Protocol-checked evidence supplied by the caller for one configured Provider.
///
/// The caller owns discovery and must associate both responses with the same
/// configured Provider. These values do not establish process identity or prove
/// that an Agent has installed a hook.
#[derive(Clone)]
pub struct ProviderEvidence {
    /// The operations, events and effects declared by the Provider.
    pub description: Description,
    /// The exact private configuration accepted by that Provider.
    pub configuration: ValidatedConfig,
}

/// Trusted Adapter evidence for a specific version and interaction entrypoint.
///
/// The embedding application supplies this evidence independently of Provider
/// claims. Admission does not probe the Agent or certify the supplied version.
#[derive(Clone, Debug)]
pub struct AdapterCapabilities {
    /// One of the four adapters declared by the configuration contract.
    pub adapter: String,
    /// The actual Agent version whose capabilities were established by the caller.
    pub version: String,
    /// The validated interaction mode, such as a print or interactive entrypoint.
    pub entrypoint: String,
    /// Supported effects by event, restricted to this library's initial surface.
    pub events: BTreeMap<String, Vec<String>>,
}

/// One configured step checked against the implementation and Adapter.
///
/// Only steps returned by [`admit`] have also passed Provider admission. Steps
/// returned by [`preflight`] remain candidates, and public fields carry no proof
/// of admission. This is not an executable Core plan or a Ready status. Runtime
/// binding, budgets, process ownership and adoption remain Host concerns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmittedStep {
    /// The tool event that selects this step.
    pub event: String,
    /// The step identifier within its event.
    pub step_id: String,
    /// The named Provider in the desired configuration.
    pub provider: String,
    /// The requested Provider operation for this event.
    pub operation: String,
    /// Requested effects, checked against Provider support only by [`admit`].
    pub effects: Vec<String>,
    /// The Host action required when Provider execution fails.
    pub on_error: String,
}

/// Admits enabled tool steps using explicit Provider and trusted Adapter evidence.
///
/// Disabled events and steps do not require Provider discovery. Enabled events
/// must be supported even when `required` is false or their step list is empty.
/// Step order within each event is preserved; the result does not order distinct
/// events or choose serial versus parallel execution.
///
/// # Errors
/// Rejects missing or mismatched evidence, unsupported events or effects, guards,
/// exact tool selectors and unsupported failure actions. Success establishes
/// capability admission only, not installed hooks or a complete runtime binding.
pub fn admit(
    config: &aw_config::Configuration,
    target: &str,
    capabilities: &AdapterCapabilities,
    providers: &BTreeMap<String, ProviderEvidence>,
) -> Result<Vec<AdmittedStep>, Error> {
    let steps = preflight(config, target, capabilities)?;
    let spec = &config.as_value()["spec"];
    for step in &steps {
        check_provider(spec, step, providers)?;
    }
    Ok(steps)
}

/// Checks enabled steps before the Host discovers or runs any Provider.
///
/// Checks the configured target, implementation and trusted Adapter capabilities,
/// transport, selectors and failure actions. Disabled events and steps are
/// skipped; enabled events require support even when optional or empty. Step
/// order within each event is preserved without choosing execution scheduling.
///
/// Returned steps are candidates, not Provider-admitted steps. Hosts must still
/// call [`admit`] with discovered Provider evidence before executing them.
///
/// # Errors
/// Rejects unsupported events, effects, transports, guards, selectors or failure
/// actions, and invalid or mismatched Adapter evidence. No Provider is invoked.
pub fn preflight(
    config: &aw_config::Configuration,
    target: &str,
    capabilities: &AdapterCapabilities,
) -> Result<Vec<AdmittedStep>, Error> {
    let spec = &config.as_value()["spec"];
    let agent = spec["agents"]
        .get(target)
        .ok_or(Error::Invalid("unknown configured Agent target"))?;
    require(
        agent["adapter"] == capabilities.adapter
            && matches!(
                capabilities.adapter.as_str(),
                "qoder" | "openclaw" | "hermes" | "qwenpaw"
            ),
        "Adapter evidence does not match the configured target",
    )?;
    require(
        !capabilities.version.trim().is_empty() && !capabilities.entrypoint.trim().is_empty(),
        "Adapter evidence requires a version and entrypoint",
    )?;
    for (event, effects) in &capabilities.events {
        require(
            supported_event(event) && effects.iter().all(|effect| supported_effect(event, effect)),
            "Adapter evidence exceeds the supported event or effect surface",
        )?;
    }
    require(
        spec["execution"]["guarantee"] == "native_hook",
        "only native_hook execution can be admitted",
    )?;
    let events = spec["events"]
        .as_object()
        .ok_or(Error::Invalid("configuration events must be an object"))?;
    let mut candidates = Vec::new();
    for (event_name, event) in events {
        if event["enabled"] != true {
            continue;
        }
        require(
            supported_event(event_name),
            "enabled event is not supported",
        )?;
        let effects = capabilities
            .events
            .get(event_name)
            .ok_or(Error::Invalid("Adapter evidence lacks an enabled event"))?;
        require(
            event.get("guard").is_none(),
            "event guards are not supported",
        )?;
        if let Some(selector) = event.get("match") {
            let tools = selector["tools"]
                .as_array()
                .ok_or(Error::Invalid("tool selector must be an array"))?;
            require(
                tools.len() == 1 && tools[0] == "*",
                "exact tool selectors are not supported",
            )?;
        }
        for step in event["steps"]
            .as_array()
            .ok_or(Error::Invalid("event steps must be an array"))?
        {
            if step["enabled"] == false {
                continue;
            }
            candidates.push(preflight_step(spec, event_name, step, effects)?);
        }
    }
    Ok(candidates)
}

fn preflight_step(
    spec: &Value,
    event: &str,
    step: &Value,
    adapter_effects: &[String],
) -> Result<AdmittedStep, Error> {
    let provider_name = string(&step["provider"])?;
    let provider = spec["providers"]
        .get(provider_name)
        .ok_or(Error::Invalid("unknown configured Provider"))?;
    require(
        provider["protocol"] == "aw-provider/v1alpha1"
            && provider["transport"]["type"] == "stdio"
            && provider["transport"]["location"] == "agent",
        "Provider transport or protocol is not supported",
    )?;
    let operation_name = string(&step["operation"])?;
    let mut effects = Vec::new();
    for effect in step["effects"]
        .as_array()
        .ok_or(Error::Invalid("step effects must be an array"))?
    {
        let effect = string(effect)?;
        require(
            supported_effect(event, effect)
                && adapter_effects.iter().any(|supported| supported == effect),
            "requested effect is not supported by the implementation and Adapter",
        )?;
        effects.push(effect.to_owned());
    }
    let on_error = string(&step["on_error"])?;
    require(
        on_error == "report"
            || (event == "tool.before"
                && on_error == "block"
                && adapter_effects.iter().any(|effect| effect == "block")),
        "failure action is not supported by the implementation and Adapter",
    )?;
    Ok(AdmittedStep {
        event: event.to_owned(),
        step_id: string(&step["id"])?.to_owned(),
        provider: provider_name.to_owned(),
        operation: operation_name.to_owned(),
        effects,
        on_error: on_error.to_owned(),
    })
}

fn check_provider(
    spec: &Value,
    step: &AdmittedStep,
    providers: &BTreeMap<String, ProviderEvidence>,
) -> Result<(), Error> {
    let evidence = providers
        .get(&step.provider)
        .ok_or(Error::Invalid("enabled step lacks Provider evidence"))?;
    require(
        evidence.configuration.as_value() == &spec["providers"][&step.provider]["config"],
        "Provider validation evidence does not match its private configuration",
    )?;
    let operation = evidence.description.as_value()["operations"]
        .as_array()
        .ok_or(Error::Invalid("Provider description lacks operations"))?
        .iter()
        .find(|operation| operation["name"] == step.operation)
        .ok_or(Error::Invalid(
            "Provider does not declare the requested operation",
        ))?;
    require(
        contains(&operation["events"], &step.event),
        "Provider operation does not support the requested event",
    )?;
    for effect in &step.effects {
        require(
            contains(&operation["effects"], effect),
            "requested effect is not supported by the Provider",
        )?;
    }
    Ok(())
}

fn supported_event(event: &str) -> bool {
    matches!(event, "tool.before" | "tool.after")
}

fn supported_effect(event: &str, effect: &str) -> bool {
    matches!(
        (event, effect),
        ("tool.before", "observe" | "block") | ("tool.after", "observe")
    )
}

fn contains(value: &Value, expected: &str) -> bool {
    value
        .as_array()
        .is_some_and(|values| values.iter().any(|value| value == expected))
}

fn string(value: &Value) -> Result<&str, Error> {
    value
        .as_str()
        .ok_or(Error::Invalid("configuration field must be a string"))
}

fn require(condition: bool, reason: &'static str) -> Result<(), Error> {
    if condition {
        Ok(())
    } else {
        Err(Error::Invalid(reason))
    }
}

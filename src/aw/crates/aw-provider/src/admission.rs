//! Pure capability admission for structured Providers and explicit native hook steps.

use crate::{Description, Error, ValidatedConfig};
use serde_json::Value;
use std::collections::BTreeMap;

mod selection;

pub use selection::{projected_out_steps, ProjectedOutStep};

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
    /// One of the adapters declared by the configuration contract.
    pub adapter: String,
    /// The actual Agent version whose capabilities were established by the caller.
    pub version: String,
    /// The validated interaction mode, such as a print or interactive entrypoint.
    pub entrypoint: String,
    /// Supported effects by event, restricted to this library's initial surface.
    pub events: BTreeMap<String, Vec<String>>,
    /// Additional failure actions supported by each event at this exact entrypoint.
    pub failure_actions: BTreeMap<String, Vec<String>>,
}

/// One configured step checked against the implementation and Adapter.
///
/// Structured steps returned by [`admit`] have also passed Provider admission;
/// native steps require no Provider handshake. Steps returned by [`preflight`]
/// remain candidates, and public fields carry no proof
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
    /// Structured Provider evaluation or uninterpreted native hook execution.
    pub execution: StepExecution,
    /// The Host action required when Provider execution fails.
    pub on_error: String,
    /// Event-level tool selectors; intersected with the step selectors.
    pub event_tools: Vec<String>,
    /// Step-level tool selectors; omitted configuration admits all tools.
    pub step_tools: Vec<String>,
}

impl AdmittedStep {
    /// Match a validated normalized event against both configured tool selectors.
    ///
    /// Standard selectors compare `tool.name`; exact native selectors compare the
    /// Adapter ID and retained `tool.native_name`. No aliases or globs are inferred.
    /// A mismatch still consumes the step and participates in ordered completion.
    pub fn matches_tool(&self, event: &Value) -> bool {
        let matches = |selectors: &[String]| {
            selectors.iter().any(|selector| {
                if selector == "*" {
                    return true;
                }
                if let Some(native) = selector.strip_prefix("native:") {
                    return native.split_once(':').is_some_and(|(adapter, name)| {
                        event["agent"]["adapter"] == adapter && event["tool"]["native_name"] == name
                    });
                }
                event["tool"]["name"] == *selector
            })
        };
        matches(&self.event_tools) && matches(&self.step_tools)
    }
}

/// Execution contract selected by the configured protocol or its documented default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepExecution {
    /// Invoke a boolean command once; the Host projects configured effects.
    Hook {
        /// Effects to request when the command returns true.
        effects: Vec<String>,
        /// Optional configured explanation for a match.
        reason_code: Option<String>,
    },
    /// Validate requests and candidate effects with aw-provider/v1alpha1.
    Provider {
        /// Operation declared by the Provider.
        operation: String,
        /// Requested effects checked against Provider and Adapter evidence.
        effects: Vec<String>,
    },
    /// Preserve raw callback input, output and process status for the Adapter.
    Native,
}

impl StepExecution {
    /// Whether this structured step requests a model-visible text replacement.
    pub fn replaces_result(&self) -> bool {
        matches!(self, Self::Provider { effects, .. } if effects.iter().any(|effect| effect == "replace_result"))
    }
}

/// Admits enabled tool steps using explicit Provider and trusted Adapter evidence.
///
/// Native commands, disabled steps and steps that cannot select this Adapter
/// require no Provider discovery. Applicable enabled events must be supported
/// even when `required` is false or their step list is empty.
/// Step order within each event is preserved; the result does not order distinct
/// events or choose serial versus parallel execution.
///
/// # Errors
/// Rejects missing or mismatched evidence, unsupported events or effects, guards,
/// and unsupported failure actions. Success establishes
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
/// skipped, as are selector intersections proven impossible for this Adapter.
/// Applicable enabled events require support even when optional or empty. Step
/// order within each event is preserved without choosing execution scheduling.
///
/// Returned steps are candidates, not Provider-admitted steps. Hosts must still
/// call [`admit`] with discovered evidence for structured Providers before execution.
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
        // Configuration is already validated against the catalog-aligned schema.
        agent["adapter"] == capabilities.adapter,
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
    for (event, actions) in &capabilities.failure_actions {
        require(
            event == "tool.after"
                && capabilities.events.contains_key(event)
                && actions.iter().all(|action| action == "withhold_result"),
            "Adapter evidence exceeds the supported failure action surface",
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
        let event_tools = tool_selectors(event)?;
        if !selection::overlap(&event_tools, &["*".into()], &capabilities.adapter) {
            continue;
        }
        let mut enabled = Vec::new();
        let mut has_enabled_steps = false;
        for step in event["steps"]
            .as_array()
            .ok_or(Error::Invalid("event steps must be an array"))?
        {
            if step["enabled"] == false {
                continue;
            }
            has_enabled_steps = true;
            if selection::overlap(&event_tools, &tool_selectors(step)?, &capabilities.adapter) {
                enabled.push(step);
            }
        }
        if has_enabled_steps && enabled.is_empty() {
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
        if let Some(index) = enabled.iter().position(|step| {
            step["effects"]
                .as_array()
                .is_some_and(|effects| effects.iter().any(|effect| effect == "replace_result"))
        }) {
            require(
                index + 1 == enabled.len(),
                "result replacement must be the last applicable enabled event step",
            )?;
        }
        for step in enabled {
            candidates.push(preflight_step(
                spec,
                event_name,
                step,
                effects,
                capabilities,
            )?);
        }
    }
    Ok(candidates)
}

fn preflight_step(
    spec: &Value,
    event: &str,
    step: &Value,
    adapter_effects: &[String],
    capabilities: &AdapterCapabilities,
) -> Result<AdmittedStep, Error> {
    let provider_name = string(&step["provider"])?;
    let provider = spec["providers"]
        .get(provider_name)
        .ok_or(Error::Invalid("unknown configured Provider"))?;
    let protocol = provider["protocol"]
        .as_str()
        .unwrap_or(aw_config::HOOK_PROTOCOL);
    require(
        matches!(
            protocol,
            "aw-provider/v1alpha1" | "native-hook/v1alpha1" | aw_config::HOOK_PROTOCOL
        ) && provider["transport"]["type"] == "stdio"
            && provider["transport"]["location"] == "agent",
        "Provider transport or protocol is not supported",
    )?;
    let execution = if protocol == "native-hook/v1alpha1" {
        require(step.get("native").is_some(), "native hook step is required")?;
        StepExecution::Native
    } else {
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
        if protocol == aw_config::HOOK_PROTOCOL {
            require(
                !effects.iter().any(|effect| effect == "replace_result"),
                "boolean Hooks cannot replace results",
            )?;
            StepExecution::Hook {
                effects,
                reason_code: step["reason_code"].as_str().map(str::to_owned),
            }
        } else {
            StepExecution::Provider {
                operation: string(&step["operation"])?.to_owned(),
                effects,
            }
        }
    };
    let on_error = string(&step["on_error"])?;
    require(
        on_error == "report"
            || (event == "tool.before"
                && on_error == "block"
                && adapter_effects.iter().any(|effect| effect == "block"))
            || (event == "tool.after"
                && on_error == "withhold_result"
                && capabilities
                    .failure_actions
                    .get(event)
                    .is_some_and(|actions| {
                        actions.iter().any(|action| action == "withhold_result")
                    })),
        "failure action is not supported by the implementation and Adapter",
    )?;
    Ok(AdmittedStep {
        event: event.to_owned(),
        step_id: string(&step["id"])?.to_owned(),
        provider: provider_name.to_owned(),
        execution,
        on_error: on_error.to_owned(),
        event_tools: tool_selectors(&spec["events"][event])?,
        step_tools: tool_selectors(step)?,
    })
}

fn check_provider(
    spec: &Value,
    step: &AdmittedStep,
    providers: &BTreeMap<String, ProviderEvidence>,
) -> Result<(), Error> {
    let StepExecution::Provider {
        operation: requested,
        effects,
    } = &step.execution
    else {
        return Ok(());
    };
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
        .find(|operation| operation["name"] == *requested)
        .ok_or(Error::Invalid(
            "Provider does not declare the requested operation",
        ))?;
    require(
        contains(&operation["events"], &step.event),
        "Provider operation does not support the requested event",
    )?;
    for effect in effects {
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
        ("tool.before", "observe" | "block") | ("tool.after", "observe" | "replace_result")
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

fn tool_selectors(value: &Value) -> Result<Vec<String>, Error> {
    match value["match"].get("tools") {
        None => Ok(vec!["*".into()]),
        Some(tools) => tools
            .as_array()
            .ok_or(Error::Invalid("tool selectors must be an array"))?
            .iter()
            .map(|tool| string(tool).map(str::to_owned))
            .collect(),
    }
}

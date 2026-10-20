//! Registered native dialects and the runtime capabilities admitted by their launchers.

use super::Adapter;
use aw_provider::admission::AdapterCapabilities as Capabilities;

pub(crate) struct Profile {
    pub name: &'static str,
    pub adapter: &'static dyn Adapter,
    pub events: &'static [(&'static str, &'static [&'static str])],
}

const TOOL_EVENTS: &[(&str, &[&str])] = &[
    ("tool.before", &["observe", "block"]),
    ("tool.after", &["observe", "replace_result"]),
];

impl Profile {
    pub fn capabilities(&self, version: &str, entrypoint: &str) -> Capabilities {
        Capabilities {
            adapter: self.name.into(),
            version: version.into(),
            entrypoint: entrypoint.into(),
            events: self
                .events
                .iter()
                .map(|(event, effects)| {
                    (
                        (*event).into(),
                        effects.iter().map(|effect| (*effect).into()).collect(),
                    )
                })
                .collect(),
        }
    }
}

pub(crate) const QODER: Profile = Profile {
    name: "qoder",
    adapter: &super::super::qoder::Qoder,
    events: TOOL_EVENTS,
};

pub(crate) const OPENCLAW: Profile = Profile {
    name: "openclaw",
    adapter: &super::super::adapters::openclaw::OpenClaw,
    events: TOOL_EVENTS,
};

pub(crate) const HERMES: Profile = Profile {
    name: "hermes",
    adapter: &super::super::adapters::hermes::Hermes,
    events: TOOL_EVENTS,
};

pub(crate) const QWENPAW: Profile = Profile {
    name: "qwenpaw",
    adapter: &super::super::adapters::qwenpaw::QwenPaw,
    events: TOOL_EVENTS,
};

pub(crate) const PI: Profile = Profile {
    name: "pi",
    adapter: &super::super::adapters::pi::Pi,
    events: TOOL_EVENTS,
};

pub(crate) const CLAUDE_SDK: Profile = Profile {
    name: "claude-sdk",
    adapter: &super::super::adapters::claude_sdk::ClaudeSdk,
    events: TOOL_EVENTS,
};

pub(crate) const AGENTSCOPE: Profile = Profile {
    name: "agentscope",
    adapter: &super::super::adapters::agentscope::AgentScope,
    events: TOOL_EVENTS,
};

pub(crate) const CODEX: Profile = Profile {
    name: "codex",
    adapter: &super::super::adapters::codex::Codex,
    events: &[
        ("tool.before", &["observe", "block"]),
        ("tool.after", &["observe"]),
    ],
};

pub(crate) const PROFILES: &[Profile] = &[
    QODER, OPENCLAW, HERMES, QWENPAW, PI, CLAUDE_SDK, AGENTSCOPE, CODEX,
];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn unsupported_capabilities_are_rejected_before_provider_discovery() {
        use aw_provider::admission::{preflight, AdapterCapabilities};
        use serde_json::json;
        let validator = aw_config::Validator::new().unwrap();
        let baseline = validator
            .parse(include_bytes!(
                "../../../aw-config/examples/aw.minimal.yaml"
            ))
            .unwrap();
        for profile in PROFILES {
            let caps: AdapterCapabilities = profile.capabilities("fixture", "fixture");
            for (event, effects) in TOOL_EVENTS {
                for effect in *effects {
                    let mut document = baseline.as_value().clone();
                    document["spec"]["agents"] = json!({"target": {
                        "adapter": profile.name, "argv": ["/must-not-start/agent"]
                    }});
                    document["spec"]["providers"] = json!({"policy": {
                        "protocol":"aw-provider/v1alpha1", "config":{},
                        "transport":{"type":"stdio", "location":"agent", "argv":["/must-not-start/provider"]},
                        "timeout_ms":100, "max_output_bytes":4096
                    }});
                    document["spec"]["events"] = json!({(*event): {"enabled":true, "required":false,
                        "match":{"tools":[format!("native:{}:tool", profile.name)]},
                        "steps":[{"id":"test", "provider":"policy", "operation":"test",
                            "effects":[effect], "on_error":"report"}]}});
                    let config = validator
                        .parse(&serde_json::to_vec(&document).unwrap())
                        .unwrap();
                    // Native selector names are configuration vocabulary; exact
                    // selectors still fail runtime admission for every adapter.
                    assert!(preflight(&config, "target", &caps).is_err());
                    document["spec"]["events"][event]
                        .as_object_mut()
                        .unwrap()
                        .remove("match");
                    let config = validator
                        .parse(&serde_json::to_vec(&document).unwrap())
                        .unwrap();
                    let supported = caps
                        .events
                        .get(*event)
                        .is_some_and(|v| v.iter().any(|v| v == effect));
                    assert_eq!(
                        preflight(&config, "target", &caps).is_ok(),
                        supported,
                        "{}/{event}/{effect}",
                        profile.name
                    );
                    let mut partial = caps.clone();
                    if let Some(values) = partial.events.get_mut(*event) {
                        values.retain(|value| value != effect);
                    }
                    assert!(preflight(&config, "target", &partial).is_err());
                    partial.events.remove(*event);
                    // Optional and empty enabled events must also fail admission.
                    document["spec"]["events"][event]["steps"] = json!([]);
                    let config = validator
                        .parse(&serde_json::to_vec(&document).unwrap())
                        .unwrap();
                    assert!(preflight(&config, "target", &partial).is_err());
                }
            }
        }
    }

    #[test]
    fn registered_capabilities_match_the_regression_matrix_and_config_schema() {
        let matrix: Value =
            serde_json::from_str(include_str!("../../../../tests/regression-matrix.json")).unwrap();
        let declared: BTreeMap<_, BTreeMap<_, BTreeSet<_>>> = matrix["agents"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(agent, row)| {
                (
                    agent.as_str(),
                    row["events"]
                        .as_object()
                        .unwrap()
                        .iter()
                        .map(|(event, effects)| {
                            (
                                event.as_str(),
                                effects
                                    .as_object()
                                    .unwrap()
                                    .keys()
                                    .map(String::as_str)
                                    .collect(),
                            )
                        })
                        .collect(),
                )
            })
            .collect();
        let actual: BTreeMap<_, BTreeMap<_, BTreeSet<_>>> = PROFILES
            .iter()
            .map(|profile| {
                (
                    profile.name,
                    profile
                        .events
                        .iter()
                        .map(|(event, effects)| (*event, effects.iter().copied().collect()))
                        .collect(),
                )
            })
            .collect();
        assert_eq!(
            actual.len(),
            PROFILES.len(),
            "duplicate adapter registration"
        );
        assert_eq!(
            actual, declared,
            "register each agent/event/effect with runnable regressions"
        );
        let schema: Value = serde_json::from_str(include_str!(
            "../../../aw-config/schemas/configuration-v1alpha1.schema.json"
        ))
        .unwrap();
        let names: BTreeSet<_> = schema["$defs"]["agent"]["properties"]["adapter"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(names, actual.keys().copied().collect());
        let wire: Value = serde_json::from_str(include_str!(
            "../../../aw-provider/schemas/request-v1alpha1.schema.json"
        ))
        .unwrap();
        let wire_names: BTreeSet<_> = wire["oneOf"][2]["properties"]["event"]["properties"]
            ["agent"]["properties"]["adapter"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect();
        assert_eq!(
            names, wire_names,
            "Provider wire schema must admit registered adapters"
        );
    }
}

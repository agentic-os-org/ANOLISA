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
            failure_actions: Default::default(),
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

pub(crate) const QODER_SDK: Profile = Profile {
    name: "qoder-sdk",
    adapter: &super::super::adapters::qoder_sdk::QoderSdk,
    events: &[
        ("tool.before", &["observe", "block"]),
        ("tool.after", &["observe"]),
    ],
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

pub(crate) const PROFILES: &[Profile] = &[
    QODER, QODER_SDK, OPENCLAW, HERMES, QWENPAW, PI, CLAUDE_SDK, AGENTSCOPE,
];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::collections::{BTreeMap, BTreeSet};

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
    }
}

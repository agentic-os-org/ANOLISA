//! Registered native dialects and the runtime capabilities admitted by their launchers.

use super::Adapter;
use aw_contracts::source::{project_capability_cells, CapabilityCell, SourceCapability};
use aw_core::{
    admission::AdapterCapabilities as Capabilities,
    resolver::{source_from_adapter, ResolveError},
};
use std::collections::BTreeMap;

/// Placeholder contract version used when projecting static default profiles.
const CATALOG_VERSION: &str = "catalog";
/// Placeholder entrypoint used when projecting static default profiles.
const CATALOG_ENTRYPOINT: &str = "default-profile";

pub(crate) struct Profile {
    pub name: &'static str,
    pub adapter: &'static dyn Adapter,
    pub events: &'static [(&'static str, &'static [&'static str])],
}

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

    pub(crate) fn source_capability(
        &self,
        version: &str,
        entrypoint: &str,
    ) -> Result<SourceCapability, ResolveError> {
        source_from_adapter(&self.capabilities(version, entrypoint))
    }
}

/// Projects every registered default profile onto the 16 semantic Hooks.
///
/// The returned cells reflect only the static default profiles declared in the
/// Adapter catalog, not conditionally prepared deployment profiles or native
/// adoption evidence. Each profile is converted through
/// [`aw_core::resolver::source_from_adapter`] using fixed catalog placeholders
/// (`version = "catalog"`, `entrypoint = "default-profile"`) and projected with
/// [`aw_contracts::source::project_capability_cells`].
///
/// # Errors
/// Returns [`ResolveError`] if a registered profile fails `SourceCapability`
/// conversion or 16-Hook capability-cell projection.
pub fn coverage_cells(
) -> Result<BTreeMap<&'static str, BTreeMap<String, CapabilityCell>>, ResolveError> {
    let mut coverage = BTreeMap::new();
    for profile in PROFILES {
        let source = profile.source_capability(CATALOG_VERSION, CATALOG_ENTRYPOINT)?;
        let cells = project_capability_cells(profile.name, std::slice::from_ref(&source))?;
        coverage.insert(profile.name, cells);
    }
    Ok(coverage)
}

pub(crate) const SDK: Profile = Profile {
    name: "sdk",
    adapter: &super::super::adapters::sdk::Sdk,
    events: &[
        ("session.start", &["observe"]),
        ("turn.stop", &["observe", "block"]),
        ("session.end", &["observe"]),
        ("input.submit", &["observe", "block"]),
        ("tool.before", &["observe", "block", "replace_input"]),
        ("tool.after", &["observe", "replace_result"]),
    ],
};

pub(crate) const QODER: Profile = Profile {
    name: "qoder",
    adapter: &super::super::qoder::Qoder,
    events: &[
        ("session.start", &["observe"]),
        ("session.end", &["observe"]),
        ("input.submit", &["observe", "block"]),
        ("tool.before", &["observe", "block", "replace_input"]),
        ("tool.after", &["observe", "replace_result"]),
    ],
};

pub(crate) const QODER_SDK: Profile = Profile {
    name: "qoder-sdk",
    adapter: &super::super::adapters::qoder_sdk::QoderSdk,
    events: &[
        ("session.start", &["observe"]),
        ("session.end", &["observe"]),
        ("input.submit", &["observe", "block"]),
        ("tool.before", &["observe", "block", "replace_input"]),
        ("tool.after", &["observe", "replace_result"]),
    ],
};

pub(crate) const OPENCLAW: Profile = Profile {
    name: "openclaw",
    adapter: &super::super::adapters::openclaw::OpenClaw,
    events: &[
        ("session.start", &["observe"]),
        ("tool.before", &["observe", "block"]),
        ("tool.after", &["observe", "replace_result"]),
        ("turn.stop", &["observe"]),
        ("session.end", &["observe"]),
    ],
};

pub(crate) const DEEPSEEK: Profile = Profile {
    name: "deepseek-harness",
    adapter: &super::super::adapters::deepseek::DeepSeek,
    events: &[
        ("session.start", &["observe"]),
        ("turn.stop", &["observe"]),
        ("input.submit", &["observe", "block"]),
        ("tool.before", &["observe", "block"]),
        ("tool.after", &["observe", "replace_result"]),
        ("security.violation", &["observe", "block"]),
    ],
};

pub(crate) const HERMES: Profile = Profile {
    name: "hermes",
    adapter: &super::super::adapters::hermes::Hermes,
    events: &[
        ("session.start", &["observe"]),
        ("turn.stop", &["observe"]),
        ("session.end", &["observe"]),
        ("input.submit", &["observe", "block"]),
        ("tool.before", &["observe", "block", "replace_input"]),
        ("tool.after", &["observe", "replace_result"]),
    ],
};

pub(crate) const QWENPAW: Profile = Profile {
    name: "qwenpaw",
    adapter: &super::super::adapters::qwenpaw::QwenPaw,
    events: &[
        ("turn.stop", &["observe"]),
        ("input.submit", &["observe", "block"]),
        ("tool.before", &["observe", "block", "replace_input"]),
        ("tool.after", &["observe", "replace_result"]),
    ],
};

pub(crate) const PI: Profile = Profile {
    name: "pi",
    adapter: &super::super::adapters::pi::Pi,
    events: &[
        ("session.start", &["observe"]),
        ("turn.stop", &["observe"]),
        ("session.end", &["observe"]),
        ("input.submit", &["observe", "block"]),
        ("tool.before", &["observe", "block", "replace_input"]),
        ("tool.after", &["observe", "replace_result"]),
    ],
};

pub(crate) const CLAUDE_SDK: Profile = Profile {
    name: "claude-sdk",
    adapter: &super::super::adapters::claude_sdk::ClaudeSdk,
    events: &[
        ("session.start", &["observe"]),
        ("session.end", &["observe"]),
        ("turn.stop", &["observe", "block"]),
        ("input.submit", &["observe", "block"]),
        ("tool.before", &["observe", "block", "replace_input"]),
        ("tool.after", &["observe", "replace_result"]),
    ],
};

pub(crate) const AGENTSCOPE: Profile = Profile {
    name: "agentscope",
    adapter: &super::super::adapters::agentscope::AgentScope,
    events: &[
        ("turn.stop", &["observe"]),
        ("input.submit", &["observe", "block"]),
        ("tool.before", &["observe", "block", "replace_input"]),
        ("tool.after", &["observe", "replace_result"]),
    ],
};

pub(crate) const CODEX: Profile = Profile {
    name: "codex",
    adapter: &super::super::adapters::codex::Codex,
    events: &[
        ("session.start", &["observe"]),
        ("turn.stop", &["observe"]),
        ("input.submit", &["observe", "block"]),
        ("tool.before", &["observe", "block"]),
        ("tool.after", &["observe", "replace_result"]),
    ],
};

pub(crate) const PROFILES: &[Profile] = &[
    SDK, QODER, QODER_SDK, OPENCLAW, DEEPSEEK, HERMES, QWENPAW, PI, CLAUDE_SDK, AGENTSCOPE, CODEX,
];

#[cfg(test)]
mod tests {
    use super::*;
    use aw_contracts::source::{CapabilityStatus, HookAuthority, SourceKind, HOOK_EVENTS};
    use serde_json::Value;
    use std::collections::{BTreeMap, BTreeSet};

    fn regression_matrix_events(matrix: &Value) -> BTreeMap<&str, BTreeMap<&str, BTreeSet<&str>>> {
        matrix["agents"]
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
            .collect()
    }

    #[test]
    fn registered_capabilities_match_the_regression_matrix_and_config_schema() {
        let matrix: Value =
            serde_json::from_str(include_str!("../../../../tests/regression-matrix.json")).unwrap();
        let declared = regression_matrix_events(&matrix);
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

    #[test]
    fn catalog_coverage_cells_match_the_regression_matrix_projection() {
        let matrix: Value =
            serde_json::from_str(include_str!("../../../../tests/regression-matrix.json")).unwrap();
        let declared = regression_matrix_events(&matrix);
        let coverage = coverage_cells().unwrap();

        let coverage_agents: BTreeSet<_> = coverage.keys().copied().collect();
        let matrix_agents: BTreeSet<_> = declared.keys().copied().collect();
        assert_eq!(coverage_agents, matrix_agents);
        assert_eq!(coverage.len(), PROFILES.len());

        let expected_events: BTreeSet<_> = HOOK_EVENTS.iter().copied().collect();
        for profile in PROFILES {
            let row = coverage
                .get(profile.name)
                .expect("every registered profile must be projected");
            assert_eq!(row.len(), HOOK_EVENTS.len());
            assert_eq!(
                row.keys().map(String::as_str).collect::<BTreeSet<_>>(),
                expected_events
            );
            let declared_events = &declared[profile.name];
            for event in HOOK_EVENTS {
                let cell = &row[event];
                assert_eq!(cell.adapter, profile.name);
                assert_eq!(cell.event, event);
                if let Some(expected_effects) = declared_events.get(event) {
                    assert_eq!(cell.status, CapabilityStatus::Supported);
                    assert_eq!(cell.sources, vec![SourceKind::Native]);
                    assert_eq!(cell.mode, Some(HookAuthority::Control));
                    let actual_effects: BTreeSet<_> =
                        cell.effects.iter().map(|effect| effect.as_str()).collect();
                    assert_eq!(&actual_effects, expected_effects);
                    assert!(cell.failure_actions.is_empty());
                } else {
                    assert_eq!(cell.status, CapabilityStatus::Unsupported);
                    assert!(cell.sources.is_empty());
                    assert!(cell.mode.is_none());
                    assert!(cell.effects.is_empty());
                    assert!(cell.failure_actions.is_empty());
                }
            }
        }
    }
}

//! Offline wiring profiles for the configuration-driven public SDK integrations.

use crate::{adapter::catalog, Result};
use aw_core::admission::AdapterCapabilities;

/// Declared wiring used to compile a deployment descriptor, before installation.
pub struct DeploymentProfile {
    /// Capability ceiling of the selected SDK integration.
    pub capabilities: AdapterCapabilities,
    /// Whether the native integration requires AW even for optional policy steps.
    pub requires_service: bool,
    /// Whether descriptors for this profile emit the optional `observe_session_start` hint.
    pub session_start_hint: bool,
}

struct ProfileDefinition {
    name: &'static str,
    build: fn() -> DeploymentProfile,
}

const PROFILES: &[ProfileDefinition] = &[
    ProfileDefinition {
        name: "qoder-public-hooks",
        build: || {
            let mut capabilities = catalog::QODER_SDK.capabilities("public-hooks/v1", "sdk-worker");
            capabilities.events.remove("session.end");
            observe_or_block_before(&mut capabilities);
            DeploymentProfile {
                capabilities,
                requires_service: false,
                session_start_hint: true,
            }
        },
    },
    ProfileDefinition {
        name: "qwenpaw-installed-native-hooks",
        build: || {
            let mut capabilities =
                catalog::QWENPAW.capabilities("installed-native-hooks/v1", "app");
            capabilities
                .failure_actions
                .insert("tool.after".into(), vec!["withhold_result".into()]);
            observe_or_block_before(&mut capabilities);
            DeploymentProfile {
                capabilities,
                requires_service: true,
                session_start_hint: false,
            }
        },
    },
    ProfileDefinition {
        name: "deepseek-cordis-hooks",
        build: || {
            let mut capabilities = catalog::DEEPSEEK.capabilities("cordis-hooks/v1", "cordis");
            capabilities.events.retain(|event, _| {
                matches!(
                    event.as_str(),
                    "input.submit" | "tool.before" | "tool.after"
                )
            });
            capabilities
                .failure_actions
                .insert("tool.after".into(), vec!["withhold_result".into()]);
            DeploymentProfile {
                capabilities,
                requires_service: true,
                session_start_hint: false,
            }
        },
    },
    // Cooperative resident workers reuse the managed bridges and their fail-closed
    // cleanup, so AW must be available for every admitted Task.
    ProfileDefinition {
        name: "claude-resident-hooks",
        build: || DeploymentProfile {
            capabilities: catalog::CLAUDE_SDK.capabilities("resident-hooks/v1", "sdk-worker"),
            requires_service: true,
            session_start_hint: false,
        },
    },
    ProfileDefinition {
        name: "pi-worker-hooks",
        build: || {
            // The ceiling includes strict output; an installed release outside the
            // verified range registers without it and the Service rejects that policy.
            let mut capabilities = catalog::PI.capabilities("worker-hooks/v1", "sdk");
            capabilities
                .failure_actions
                .insert("tool.after".into(), vec!["withhold_result".into()]);
            DeploymentProfile {
                capabilities,
                requires_service: true,
                session_start_hint: false,
            }
        },
    },
];

/// Returns whether any deployment profile for `adapter` requests the `observe_session_start` hint.
pub fn emits_session_start_hint(adapter: &str) -> bool {
    PROFILES.iter().any(|definition| {
        let profile = (definition.build)();
        profile.session_start_hint && profile.capabilities.adapter == adapter
    })
}

/// These SDK bindings capture the public before boundary without input rewriting.
fn observe_or_block_before(capabilities: &mut AdapterCapabilities) {
    capabilities
        .events
        .insert("tool.before".into(), vec!["observe".into(), "block".into()]);
}

/// Names accepted by [`profile`], in presentation order.
pub fn profile_names() -> impl Iterator<Item = &'static str> {
    PROFILES.iter().map(|profile| profile.name)
}

/// Selects an existing SDK wiring contract without probing or installing an Agent.
///
/// These declarations describe the public SDK integrations. They do not certify
/// installed callbacks, the installed native release or actual effect adoption.
///
/// # Errors
/// Rejects unknown profiles instead of using a broader Adapter default.
pub fn profile(name: &str, input_enabled: bool) -> Result<DeploymentProfile> {
    let definition = PROFILES
        .iter()
        .find(|profile| profile.name == name)
        .ok_or_else(|| format!("unsupported deployment profile: {name}"))?;
    let mut profile = (definition.build)();
    if !input_enabled {
        profile.capabilities.events.remove("input.submit");
    }
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_profiles_preserve_their_native_effect_boundaries() {
        let qoder = profile("qoder-public-hooks", true).unwrap();
        assert!(!qoder.requires_service);
        assert!(qoder.session_start_hint);
        assert!(emits_session_start_hint("qoder-sdk"));
        assert_eq!(qoder.capabilities.version, "public-hooks/v1");
        assert_eq!(
            qoder.capabilities.events["tool.after"],
            ["observe", "replace_result"]
        );
        assert!(!qoder.capabilities.events.contains_key("session.end"));
        assert!(qoder.capabilities.failure_actions.is_empty());
        let qwenpaw = profile("qwenpaw-installed-native-hooks", true).unwrap();
        assert!(qwenpaw.requires_service);
        assert!(!qwenpaw.session_start_hint);
        assert!(!emits_session_start_hint("qwenpaw"));
        assert_eq!(qwenpaw.capabilities.version, "installed-native-hooks/v1");
        assert_eq!(
            qwenpaw.capabilities.events["tool.after"],
            ["observe", "replace_result"]
        );
        assert_eq!(
            qwenpaw.capabilities.failure_actions["tool.after"],
            ["withhold_result"]
        );
        for name in ["qoder-public-hooks", "qwenpaw-installed-native-hooks"] {
            let enabled = profile(name, true).unwrap().capabilities;
            assert_eq!(enabled.events["tool.before"], ["observe", "block"]);
        }
        for name in profile_names() {
            let enabled = profile(name, true).unwrap().capabilities;
            assert_eq!(enabled.events["input.submit"], ["observe", "block"]);
            assert!(!profile(name, false)
                .unwrap()
                .capabilities
                .events
                .contains_key("input.submit"));
            assert!(!enabled.events.contains_key("security.violation"));
        }
        assert!(profile("qoder-sdk", true).is_err());
    }

    #[test]
    fn resident_worker_profiles_require_the_service_and_keep_rewrites() {
        for (name, adapter, version, entrypoint, withheld) in [
            (
                "claude-resident-hooks",
                "claude-sdk",
                "resident-hooks/v1",
                "sdk-worker",
                false,
            ),
            ("pi-worker-hooks", "pi", "worker-hooks/v1", "sdk", true),
        ] {
            let profile = profile(name, true).unwrap();
            assert!(profile.requires_service, "{name}");
            assert!(!profile.session_start_hint, "{name}");
            assert!(!emits_session_start_hint(adapter), "{adapter}");
            let capabilities = profile.capabilities;
            assert_eq!(
                (
                    capabilities.adapter.as_str(),
                    capabilities.version.as_str(),
                    capabilities.entrypoint.as_str()
                ),
                (adapter, version, entrypoint)
            );
            assert_eq!(
                capabilities.events["tool.before"],
                ["observe", "block", "replace_input"]
            );
            assert_eq!(
                capabilities.events["tool.after"],
                ["observe", "replace_result"]
            );
            assert_eq!(
                capabilities.failure_actions.contains_key("tool.after"),
                withheld,
                "{name}"
            );
        }
    }
}

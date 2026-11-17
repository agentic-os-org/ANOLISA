//! Offline wiring profiles for the configuration-driven public SDK integrations.

use crate::{adapter::catalog, Result};
use aw_core::admission::AdapterCapabilities;

/// Declared wiring used to compile a deployment descriptor, before installation.
pub struct DeploymentProfile {
    /// Capability ceiling of the selected SDK integration.
    pub capabilities: AdapterCapabilities,
    /// Whether the native integration requires AW even for optional policy steps.
    pub requires_service: bool,
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
            capabilities
                .events
                .insert("tool.after".into(), vec!["observe".into()]);
            DeploymentProfile {
                capabilities,
                requires_service: false,
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
            DeploymentProfile {
                capabilities,
                requires_service: true,
            }
        },
    },
];

/// Names accepted by [`profile`], in presentation order.
pub fn profile_names() -> impl Iterator<Item = &'static str> {
    PROFILES.iter().map(|profile| profile.name)
}

/// Selects an existing SDK wiring contract without probing or installing an Agent.
///
/// These declarations describe the public Qoder bridge and configured QwenPaw
/// plugin. They do not certify installed callbacks or actual effect adoption.
///
/// # Errors
/// Rejects unknown profiles instead of using a broader Adapter default.
pub fn profile(name: &str, input_enabled: bool) -> Result<DeploymentProfile> {
    let definition = PROFILES
        .iter()
        .find(|profile| profile.name == name)
        .ok_or_else(|| format!("unsupported deployment profile: {name}"))?;
    let mut profile = (definition.build)();
    // These SDK bindings capture the public before boundary without input rewriting.
    profile
        .capabilities
        .events
        .insert("tool.before".into(), vec!["observe".into(), "block".into()]);
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
        assert_eq!(qoder.capabilities.version, "public-hooks/v1");
        assert_eq!(qoder.capabilities.events["tool.after"], ["observe"]);
        assert!(qoder.capabilities.failure_actions.is_empty());
        let qwenpaw = profile("qwenpaw-installed-native-hooks", true).unwrap();
        assert!(qwenpaw.requires_service);
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
}

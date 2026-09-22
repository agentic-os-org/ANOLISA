//! Feature flag storage and resolution.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// In-memory representation of feature flags for all components.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct FeatureStore {
    /// Per-component flag table keyed by stable component name.
    #[serde(default)]
    pub component: HashMap<String, ComponentConfig>,
}

/// Feature configuration for one component.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentConfig {
    /// Component-level master switch; missing values default to enabled
    /// so older configs remain active after schema growth.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Per-feature overrides keyed by feature name.
    #[serde(default)]
    pub features: HashMap<String, bool>,
}

fn default_true() -> bool {
    true
}

impl FeatureStore {
    /// Load feature store from a TOML file.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be read or parsed as the feature-store
    /// TOML schema.
    pub fn load(path: &Path) -> Result<Self, FeatureStoreError> {
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(FeatureStoreError::Io(e.to_string())),
        };
        toml::from_str(&content).map_err(|e| FeatureStoreError::Parse(e.to_string()))
    }

    /// Save feature store to a TOML file.
    ///
    /// # Errors
    ///
    /// Fails when the parent directory cannot be created, the store
    /// cannot be encoded, or the file cannot be written.
    pub fn save(&self, path: &Path) -> Result<(), FeatureStoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| FeatureStoreError::Io(e.to_string()))?;
        }
        let content =
            toml::to_string_pretty(self).map_err(|e| FeatureStoreError::Parse(e.to_string()))?;
        std::fs::write(path, content).map_err(|e| FeatureStoreError::Io(e.to_string()))
    }

    /// Check if a feature is enabled for a component.
    pub fn is_enabled(&self, component: &str, feature: &str) -> bool {
        self.component
            .get(component)
            .filter(|c| c.enabled)
            .and_then(|c| c.features.get(feature))
            .copied()
            .unwrap_or(false)
    }

    /// Enable a feature.
    pub fn enable(&mut self, component: &str, feature: &str) {
        self.component
            .entry(component.to_string())
            .or_insert_with(|| ComponentConfig {
                enabled: true,
                features: HashMap::new(),
            })
            .features
            .insert(feature.to_string(), true);
    }

    /// Disable a feature.
    pub fn disable(&mut self, component: &str, feature: &str) {
        if let Some(comp) = self.component.get_mut(component) {
            comp.features.insert(feature.to_string(), false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_component_disables_all_feature_overrides() {
        let mut store = FeatureStore::default();
        store.enable("agentsight", "ebpf_tracing");
        assert!(store.is_enabled("agentsight", "ebpf_tracing"));

        store.component.get_mut("agentsight").unwrap().enabled = false;

        assert!(!store.is_enabled("agentsight", "ebpf_tracing"));
    }

    #[test]
    fn missing_feature_store_file_loads_as_default() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FeatureStore::load(&tmp.path().join("missing.toml")).unwrap();

        assert!(store.component.is_empty());
    }
}

/// Errors raised while loading or saving [`FeatureStore`].
#[derive(Debug, thiserror::Error)]
pub enum FeatureStoreError {
    /// Filesystem access failed.
    #[error("I/O error: {0}")]
    Io(String),
    /// TOML parsing or encoding failed.
    #[error("parse error: {0}")]
    Parse(String),
}

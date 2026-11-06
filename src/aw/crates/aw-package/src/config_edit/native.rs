//! Edit AW-owned references without opening or rewriting any native source.

use super::ConfigurationEditor;
use crate::{require, Error, Result};
use serde_json::{json, Value};

impl ConfigurationEditor {
    /// Append one native source descriptor, preserving the existing sequence.
    ///
    /// Identical IDs and definitions are no-ops. Native objects live in their
    /// original storage and never share this descriptor's namespace.
    ///
    /// # Errors
    /// Rejects absent targets, conflicting IDs and invalid AW candidates.
    pub fn add_native_source(&mut self, target: &str, source: Value) -> Result<bool> {
        let id = source["id"]
            .as_str()
            .ok_or_else(|| Error::Invalid("native source requires a string ID".into()))?
            .to_owned();
        self.edit(|document| {
            let agent = document["spec"]["agents"]
                .get_mut(target)
                .ok_or_else(|| Error::Invalid("native source target is absent".into()))?;
            if agent.get("native_config").is_none() {
                agent["native_config"] = json!({"sources": []});
            }
            let sources = agent["native_config"]["sources"]
                .as_array_mut()
                .ok_or_else(|| Error::Invalid("native sources must be an array".into()))?;
            if let Some(existing) = sources.iter().find(|existing| existing["id"] == id) {
                require(existing == &source, "conflicting native source ID")?;
                return Ok(false);
            }
            sources.push(source);
            Ok(true)
        })
    }

    /// Remove an AW-owned source descriptor; never delete its native source.
    /// The last removal also drops the optional `native_config` section.
    ///
    /// # Errors
    /// Rejects absent targets or an invalid resulting AW document.
    pub fn remove_native_source(&mut self, target: &str, id: &str) -> Result<bool> {
        self.edit(|document| {
            let agent = document["spec"]["agents"]
                .get_mut(target)
                .ok_or_else(|| Error::Invalid("native source target is absent".into()))?;
            let Some(sources) = agent["native_config"]["sources"].as_array_mut() else {
                return Ok(false);
            };
            let Some(index) = sources.iter().position(|source| source["id"] == id) else {
                return Ok(false);
            };
            sources.remove(index);
            if sources.is_empty() {
                agent
                    .as_object_mut()
                    .ok_or_else(|| Error::Invalid("native source target must be an object".into()))?
                    .remove("native_config");
            }
            Ok(true)
        })
    }

    /// Inspect source categories and order, omitting all user-supplied strings.
    ///
    /// # Errors
    /// Rejects an absent target or invalid AW configuration.
    pub fn native_sources(&self, target: &str) -> Result<Vec<aw_config::native::SourceReport>> {
        let bytes = serde_json::to_vec(self.as_value())?;
        let config = self
            .validator
            .parse(&bytes)
            .map_err(super::configuration_error)?;
        let sources = config
            .native_sources(target)
            .map_err(super::configuration_error)?;
        Ok(aw_config::native::source_report(&sources))
    }
}

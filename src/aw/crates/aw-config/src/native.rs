//! Native source references stay separate from AW policy and native file contents.

use crate::{Configuration, Error};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

/// Native loader input; its order and opaque references are preserved by AW.
///
/// These descriptors do not override native argv/environment or assert that an
/// existing Adapter has consumed a source. The embedding loader owns resolution,
/// precedence, errors and hot updates. Source contents never enter this type.
#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeSource {
    /// Local descriptor identity, unrelated to native Hook/MCP/plugin names.
    pub id: String,
    /// How the native loader locates this source.
    pub kind: SourceKind,
    /// Native scope label, such as user, project or local.
    pub scope: String,
    /// File path, environment variable name, argument name or discovery key.
    pub reference: String,
    /// Native path-resolution context, passed through without canonicalization.
    pub base_dir: Option<String>,
}

/// Native source categories; actual lookup belongs to the Adapter.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// An original native configuration file.
    File,
    /// An environment variable name; never its credential-bearing value.
    Environment,
    /// A native option name; its value remains in native argv.
    Argument,
    /// A native loader's implicit source-discovery key.
    Discovery,
}

impl Configuration {
    /// Return ordered native references without reading files or environment.
    ///
    /// # Errors
    /// Rejects an absent target. Diagnostics omit target and source values.
    pub fn native_sources(&self, target: &str) -> Result<Vec<NativeSource>, Error> {
        let agent = self.as_value()["spec"]["agents"]
            .get(target)
            .ok_or(Error::Invariant {
                path: "/spec/agents".into(),
                reason: "native sources require an existing Agent target",
            })?;
        match agent.get("native_config") {
            None => Ok(Vec::new()),
            Some(value) => {
                serde_json::from_value(value["sources"].clone()).map_err(|_| Error::InvalidSchema)
            }
        }
    }
}

/// Value-free source inventory for logs and offline CLI inspection.
///
/// Only ordinal position and kind are exposed: even IDs, paths and scope labels
/// can contain private information. A declaration proves neither load nor coverage.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SourceReport {
    /// Zero-based position in the original source sequence.
    pub index: usize,
    /// Native source category.
    pub kind: SourceKind,
}

/// Project source metadata without credential values or source identifiers.
pub fn source_report(sources: &[NativeSource]) -> Vec<SourceReport> {
    sources
        .iter()
        .enumerate()
        .map(|(index, source)| SourceReport {
            index,
            kind: source.kind,
        })
        .collect()
}

pub(crate) fn validate(document: &Value) -> Result<(), Error> {
    let agents = document["spec"]["agents"]
        .as_object()
        .ok_or(Error::InvalidSchema)?;
    for (target, agent) in agents {
        if let Some(sources) = agent["native_config"]["sources"].as_array() {
            let mut ids = BTreeSet::new();
            for (index, source) in sources.iter().enumerate() {
                if !ids.insert(source["id"].as_str().ok_or(Error::InvalidSchema)?) {
                    return Err(Error::Invariant {
                        path: format!("/spec/agents/{target}/native_config/sources/{index}/id"),
                        reason: "native source IDs must be unique within a target",
                    });
                }
            }
        }
    }
    Ok(())
}

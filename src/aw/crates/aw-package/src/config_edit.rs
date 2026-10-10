//! Offline Provider and event-step editing with validated, atomic publication.
//! No operation starts an Agent, discovers capabilities or executes a Hook.

mod publication;

use crate::{require, Error, Result};
use publication::Snapshot;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

/// A validated configuration snapshot and its unpublished edits.
///
/// Mutations preserve unrelated values and omitted optional fields. Public fields
/// obey the bundled `aw-config` schema; Provider-owned `config` remains opaque.
/// Failed operations leave both the working document and the file unchanged.
pub struct ConfigurationEditor {
    path: PathBuf,
    original: Snapshot,
    document: Value,
    validator: aw_config::Validator,
}

impl ConfigurationEditor {
    /// Opens an existing, owned regular file through an absolute, symlink-free path.
    ///
    /// The parent must be owned and not group/world writable. Input is bounded by
    /// `aw-config`; unknown public fields and duplicate keys are rejected.
    ///
    /// # Errors
    /// Rejects unsafe paths, unreadable files or invalid desired configuration.
    pub fn open(path: &Path) -> Result<Self> {
        let original = publication::read(path)?;
        let validator = aw_config::Validator::new().map_err(configuration_error)?;
        let document = validator
            .parse(&original.bytes)
            .map_err(configuration_error)?
            .as_value()
            .clone();
        Ok(Self {
            path: path.into(),
            original,
            document,
            validator,
        })
    }

    /// Borrows the complete working document, including Provider private values.
    pub fn as_value(&self) -> &Value {
        &self.document
    }

    /// Looks up a named Provider without executing it.
    pub fn provider(&self, name: &str) -> Option<&Value> {
        self.document["spec"]["providers"].get(name)
    }

    /// Looks up an event step by its event-scoped ID.
    pub fn hook(&self, event: &str, id: &str) -> Option<&Value> {
        self.document["spec"]["events"][event]["steps"]
            .as_array()?
            .iter()
            .find(|step| step["id"] == id)
    }

    /// Adds a Provider; an identical definition is a no-op.
    ///
    /// # Errors
    /// Rejects a different definition under the same name or invalid configuration.
    pub fn add_provider(&mut self, name: &str, provider: Value) -> Result<bool> {
        self.edit(|document| insert(object(document, "providers")?, name, provider, "Provider"))
    }

    /// Removes an unreferenced Provider; an absent name is a no-op.
    ///
    /// Remove its referencing Hooks first. Disabled steps also retain references.
    ///
    /// # Errors
    /// Rejects removal that leaves a dangling reference; no Hook is removed implicitly.
    pub fn remove_provider(&mut self, name: &str) -> Result<bool> {
        self.edit(|document| Ok(object(document, "providers")?.remove(name).is_some()))
    }

    /// Adds an explicit event definition; an identical definition is a no-op.
    ///
    /// To add a Hook to an absent event, first supply its `enabled` and `steps`
    /// fields here, then call [`Self::add_hook`]. No event is implicitly enabled.
    ///
    /// # Errors
    /// Rejects a conflicting definition, unknown event or invalid configuration.
    pub fn add_event(&mut self, event: &str, definition: Value) -> Result<bool> {
        self.edit(|document| insert(object(document, "events")?, event, definition, "event"))
    }

    /// Appends a structured or native Hook step to an existing event.
    ///
    /// IDs are scoped to the event. Identical steps are no-ops; existing step
    /// order, event options and Provider configuration remain unchanged.
    ///
    /// # Errors
    /// Rejects an absent event, missing ID, conflicting ID or invalid configuration.
    pub fn add_hook(&mut self, event: &str, hook: Value) -> Result<bool> {
        let id = hook
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Invalid("Hook requires a string ID".into()))?
            .to_owned();
        self.edit(|document| {
            let steps = steps(document, event)?;
            if let Some(existing) = steps.iter().find(|step| step["id"] == id) {
                require(existing == &hook, "conflicting Hook ID in event")?;
                return Ok(false);
            }
            steps.push(hook);
            Ok(true)
        })
    }

    /// Removes an event-scoped Hook; an absent event or ID is a no-op.
    ///
    /// The event and its options are retained, including an empty `steps` array.
    ///
    /// # Errors
    /// Returns a validation error if the resulting configuration is invalid.
    pub fn remove_hook(&mut self, event: &str, id: &str) -> Result<bool> {
        self.edit(|document| {
            if document["spec"]["events"].get(event).is_none() {
                return Ok(false);
            }
            let steps = steps(document, event)?;
            let Some(index) = steps.iter().position(|step| step["id"] == id) else {
                return Ok(false);
            };
            steps.remove(index);
            Ok(true)
        })
    }

    /// Checks syntax, the public schema and static references without side effects.
    ///
    /// # Errors
    /// Returns the bundled validator's diagnostics, with configuration values masked.
    pub fn validate(&self) -> Result<()> {
        // Viewing a valid JSON input must not depend on YAML indentation overhead.
        let json = serde_json::to_vec(&self.document)?;
        self.validator.parse(&json).map_err(configuration_error)?;
        Ok(())
    }

    /// Publishes validated edits atomically, returning whether the file changed.
    ///
    /// A semantic no-op preserves original bytes and inode. A changed document is
    /// serialized as YAML: comments, formatting and key order are not retained.
    /// File mode is retained. Publication uses a nonblocking file lock and detects
    /// stale snapshots by file identity, metadata and bytes; callers must reopen
    /// after another writer succeeds. Arbitrary writers must honor the same lock
    /// to avoid a race between the final snapshot check and atomic replacement.
    /// No fallible operation follows replacement; this is not a power-loss
    /// durability guarantee or preservation of extended attributes/ACLs.
    ///
    /// # Errors
    /// Rejects invalid edits, contention, stale snapshots or filesystem failures.
    /// Errors leave the existing file intact and edits available for inspection.
    pub fn save(&mut self) -> Result<bool> {
        let original_value = self
            .validator
            .parse(&self.original.bytes)
            .map_err(configuration_error)?;
        let changed = original_value.as_value() != &self.document;
        let bytes = if changed {
            Some(self.validate_document(&self.document)?)
        } else {
            None
        };
        if let Some(snapshot) = publication::publish(
            &self.path,
            &self.original,
            bytes.as_ref().map(|bytes| bytes.as_bytes()),
        )? {
            self.original = snapshot;
        }
        Ok(changed)
    }

    fn edit(&mut self, operation: impl FnOnce(&mut Value) -> Result<bool>) -> Result<bool> {
        let mut candidate = self.document.clone();
        if !operation(&mut candidate)? {
            return Ok(false);
        }
        self.validate_document(&candidate)?;
        self.document = candidate;
        Ok(true)
    }

    fn validate_document(&self, document: &Value) -> Result<String> {
        let yaml = serde_yaml_ng::to_string(document)?;
        self.validator
            .parse(yaml.as_bytes())
            .map_err(configuration_error)?;
        Ok(yaml)
    }
}

fn configuration_error(error: aw_config::Error) -> Error {
    Error::Invalid(error.to_string())
}

fn object<'a>(document: &'a mut Value, field: &str) -> Result<&'a mut Map<String, Value>> {
    document["spec"][field]
        .as_object_mut()
        .ok_or_else(|| Error::Invalid("validated configuration object is missing".into()))
}

fn steps<'a>(document: &'a mut Value, event: &str) -> Result<&'a mut Vec<Value>> {
    object(document, "events")?
        .get_mut(event)
        .and_then(|event| event.get_mut("steps"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| Error::Invalid("declare the event before adding a Hook".into()))
}

fn insert(
    object: &mut Map<String, Value>,
    name: &str,
    definition: Value,
    kind: &str,
) -> Result<bool> {
    if let Some(existing) = object.get(name) {
        require(existing == &definition, format!("conflicting {kind} name"))?;
        return Ok(false);
    }
    object.insert(name.into(), definition);
    Ok(true)
}

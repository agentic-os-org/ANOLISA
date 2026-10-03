//! Versioned extension desired state and explicit source selection persistence.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::state::EXTENSIONS_STATE;

/// Current extension state schema version.
pub const EXTENSION_STATE_SCHEMA_VERSION: u32 = 1;

/// User-selectable installation source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceSelection {
    /// Select the user installation.
    User,
    /// Select the read-only system installation.
    System,
}

/// Explicit source selection bound to a canonical installation identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SourceSelectionRecord {
    /// User or system source class.
    pub source: SourceSelection,
    /// Canonical source identity present when the selection was made.
    pub source_identity: String,
}

/// Versioned extension desired-state document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExtensionState {
    /// State schema generation.
    pub schema_version: u32,
    /// Package identities the user wants disabled.
    #[serde(default)]
    pub disabled: BTreeSet<String>,
    /// Explicit package source selections.
    #[serde(default)]
    pub source_selections: BTreeMap<String, SourceSelectionRecord>,
    /// Last runtime generation persisted by the extension service.
    #[serde(default)]
    pub active_generation: u64,
}

impl Default for ExtensionState {
    fn default() -> Self {
        Self {
            schema_version: EXTENSION_STATE_SCHEMA_VERSION,
            disabled: BTreeSet::new(),
            source_selections: BTreeMap::new(),
            active_generation: 0,
        }
    }
}

/// Origin of a loaded state document, used for one-time compatibility choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateOrigin {
    /// No extension state file existed before this load.
    Missing,
    /// The legacy `{ "disabled": [...] }` schema was loaded.
    Legacy,
    /// A versioned state document was loaded.
    Versioned,
}

/// Loaded state plus its migration origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedExtensionState {
    /// Parsed versioned state projection.
    pub state: ExtensionState,
    /// Source schema of the file.
    pub origin: StateOrigin,
}

/// Extension state read or write failure.
#[derive(Debug)]
pub struct ExtensionStateError {
    code: &'static str,
    message: String,
}

impl ExtensionStateError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Returns the stable diagnostic code.
    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for ExtensionStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ExtensionStateError {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyExtensionState {
    #[serde(default)]
    disabled: Vec<String>,
}

/// Loads extension state without treating corruption as an empty enabled set.
pub fn load(
    state_dir_override: Option<&Path>,
) -> Result<LoadedExtensionState, ExtensionStateError> {
    let path = state_path(state_dir_override)?;
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LoadedExtensionState {
                state: ExtensionState::default(),
                origin: StateOrigin::Missing,
            });
        }
        Err(error) => {
            return Err(ExtensionStateError::new(
                "extension_state_unreadable",
                format!("failed to read {}: {error}", path.display()),
            ));
        }
    };
    let probe: serde_json::Value = serde_json::from_str(&content).map_err(|error| {
        ExtensionStateError::new(
            "extension_state_invalid",
            format!("failed to parse {}: {error}", path.display()),
        )
    })?;
    if probe.get("schemaVersion").is_some() {
        let state: ExtensionState = serde_json::from_str(&content).map_err(|error| {
            ExtensionStateError::new(
                "extension_state_invalid",
                format!(
                    "failed to parse versioned state {}: {error}",
                    path.display()
                ),
            )
        })?;
        if state.schema_version != EXTENSION_STATE_SCHEMA_VERSION {
            return Err(ExtensionStateError::new(
                "extension_state_schema_unsupported",
                format!(
                    "unsupported extension state schema {} in {}",
                    state.schema_version,
                    path.display()
                ),
            ));
        }
        return Ok(LoadedExtensionState {
            state,
            origin: StateOrigin::Versioned,
        });
    }
    let legacy: LegacyExtensionState = serde_json::from_str(&content).map_err(|error| {
        ExtensionStateError::new(
            "extension_state_invalid",
            format!("failed to parse legacy state {}: {error}", path.display()),
        )
    })?;
    Ok(LoadedExtensionState {
        state: ExtensionState {
            disabled: legacy.disabled.into_iter().collect(),
            ..ExtensionState::default()
        },
        origin: StateOrigin::Legacy,
    })
}

/// Atomically persists the current extension state schema.
pub fn save(
    state: &ExtensionState,
    state_dir_override: Option<&Path>,
) -> Result<(), ExtensionStateError> {
    let target = state_path(state_dir_override)?;
    let directory = target.parent().ok_or_else(|| {
        ExtensionStateError::new(
            "extension_state_path_invalid",
            format!("state path has no parent: {}", target.display()),
        )
    })?;
    fs::create_dir_all(directory).map_err(|error| {
        ExtensionStateError::new(
            "extension_state_write_failed",
            format!("failed to create {}: {error}", directory.display()),
        )
    })?;
    let bytes = serde_json::to_vec_pretty(state).map_err(|error| {
        ExtensionStateError::new(
            "extension_state_write_failed",
            format!("failed to serialize extension state: {error}"),
        )
    })?;
    let temporary = directory.join(format!(".{EXTENSIONS_STATE}.tmp"));
    fs::write(&temporary, bytes).map_err(|error| {
        ExtensionStateError::new(
            "extension_state_write_failed",
            format!("failed to write {}: {error}", temporary.display()),
        )
    })?;
    fs::rename(&temporary, &target).map_err(|error| {
        ExtensionStateError::new(
            "extension_state_write_failed",
            format!(
                "failed to atomically replace {} with {}: {error}",
                target.display(),
                temporary.display()
            ),
        )
    })?;
    Ok(())
}

/// Updates desired enabled state while preserving source selections.
pub fn set_enabled(
    name: &str,
    enabled: bool,
    state_dir_override: Option<&Path>,
) -> Result<ExtensionState, ExtensionStateError> {
    let mut loaded = load(state_dir_override)?.state;
    if enabled {
        loaded.disabled.remove(name);
    } else {
        loaded.disabled.insert(name.to_string());
    }
    save(&loaded, state_dir_override)?;
    Ok(loaded)
}

/// Persists an explicit user or system source selection.
pub fn select_source(
    name: &str,
    source: SourceSelection,
    source_identity: &str,
    state_dir_override: Option<&Path>,
) -> Result<ExtensionState, ExtensionStateError> {
    let mut loaded = load(state_dir_override)?.state;
    loaded.source_selections.insert(
        name.to_string(),
        SourceSelectionRecord {
            source,
            source_identity: source_identity.to_string(),
        },
    );
    save(&loaded, state_dir_override)?;
    Ok(loaded)
}

/// Persists the next successfully constructed runtime generation.
pub fn publish_next_generation(
    state_dir_override: Option<&Path>,
) -> Result<u64, ExtensionStateError> {
    let mut loaded = load(state_dir_override)?.state;
    loaded.active_generation = loaded.active_generation.saturating_add(1).max(1);
    save(&loaded, state_dir_override)?;
    Ok(loaded.active_generation)
}

/// Persists a generation only after the long-lived runtime makes it current.
pub fn persist_active_generation(
    generation: u64,
    state_dir_override: Option<&Path>,
) -> Result<u64, ExtensionStateError> {
    let mut loaded = load(state_dir_override)?.state;
    if generation < loaded.active_generation {
        return Err(ExtensionStateError::new(
            "extension_generation_regression",
            format!(
                "cannot move active generation backward from {} to {generation}",
                loaded.active_generation
            ),
        ));
    }
    loaded.active_generation = generation.max(1);
    save(&loaded, state_dir_override)?;
    Ok(loaded.active_generation)
}

fn state_path(state_dir_override: Option<&Path>) -> Result<PathBuf, ExtensionStateError> {
    let directory = match state_dir_override {
        Some(directory) => directory.to_path_buf(),
        None => crate::state::states_dir().ok_or_else(|| {
            ExtensionStateError::new(
                "extension_state_path_unavailable",
                "cannot determine extension state directory",
            )
        })?,
    };
    Ok(directory.join(EXTENSIONS_STATE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_generation_is_monotonic() {
        let temporary = tempfile::tempdir().unwrap();
        assert_eq!(publish_next_generation(Some(temporary.path())).unwrap(), 1);
        assert_eq!(publish_next_generation(Some(temporary.path())).unwrap(), 2);
        assert_eq!(
            load(Some(temporary.path()))
                .unwrap()
                .state
                .active_generation,
            2
        );
    }

    #[test]
    fn active_generation_rejects_regression() {
        let temporary = tempfile::tempdir().unwrap();
        persist_active_generation(4, Some(temporary.path())).unwrap();
        let error = persist_active_generation(3, Some(temporary.path())).unwrap_err();
        assert_eq!(error.code(), "extension_generation_regression");
        assert_eq!(
            load(Some(temporary.path()))
                .unwrap()
                .state
                .active_generation,
            4
        );
    }

    #[test]
    fn migrates_legacy_disabled_state_without_enabling() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join(EXTENSIONS_STATE),
            r#"{"disabled":["example.ops"]}"#,
        )
        .unwrap();
        let loaded = load(Some(directory.path())).unwrap();
        assert_eq!(loaded.origin, StateOrigin::Legacy);
        assert!(loaded.state.disabled.contains("example.ops"));

        save(&loaded.state, Some(directory.path())).unwrap();
        let migrated = load(Some(directory.path())).unwrap();
        assert_eq!(migrated.origin, StateOrigin::Versioned);
        assert!(migrated.state.disabled.contains("example.ops"));
    }

    #[test]
    fn rejects_corrupt_state_instead_of_returning_empty() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join(EXTENSIONS_STATE), "not json").unwrap();
        let error = load(Some(directory.path())).unwrap_err();
        assert_eq!(error.code(), "extension_state_invalid");
    }

    #[test]
    fn desired_state_updates_preserve_source_selection() {
        let directory = tempfile::tempdir().unwrap();
        select_source(
            "example.ops",
            SourceSelection::System,
            "/usr/share/anolisa/extensions/example.ops",
            Some(directory.path()),
        )
        .unwrap();
        let state = set_enabled("example.ops", false, Some(directory.path())).unwrap();
        assert!(state.disabled.contains("example.ops"));
        assert_eq!(
            state.source_selections.get("example.ops"),
            Some(&SourceSelectionRecord {
                source: SourceSelection::System,
                source_identity: "/usr/share/anolisa/extensions/example.ops".to_string(),
            })
        );
    }
}

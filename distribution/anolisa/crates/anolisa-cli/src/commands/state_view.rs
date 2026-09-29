use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anolisa_core::ObjectKind;
use anolisa_core::domain::{Installation, ProviderBinding};
use anolisa_core::state_store::StateStore;
use anolisa_platform::fs_layout::FsLayout;
use anolisa_platform::privilege;

use crate::commands::common;
use crate::context::{CliContext, InstallMode};
use crate::response::CliError;

const INSTALLED_STATE_FILE: &str = "installed.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StateScope {
    User,
    System,
}

impl StateScope {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::System => "system",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StateVisibility {
    UserPlusSystem,
}

#[derive(Debug, Clone)]
pub(crate) struct ScopedStateRoot {
    pub(crate) scope: StateScope,
    pub(crate) layout: FsLayout,
    pub(crate) state_path: PathBuf,
    pub(crate) writable: bool,
    pub(crate) state: StateStore,
}

#[derive(Debug, Clone)]
pub(crate) struct UnavailableStateRoot {
    pub(crate) scope: StateScope,
    pub(crate) state_path: PathBuf,
    pub(crate) reason: String,
}

#[derive(Debug, Clone)]
pub(crate) struct StateView {
    pub(crate) writable: ScopedStateRoot,
    pub(crate) visible_roots: Vec<ScopedStateRoot>,
    pub(crate) unavailable_roots: Vec<UnavailableStateRoot>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct RootSpec {
    scope: StateScope,
    writable: bool,
}

impl StateView {
    pub(crate) fn load(
        ctx: &CliContext,
        command: &str,
        visibility: StateVisibility,
    ) -> Result<Self, CliError> {
        let current_layout = common::resolve_layout(ctx);
        let current_scope = scope_for_mode(ctx.install_mode);
        let mut roots = Vec::new();
        roots.push((
            current_layout,
            RootSpec {
                scope: current_scope,
                writable: true,
            },
        ));

        if ctx.install_mode == InstallMode::User && visibility == StateVisibility::UserPlusSystem {
            roots.push((
                ctx.visible_system_layout().clone(),
                RootSpec {
                    scope: StateScope::System,
                    writable: false,
                },
            ));
        }

        Self::from_layouts(command, roots)
    }

    /// Build the visible view around a writable state snapshot the caller
    /// already loaded under its command-specific error contract.
    pub(crate) fn load_with_writable_state(
        ctx: &CliContext,
        command: &str,
        visibility: StateVisibility,
        state: StateStore,
    ) -> Result<Self, CliError> {
        let layout = common::resolve_layout(ctx);
        let state_path = layout.state_dir.join(INSTALLED_STATE_FILE);
        let writable = ScopedStateRoot {
            scope: scope_for_mode(ctx.install_mode),
            layout,
            state_path,
            writable: true,
            state,
        };
        let mut visible_roots = vec![writable.clone()];
        let mut unavailable_roots = Vec::new();
        let mut warnings = Vec::new();

        if ctx.install_mode == InstallMode::User && visibility == StateVisibility::UserPlusSystem {
            let system_layout = ctx.visible_system_layout().clone();
            let system_state_path = system_layout.state_dir.join(INSTALLED_STATE_FILE);
            match load_root_state(command, &system_layout, &system_state_path, false) {
                Ok(state) => visible_roots.push(ScopedStateRoot {
                    scope: StateScope::System,
                    layout: system_layout,
                    state_path: system_state_path,
                    writable: false,
                    state,
                }),
                Err(RootLoad::Warning(warning)) => {
                    unavailable_roots.push(UnavailableStateRoot {
                        scope: StateScope::System,
                        state_path: system_state_path,
                        reason: warning.clone(),
                    });
                    warnings.push(warning);
                }
                Err(RootLoad::Fatal(err)) => return Err(err),
            }
        }

        Ok(Self {
            writable,
            visible_roots,
            unavailable_roots,
            warnings,
        })
    }

    pub(crate) fn visible_components(&self) -> Vec<ScopedInstalledObject<'_>> {
        let mut records = Vec::new();
        for root in &self.visible_roots {
            for object in root
                .state
                .installations
                .iter()
                .filter(|installation| installation.kind == ObjectKind::Component)
            {
                let shadowed_by = records
                    .iter()
                    .find(|record: &&ScopedInstalledObject<'_>| record.object.name == object.name)
                    .map(ScopedInstalledObject::scope);
                records.push(ScopedInstalledObject {
                    root,
                    object,
                    active: shadowed_by.is_none(),
                    shadowed_by,
                    mutable_by_current_invocation: root.writable,
                });
            }
        }
        records
    }

    /// Whether any visible root owns an active or quarantined component under
    /// this exact name. Exact state identity precedes package aliases.
    pub(crate) fn has_exact_component(&self, component: &str) -> bool {
        self.exact_component_root(component).is_some()
    }

    /// Whether any visible root records an object of any kind under `name`,
    /// active or quarantined.
    ///
    /// Lifecycle target resolution pins such names literally: a non-component
    /// record (e.g. a legacy capability) is exact local state, and the
    /// owning command's migration guidance must win over an identity
    /// rejection for a name local state demonstrably knows.
    pub(crate) fn has_exact_object(&self, name: &str) -> bool {
        self.visible_roots.iter().any(|root| {
            root.state
                .installations
                .iter()
                .any(|installation| installation.name == name)
                || root
                    .state
                    .quarantined
                    .iter()
                    .any(|entry| entry.record.name == name)
                // Dropped legacy capability names still get their dedicated
                // migration guidance from the owning command.
                || root
                    .state
                    .dropped_capabilities
                    .iter()
                    .any(|dropped| dropped == name)
        })
    }

    pub(crate) fn resolve_mutation_component_identity(
        &self,
        command: &str,
        input: &str,
    ) -> Result<Option<String>, CliError> {
        if let Some(root) = self.exact_component_root(input) {
            return if root.writable {
                Ok(Some(input.to_string()))
            } else {
                Err(non_writable_component_error(
                    self.writable.scope,
                    root.scope,
                    command,
                    input,
                ))
            };
        }
        self.reject_incomplete_alias_visibility(command, input)?;

        let mut owners: BTreeMap<String, Vec<&ScopedStateRoot>> = BTreeMap::new();
        for root in &self.visible_roots {
            for installation in root
                .state
                .installations
                .iter()
                .filter(|installation| installation.kind == ObjectKind::Component)
            {
                if record_package_alias(installation) == Some(input) {
                    owners
                        .entry(installation.name.clone())
                        .or_default()
                        .push(root);
                }
            }
            for quarantined in root
                .state
                .quarantined
                .iter()
                .filter(|quarantined| quarantined.record.kind == ObjectKind::Component)
            {
                if quarantined_package_alias(&quarantined.record) == Some(input) {
                    owners
                        .entry(quarantined.record.name.clone())
                        .or_default()
                        .push(root);
                }
            }
        }

        if owners.len() > 1 {
            return Err(CliError::InvalidArgument {
                command: command.to_string(),
                reason: format!(
                    "package identity '{input}' is claimed by multiple components ({}); use an exact component name",
                    owners.keys().cloned().collect::<Vec<_>>().join(", ")
                ),
            });
        }
        let Some((component, roots)) = owners.into_iter().next() else {
            return Ok(None);
        };
        if roots.iter().any(|root| root.writable) {
            return Ok(Some(component));
        }
        let root = roots[0];
        Err(non_writable_component_error(
            self.writable.scope,
            root.scope,
            command,
            input,
        ))
    }

    pub(crate) fn reject_non_writable_component_mutation(
        &self,
        command: &str,
        component: &str,
    ) -> Result<(), CliError> {
        if let Some(root) = self.exact_component_root(component) {
            if root.writable {
                return Ok(());
            }
            return Err(non_writable_component_error(
                self.writable.scope,
                root.scope,
                command,
                component,
            ));
        }
        if let Some(record) = self.visible_components().into_iter().find(|record| {
            record.active
                && !record.mutable_by_current_invocation
                && record_package_alias(record.object) == Some(component)
        }) {
            return Err(non_writable_component_error(
                self.writable.scope,
                record.scope(),
                command,
                component,
            ));
        }
        Ok(())
    }

    /// Refuse alias inference when one of the roots that could own the exact
    /// input identity could not be loaded.
    pub(crate) fn reject_incomplete_alias_visibility(
        &self,
        command: &str,
        input: &str,
    ) -> Result<(), CliError> {
        if self.unavailable_roots.is_empty() {
            return Ok(());
        }
        let roots = self
            .unavailable_roots
            .iter()
            .map(|root| {
                format!(
                    "{} scope at {} ({})",
                    root.scope.label(),
                    root.state_path.display(),
                    root.reason
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        Err(CliError::Runtime {
            command: command.to_string(),
            reason: format!(
                "cannot resolve '{input}' through a repository alias because visible state is incomplete: {roots}; restore state readability or use an exact name already present in the writable scope"
            ),
        })
    }

    fn exact_component_root(&self, component: &str) -> Option<&ScopedStateRoot> {
        self.visible_roots
            .iter()
            .find(|root| root.state.contains_record(ObjectKind::Component, component))
    }

    fn from_layouts(command: &str, roots: Vec<(FsLayout, RootSpec)>) -> Result<Self, CliError> {
        let mut visible_roots = Vec::new();
        let mut writable = None;
        let mut unavailable_roots = Vec::new();
        let mut warnings = Vec::new();

        for (layout, spec) in roots {
            let state_path = layout.state_dir.join(INSTALLED_STATE_FILE);
            let loaded = load_root_state(command, &layout, &state_path, spec.writable);
            let state = match loaded {
                Ok(state) => state,
                Err(RootLoad::Fatal(err)) => return Err(err),
                Err(RootLoad::Warning(warning)) => {
                    unavailable_roots.push(UnavailableStateRoot {
                        scope: spec.scope,
                        state_path,
                        reason: warning.clone(),
                    });
                    warnings.push(warning);
                    continue;
                }
            };
            let root = ScopedStateRoot {
                scope: spec.scope,
                layout,
                state_path,
                writable: spec.writable,
                state,
            };
            if spec.writable {
                writable = Some(root.clone());
            }
            visible_roots.push(root);
        }

        let Some(writable) = writable else {
            return Err(CliError::Runtime {
                command: command.to_string(),
                reason: "state view has no writable root".to_string(),
            });
        };

        Ok(Self {
            writable,
            visible_roots,
            unavailable_roots,
            warnings,
        })
    }
}

fn non_writable_component_error(
    current_scope: StateScope,
    record_scope: StateScope,
    command: &str,
    component: &str,
) -> CliError {
    CliError::PermissionDenied {
        command: command.to_string(),
        reason: format!(
            "component '{component}' is {}-scope and read-only from the current {}-mode invocation",
            record_scope.label(),
            current_scope.label(),
        ),
        hint: Some(scope_mutation_hint(record_scope, command)),
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ScopedInstalledObject<'a> {
    pub(crate) root: &'a ScopedStateRoot,
    pub(crate) object: &'a Installation,
    pub(crate) active: bool,
    pub(crate) shadowed_by: Option<StateScope>,
    pub(crate) mutable_by_current_invocation: bool,
}

impl ScopedInstalledObject<'_> {
    pub(crate) const fn scope(&self) -> StateScope {
        self.root.scope
    }
}

/// Package-name alias an installation is addressable by, in addition to its
/// component name: the raw package for owned artifacts, the resolved native
/// package for delegated ones.
fn record_package_alias(installation: &Installation) -> Option<&str> {
    match &installation.binding {
        ProviderBinding::Owned { artifact } => artifact.raw_package.as_deref(),
        ProviderBinding::Delegated { package, .. } => package.resolved_name(),
    }
}

fn quarantined_package_alias(installation: &anolisa_core::state::InstalledObject) -> Option<&str> {
    installation.raw_package.as_deref().or_else(|| {
        installation
            .rpm_metadata
            .as_ref()
            .map(|metadata| metadata.package_name.as_str())
    })
}

enum RootLoad {
    Fatal(CliError),
    Warning(String),
}

fn load_root_state(
    command: &str,
    layout: &FsLayout,
    state_path: &Path,
    writable: bool,
) -> Result<StateStore, RootLoad> {
    StateStore::load_for_layout(state_path, privilege::effective_uid(), layout).map_err(|err| {
        if writable {
            RootLoad::Fatal(CliError::InvalidArgument {
                command: command.to_string(),
                reason: format!(
                    "failed to load installed state at {}: {err}",
                    state_path.display()
                ),
            })
        } else {
            RootLoad::Warning(format!(
                "failed to load visible system state at {}: {err}",
                state_path.display()
            ))
        }
    })
}

const fn scope_for_mode(mode: InstallMode) -> StateScope {
    match mode {
        InstallMode::User => StateScope::User,
        InstallMode::System => StateScope::System,
    }
}

fn scope_mutation_hint(scope: StateScope, command: &str) -> String {
    match scope {
        StateScope::System => {
            format!("run `sudo anolisa --install-mode system {command}` to mutate system state")
        }
        StateScope::User => {
            format!("run `anolisa --install-mode user {command}` to mutate user state")
        }
    }
}

#[cfg(test)]
mod tests;

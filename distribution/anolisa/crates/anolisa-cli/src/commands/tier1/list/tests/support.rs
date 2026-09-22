use anolisa_core::domain::{
    Installation, InstallationScope, LifecycleStatus, ManagementRelation, NativePm, Observation,
    OwnedArtifact, PackageIdentity, ProviderBinding,
};
use anolisa_core::state::ObjectKind;
use anolisa_core::state_store::StateStore;
use anolisa_platform::pkg_query::{PackageInfo, PackageQuery, PackageQueryError, PackageVersion};

use crate::commands::tier1::list::state_view::{self, LocalProjection};
use crate::resolution::{
    ComponentAliasEntry, ComponentBackendEntry, ComponentIndex, ComponentIndexEntry,
    ComponentTarget,
};

pub(super) fn target(os: &str, arch: &str) -> ComponentTarget {
    ComponentTarget {
        os: os.to_string(),
        arch: arch.to_string(),
    }
}

pub(super) fn sample_index() -> ComponentIndex {
    ComponentIndex {
        schema_version: 2,
        generated_at: None,
        publisher: Some("anolisa".to_string()),
        components: vec![
            ComponentIndexEntry {
                name: "agentsight".to_string(),
                display_name: Some("AgentSight".to_string()),
                summary: Some("eBPF-based AI agent observability tool".to_string()),
                targets: vec![target("linux", "x86_64"), target("macos", "aarch64")],
                backends: vec![
                    ComponentBackendEntry {
                        kind: "raw".to_string(),
                        package: "agentsight".to_string(),
                        provides: None,
                        legacy_adopt: false,
                    },
                    ComponentBackendEntry {
                        kind: "rpm".to_string(),
                        package: "agentsight".to_string(),
                        provides: Some("anolisa-component(agentsight)".to_string()),
                        legacy_adopt: true,
                    },
                ],
                aliases: Vec::new(),
            },
            ComponentIndexEntry {
                name: "tokenless".to_string(),
                display_name: Some("Tokenless".to_string()),
                summary: Some("LLM token optimization toolkit".to_string()),
                targets: vec![
                    target("linux", "x86_64"),
                    target("linux", "aarch64"),
                    target("macos", "aarch64"),
                ],
                backends: vec![ComponentBackendEntry {
                    kind: "raw".to_string(),
                    package: "tokenless".to_string(),
                    provides: None,
                    legacy_adopt: false,
                }],
                aliases: Vec::new(),
            },
        ],
    }
}

const FIXTURE_TIME: &str = "2026-06-12T00:00:00Z";

pub(super) fn empty_state() -> StateStore {
    StateStore::empty()
}

pub(super) fn managed() -> ManagementRelation {
    ManagementRelation::Managed {
        since: FIXTURE_TIME.to_string(),
    }
}

pub(super) fn adopted() -> ManagementRelation {
    ManagementRelation::Adopted {
        since: FIXTURE_TIME.to_string(),
    }
}

fn installation(
    kind: ObjectKind,
    name: &str,
    status: LifecycleStatus,
    binding: ProviderBinding,
) -> Installation {
    Installation {
        kind,
        name: name.to_string(),
        scope: InstallationScope::System,
        binding,
        status,
        installed_at: FIXTURE_TIME.to_string(),
        last_operation_id: None,
        subscription_scope: Default::default(),
        enabled_features: Vec::new(),
        health: Vec::new(),
    }
}

fn owned_binding() -> ProviderBinding {
    ProviderBinding::Owned {
        artifact: OwnedArtifact {
            version: "0.1.0".to_string(),
            distribution_source: None,
            raw_package: None,
            manifest_digest: None,
            files: Vec::new(),
            services: Vec::new(),
            external_modified_files: Vec::new(),
            provisioned_packages: Vec::new(),
        },
    }
}

pub(super) fn state_with_owned_object(
    kind: ObjectKind,
    name: &str,
    status: LifecycleStatus,
) -> StateStore {
    let mut store = StateStore::empty();
    store
        .installations
        .push(installation(kind, name, status, owned_binding()));
    store
}

/// An adopted delegated record with no package resolution — the minimal
/// rpm-backed shape whose projected status is `adopted`.
pub(super) fn state_with_adopted_object(kind: ObjectKind, name: &str) -> StateStore {
    let mut store = StateStore::empty();
    store.installations.push(installation(
        kind,
        name,
        LifecycleStatus::Installed,
        ProviderBinding::Delegated {
            pm: NativePm::Rpm,
            package: PackageIdentity::Unresolved {
                component_hint: name.to_string(),
            },
            relation: adopted(),
            last_observed: None,
        },
    ));
    store
}

pub(super) fn state_with_component_object(mut installation: Installation) -> StateStore {
    installation.kind = ObjectKind::Component;
    let mut store = StateStore::empty();
    store.installations.push(installation);
    store
}

pub(super) fn owned_component(name: &str, status: LifecycleStatus) -> Installation {
    installation(ObjectKind::Component, name, status, owned_binding())
}

/// Delegated component without package resolution (no rpm metadata in the
/// legacy vocabulary) — drift probes have nothing to query.
pub(super) fn delegated_component(
    name: &str,
    status: LifecycleStatus,
    relation: ManagementRelation,
) -> Installation {
    installation(
        ObjectKind::Component,
        name,
        status,
        ProviderBinding::Delegated {
            pm: NativePm::Rpm,
            package: PackageIdentity::Unresolved {
                component_hint: name.to_string(),
            },
            relation,
            last_observed: None,
        },
    )
}

pub(super) fn rpm_component_object(
    name: &str,
    status: LifecycleStatus,
    relation: ManagementRelation,
    package: &str,
    evr: &str,
) -> Installation {
    installation(
        ObjectKind::Component,
        name,
        status,
        ProviderBinding::Delegated {
            pm: NativePm::Rpm,
            package: PackageIdentity::Resolved {
                name: package.to_string(),
            },
            relation,
            last_observed: Some(Observation {
                version: evr.to_string(),
                evr: Some(evr.to_string()),
                arch: Some("x86_64".to_string()),
                source_repo: Some("@System".to_string()),
                observed_at: FIXTURE_TIME.to_string(),
            }),
        },
    )
}

#[derive(Default)]
pub(super) struct FakeRpmQuery {
    pub(super) installed: Vec<(String, PackageInfo)>,
    pub(super) command_missing: bool,
    pub(super) what_provides: Vec<(String, Vec<String>)>,
}

impl PackageQuery for FakeRpmQuery {
    fn query_installed(&self, package: &str) -> Result<Option<PackageInfo>, PackageQueryError> {
        if self.command_missing {
            return Err(PackageQueryError::CommandMissing {
                command: "rpm".to_string(),
            });
        }
        Ok(self
            .installed
            .iter()
            .find(|(name, _)| name == package)
            .map(|(_, info)| info.clone()))
    }

    fn query_available(&self, _package: &str) -> Result<Vec<PackageInfo>, PackageQueryError> {
        Ok(Vec::new())
    }

    fn what_provides_installed(&self, capability: &str) -> Result<Vec<String>, PackageQueryError> {
        if self.command_missing {
            return Err(PackageQueryError::CommandMissing {
                command: "rpm".to_string(),
            });
        }
        Ok(self
            .what_provides
            .iter()
            .find(|(cap, _)| cap == capability)
            .map(|(_, names)| names.clone())
            .unwrap_or_default())
    }
}

pub(super) fn pkg_info(
    name: &str,
    version: &str,
    release: Option<&str>,
    arch: &str,
) -> PackageInfo {
    PackageInfo {
        name: name.to_string(),
        version: PackageVersion {
            epoch: None,
            version: version.to_string(),
            release: release.map(str::to_string),
        },
        arch: arch.to_string(),
        origin: Some("@System".to_string()),
    }
}

pub(super) fn projection_for(
    component: &str,
    state: &StateStore,
    query: &dyn PackageQuery,
) -> LocalProjection {
    let index = sample_index();
    projection_for_index(&index, component, state, query)
}

pub(super) fn projection_for_index(
    index: &ComponentIndex,
    component: &str,
    state: &StateStore,
    query: &dyn PackageQuery,
) -> LocalProjection {
    let entry = index
        .components
        .iter()
        .find(|entry| entry.name == component)
        .unwrap();
    state_view::project_component(entry, index, state, Some(query))
}

/// A component whose RPM backend package name differs from the component name,
/// with an rpm-package alias — mirrors the real `cosh` / `copilot-shell` mapping.
pub(super) fn sample_index_with_aliases() -> ComponentIndex {
    ComponentIndex {
        schema_version: 2,
        generated_at: None,
        publisher: Some("anolisa".to_string()),
        components: vec![ComponentIndexEntry {
            name: "cosh".to_string(),
            display_name: Some("Copilot Shell".to_string()),
            summary: Some("shell".to_string()),
            targets: vec![target("linux", "x86_64")],
            backends: vec![
                ComponentBackendEntry {
                    kind: "raw".to_string(),
                    package: "cosh".to_string(),
                    provides: None,
                    legacy_adopt: false,
                },
                ComponentBackendEntry {
                    kind: "rpm".to_string(),
                    package: "copilot-shell".to_string(),
                    provides: Some("anolisa-component(cosh)".to_string()),
                    legacy_adopt: true,
                },
            ],
            aliases: vec![ComponentAliasEntry {
                kind: "rpm-package".to_string(),
                name: "cosh-old".to_string(),
            }],
        }],
    }
}

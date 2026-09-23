use anolisa_core::domain::{Installation, LifecycleStatus, ManagementRelation, ProviderBinding};
use anolisa_core::state::ObjectKind;
use anolisa_core::state_store::StateStore;
use anolisa_platform::pkg_query::{PackageInfo, PackageQuery, PackageQueryError};

use crate::commands::common;
use crate::resolution::{
    BackendKind, ComponentIndex, ComponentIndexEntry, ComponentResolver, ResolutionSet,
    ResolveOptions,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LocalState {
    Observed,
    Tracked,
    Installed,
    Drifted,
    Missing,
    Failed,
    Degraded,
    Disabled,
    NotInstalled,
}

impl LocalState {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Tracked => "tracked",
            Self::Installed => "installed",
            Self::Drifted => "drifted",
            Self::Missing => "missing",
            Self::Failed => "failed",
            Self::Degraded => "degraded",
            Self::Disabled => "disabled",
            Self::NotInstalled => "not_installed",
        }
    }

    pub(super) const fn matches_installed_filter(self) -> bool {
        match self {
            Self::Observed | Self::Tracked | Self::Installed | Self::Degraded => true,
            Self::Drifted | Self::Missing | Self::Failed | Self::Disabled | Self::NotInstalled => {
                false
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListAction {
    Install,
    Status,
}

impl ListAction {
    const fn label(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Status => "status",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LocalProjection {
    pub(super) backend: Option<String>,
    pub(super) local_state: LocalState,
    /// Provenance label of the tracked record (`owned` / `managed` /
    /// `adopted` / `observed`); `None` for untracked entries.
    pub(super) ownership: Option<&'static str>,
    action: ListAction,
    pub(super) status: String,
    pub(super) rpm_package: Option<String>,
    pub(super) rpm_evr: Option<String>,
    pub(super) rpm_arch: Option<String>,
    pub(super) rpm_source_repo: Option<String>,
}

impl LocalProjection {
    #[cfg(test)]
    pub(super) const fn local_state_label(&self) -> &'static str {
        self.local_state.label()
    }

    pub(super) fn ownership_label(&self) -> &'static str {
        match self.ownership {
            Some(label) => label,
            // Observed RPMs are owned by the package manager even though
            // ANOLISA has not claimed any ownership yet.
            None if self.local_state == LocalState::Observed => "rpm",
            None => "none",
        }
    }

    pub(super) const fn action_label(&self) -> &'static str {
        self.action.label()
    }
}

pub(super) fn project_component(
    entry: &ComponentIndexEntry,
    index: &ComponentIndex,
    state: &StateStore,
    rpm_query: Option<&dyn PackageQuery>,
) -> LocalProjection {
    match state.find(ObjectKind::Component, &entry.name) {
        Some(installation) => project_tracked_object(installation, rpm_query),
        None => project_untracked_entry(entry, index, rpm_query),
    }
}

fn project_untracked_entry(
    entry: &ComponentIndexEntry,
    index: &ComponentIndex,
    rpm_query: Option<&dyn PackageQuery>,
) -> LocalProjection {
    match observed_rpm_info(entry, index, rpm_query) {
        Some(info) => projection_from_observed_rpm(info),
        None => LocalProjection {
            backend: None,
            local_state: LocalState::NotInstalled,
            ownership: None,
            action: ListAction::Install,
            status: "not_installed".to_string(),
            rpm_package: None,
            rpm_evr: None,
            rpm_arch: None,
            rpm_source_repo: None,
        },
    }
}

fn project_tracked_object(
    installation: &Installation,
    rpm_query: Option<&dyn PackageQuery>,
) -> LocalProjection {
    let base_state = tracked_state_without_rpm_drift(installation);
    let local_state = match base_state {
        LocalState::Installed | LocalState::Tracked => {
            rpm_drift_state(installation, rpm_query).unwrap_or(base_state)
        }
        LocalState::Observed
        | LocalState::Drifted
        | LocalState::Missing
        | LocalState::Failed
        | LocalState::Degraded
        | LocalState::Disabled
        | LocalState::NotInstalled => base_state,
    };
    let (provenance, backend, rpm_package, rpm_evr, rpm_arch, rpm_source_repo) =
        match &installation.binding {
            ProviderBinding::Owned { .. } => ("owned", "raw", None, None, None, None),
            ProviderBinding::Delegated {
                package,
                relation,
                last_observed,
                ..
            } => (
                relation.label(),
                "rpm",
                package.resolved_name().map(str::to_string),
                last_observed.as_ref().and_then(|o| o.evr.clone()),
                last_observed.as_ref().and_then(|o| o.arch.clone()),
                last_observed.as_ref().and_then(|o| o.source_repo.clone()),
            ),
        };

    LocalProjection {
        backend: Some(backend.to_string()),
        local_state,
        ownership: Some(provenance),
        action: ListAction::Status,
        status: common::installation_status_str(installation).to_string(),
        rpm_package,
        rpm_evr,
        rpm_arch,
        rpm_source_repo,
    }
}

fn tracked_state_without_rpm_drift(installation: &Installation) -> LocalState {
    match installation.status {
        LifecycleStatus::Installed => match &installation.binding {
            // No management consent: tracked, but not ANOLISA-installed.
            ProviderBinding::Delegated {
                relation: ManagementRelation::Adopted { .. } | ManagementRelation::Observed,
                ..
            } => LocalState::Tracked,
            _ => LocalState::Installed,
        },
        LifecycleStatus::Partial => LocalState::Degraded,
        LifecycleStatus::Disabled => LocalState::Disabled,
        LifecycleStatus::Failed => LocalState::Failed,
    }
}

fn rpm_drift_state(
    installation: &Installation,
    rpm_query: Option<&dyn PackageQuery>,
) -> Option<LocalState> {
    let ProviderBinding::Delegated {
        package,
        last_observed,
        ..
    } = &installation.binding
    else {
        return None;
    };
    let package = package.resolved_name()?;
    let query = rpm_query?;
    match query.query_installed(package) {
        Ok(Some(info)) => {
            let live_evr = info.version.to_string();
            let recorded_evr = last_observed.as_ref().and_then(|o| o.evr.as_deref());
            let recorded_arch = last_observed.as_ref().and_then(|o| o.arch.as_deref());
            let evr_drifted = recorded_evr.is_some_and(|evr| evr != live_evr);
            let arch_drifted = recorded_arch.is_some_and(|arch| arch != info.arch);
            if evr_drifted || arch_drifted {
                Some(LocalState::Drifted)
            } else {
                None
            }
        }
        Ok(None) => Some(LocalState::Missing),
        Err(PackageQueryError::CommandMissing { .. })
        | Err(PackageQueryError::PermissionDenied { .. })
        | Err(PackageQueryError::QueryFailed { .. })
        | Err(PackageQueryError::UnexpectedOutput { .. })
        | Err(PackageQueryError::Repository(_)) => None,
    }
}

fn observed_rpm_info(
    entry: &ComponentIndexEntry,
    index: &ComponentIndex,
    rpm_query: Option<&dyn PackageQuery>,
) -> Option<PackageInfo> {
    let query = rpm_query?;
    // Without an RPM mapping, the resolver would query available providers;
    // list only observes installed packages from the published RPM backends.
    if !entry.backends.iter().any(|backend| backend.kind == "rpm") {
        return None;
    }
    // Observation must use the same package authority as install. Aliases
    // identify components; a stale capability cannot override the index.
    let resolver = ComponentResolver::new(Some(index), None, Some(query));
    let ResolutionSet::Unique(target) = resolver
        .resolve(&entry.name, BackendKind::Rpm, ResolveOptions::default())
        .ok()?
    else {
        return None;
    };
    safe_query_installed(query, &target.package)
}

/// Query an installed package, treating all errors as "not found" so the list
/// command degrades gracefully when `rpm`/`dnf` is absent or the query fails.
fn safe_query_installed(query: &dyn PackageQuery, package: &str) -> Option<PackageInfo> {
    match query.query_installed(package) {
        Ok(Some(mut info)) => {
            if info.origin.is_none() {
                info.origin = query.installed_origin(&info.name).ok().flatten();
            }
            Some(info)
        }
        Ok(None) => None,
        Err(_) => None,
    }
}

fn projection_from_observed_rpm(info: PackageInfo) -> LocalProjection {
    LocalProjection {
        backend: Some("rpm".to_string()),
        local_state: LocalState::Observed,
        ownership: None,
        action: ListAction::Install,
        status: "not_installed".to_string(),
        rpm_package: Some(info.name),
        rpm_evr: Some(info.version.to_string()),
        rpm_arch: Some(info.arch),
        rpm_source_repo: info.origin,
    }
}

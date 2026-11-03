//! File-system and IO helpers: atomic writes, rollback, state snapshots,
//! and timestamp formatting for the `install` command.

use anolisa_platform::fs_layout::create_install_dir_all;
use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use anolisa_core::adapter::contract::{
    ContractProvenance, ContractSourceKind, read_snapshot_provenance,
};
use anolisa_core::central_log::CentralLog;
use anolisa_core::{ServiceManager, ServiceRunOutcome, deactivate_services};
use anolisa_platform::fs_layout::FsLayout;
use chrono::{SecondsFormat, Utc};

use crate::commands::common;
use crate::response::CliError;

/// Best-effort cleanup of installed files after a state-save failure.
pub(crate) fn rollback_installed_files(files: &[anolisa_core::InstalledFile]) {
    for f in files {
        let _ = std::fs::remove_file(&f.path);
    }
}

/// Best-effort cleanup for service side effects from an install that will
/// otherwise roll back.
pub(crate) fn rollback_activated_services(
    manager: &dyn ServiceManager,
    service_run: &ServiceRunOutcome,
    log: Option<&CentralLog>,
    component: &str,
    operation_id: &str,
    install_mode: &str,
) -> Vec<String> {
    let units: BTreeSet<String> = service_run
        .enabled_units
        .iter()
        .chain(service_run.started_units.iter())
        .cloned()
        .collect();
    if units.is_empty() {
        return Vec::new();
    }
    let units = units
        .into_iter()
        .map(|unit| (component.to_string(), unit))
        .collect::<Vec<_>>();
    deactivate_services(manager, &units, log, operation_id, "cli", install_mode).warnings
}

pub(crate) fn service_cleanup_suffix(warnings: &[String]) -> String {
    if warnings.is_empty() {
        String::new()
    } else {
        format!("; service cleanup warnings: {}", warnings.join("; "))
    }
}

pub(crate) fn write_installed_component_manifest(
    layout: &FsLayout,
    component: &str,
    toml: &str,
) -> Result<PathBuf, CliError> {
    let path = common::installed_component_manifest_path(layout, component, super::COMMAND)?;
    write_atomic_text(&path, toml).map_err(|err| CliError::Runtime {
        command: super::COMMAND.to_string(),
        reason: format!(
            "failed to write installed component manifest at {}: {err}",
            path.display()
        ),
    })?;
    Ok(path)
}

struct DatadirContract {
    content: String,
    source_path: PathBuf,
    datadir_root: PathBuf,
}

enum DatadirContractLookup {
    Found(DatadirContract),
    Missing(Vec<PathBuf>),
    Unreadable {
        source_path: PathBuf,
        source: std::io::Error,
    },
}

/// Read-only result for comparing a distribution-owned contract with its state snapshot.
pub(crate) struct ContractDriftInspection {
    /// Whether the snapshot and provenance differ from the distribution contract.
    pub(crate) drifted: bool,
    /// Non-fatal lookup or path-resolution failures.
    pub(crate) warnings: Vec<String>,
}

/// Result of refreshing a distribution-owned contract snapshot and its provenance.
pub(crate) struct ContractRefreshOutcome {
    /// Whether publication succeeded, was unnecessary, or failed.
    state: ContractRefreshState,
    /// Diagnostics for lookup or publication failures.
    pub(crate) warnings: Vec<String>,
}

#[derive(Clone, Copy)]
enum ContractRefreshState {
    Refreshed,
    NotApplicable,
    Failed,
}

impl ContractRefreshOutcome {
    /// Describe why a required refresh did not complete.
    pub(crate) fn failure_detail(&self) -> Option<String> {
        match self.state {
            ContractRefreshState::Refreshed => None,
            ContractRefreshState::NotApplicable => {
                Some("the distribution contract was unavailable during refresh".into())
            }
            ContractRefreshState::Failed => Some(self.warning_detail()),
        }
    }

    /// Describe a genuine lookup or publication failure.
    pub(crate) fn error_detail(&self) -> Option<String> {
        matches!(self.state, ContractRefreshState::Failed).then(|| self.warning_detail())
    }

    fn warning_detail(&self) -> String {
        if self.warnings.is_empty() {
            "the distribution-owned component contract could not be refreshed".to_string()
        } else {
            self.warnings.join("; ")
        }
    }
}

fn datadir_contract_roots(
    layout: &FsLayout,
    packaged_data_probe: &crate::packaged::PackagedDataProbe,
) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(package_datadir) = layout.package_datadir() {
        roots.push(package_datadir);
    }
    if let Some(packaged) = crate::packaged::packaged_datadir_root(layout, packaged_data_probe)
        && !roots.iter().any(|root| root == &packaged)
    {
        roots.push(packaged);
    }
    if !roots.iter().any(|root| root == &layout.datadir) {
        roots.push(layout.datadir.clone());
    }
    roots
}

fn lookup_datadir_contract(
    layout: &FsLayout,
    component: &str,
    packaged_data_probe: &crate::packaged::PackagedDataProbe,
) -> DatadirContractLookup {
    let mut searched: Vec<PathBuf> = Vec::new();
    for datadir_root in datadir_contract_roots(layout, packaged_data_probe) {
        let source_path = FsLayout::component_contract_path(&datadir_root, component);
        match std::fs::read_to_string(&source_path) {
            Ok(content) => {
                return DatadirContractLookup::Found(DatadirContract {
                    content,
                    source_path,
                    datadir_root,
                });
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                searched.push(source_path);
            }
            Err(source) => {
                return DatadirContractLookup::Unreadable {
                    source_path,
                    source,
                };
            }
        }
    }
    DatadirContractLookup::Missing(searched)
}

/// Compare the distribution-owned component contract with its state snapshot.
///
/// A missing distribution contract is not drift because there is no authoritative
/// content to copy. An unreadable snapshot is drift so a real reconciliation
/// can attempt to replace it and surface any write failure.
pub(crate) fn inspect_datadir_contract_drift(
    layout: &FsLayout,
    component: &str,
    command: &str,
    packaged_data_probe: &crate::packaged::PackagedDataProbe,
) -> ContractDriftInspection {
    let contract = match lookup_datadir_contract(layout, component, packaged_data_probe) {
        DatadirContractLookup::Found(contract) => contract,
        DatadirContractLookup::Missing(_) => {
            return ContractDriftInspection {
                drifted: false,
                warnings: Vec::new(),
            };
        }
        DatadirContractLookup::Unreadable {
            source_path,
            source,
        } => {
            return ContractDriftInspection {
                drifted: false,
                warnings: vec![format!(
                    "could not read datadir component contract at {}: {source}",
                    source_path.display()
                )],
            };
        }
    };
    let destination = match common::installed_component_manifest_path(layout, component, command) {
        Ok(path) => path,
        Err(err) => {
            return ContractDriftInspection {
                drifted: false,
                warnings: vec![format!(
                    "could not resolve snapshot path for component '{component}': {err}"
                )],
            };
        }
    };

    let expected_provenance = ContractProvenance {
        schema_version: 1,
        source_kind: ContractSourceKind::Datadir,
        source_path: contract.source_path,
        datadir_root: contract.datadir_root,
    };
    match std::fs::read_to_string(&destination) {
        Ok(snapshot) => ContractDriftInspection {
            drifted: snapshot != contract.content
                || !snapshot_provenance_matches(&destination, &expected_provenance),
            warnings: Vec::new(),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => ContractDriftInspection {
            drifted: true,
            warnings: Vec::new(),
        },
        Err(err) => ContractDriftInspection {
            drifted: true,
            warnings: vec![format!(
                "could not read installed component manifest at {}: {err}",
                destination.display()
            )],
        },
    }
}

fn snapshot_provenance_matches(snapshot_path: &Path, expected: &ContractProvenance) -> bool {
    read_snapshot_provenance(snapshot_path).is_some_and(|actual| {
        actual.schema_version == expected.schema_version
            && actual.source_kind == expected.source_kind
            && actual.source_path == expected.source_path
            && actual.datadir_root == expected.datadir_root
    })
}

/// Best-effort snapshot of the datadir component contract for RPM paths.
///
/// ANOLISA publishes integration contracts at
/// `{datadir}/components/<component>/component.toml`. Its RPM uses
/// `/usr/share/anolisa/`; the source installer may use `/usr/local/share/anolisa/`.
/// Every RPM lifecycle operation probes the FHS package datadir first, then
/// the running CLI packaged tree and the selected installation datadir.
///
/// The contract is copied verbatim (no TOML parsing) to the state snapshot
/// at `{state_dir}/component-manifests/<component>/component.toml` so that
/// later `adapter enable` can discover the component's declared adapters.
///
/// Returns any warning messages that should be surfaced to the user.
/// Neither a missing contract nor a write failure is fatal — both produce
/// a warning instead of an error.
pub(crate) fn snapshot_datadir_contract(
    layout: &FsLayout,
    component: &str,
    command: &str,
    packaged_data_probe: &crate::packaged::PackagedDataProbe,
) -> Vec<String> {
    snapshot_datadir_contract_with_missing_policy(
        layout,
        component,
        command,
        true,
        packaged_data_probe,
    )
    .warnings
}

/// Refresh a state snapshot from ANOLISA's distribution registry.
///
/// Uses the same scoped datadir roots as install/adopt, including a relocated
/// CLI packaged tree. A missing contract is not a new warning during upgrade
/// or repair because there is no authoritative snapshot to refresh.
pub(crate) fn refresh_datadir_contract_snapshot(
    layout: &FsLayout,
    component: &str,
    command: &str,
    packaged_data_probe: &crate::packaged::PackagedDataProbe,
) -> ContractRefreshOutcome {
    snapshot_datadir_contract_with_missing_policy(
        layout,
        component,
        command,
        false,
        packaged_data_probe,
    )
}

fn snapshot_datadir_contract_with_missing_policy(
    layout: &FsLayout,
    component: &str,
    command: &str,
    warn_if_missing: bool,
    packaged_data_probe: &crate::packaged::PackagedDataProbe,
) -> ContractRefreshOutcome {
    let mut warnings: Vec<String> = Vec::new();
    let contract = match lookup_datadir_contract(layout, component, packaged_data_probe) {
        DatadirContractLookup::Found(contract) => contract,
        DatadirContractLookup::Missing(searched) => {
            if warn_if_missing {
                let paths: Vec<String> = searched
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect();
                warnings.push(format!(
                    "ANOLISA has no integration contract for component '{component}' at {}",
                    paths.join(" or ")
                ));
            }
            return ContractRefreshOutcome {
                state: ContractRefreshState::NotApplicable,
                warnings,
            };
        }
        DatadirContractLookup::Unreadable {
            source_path,
            source,
        } => {
            warnings.push(format!(
                "could not read datadir component contract at {}: {source}",
                source_path.display()
            ));
            return ContractRefreshOutcome {
                state: ContractRefreshState::Failed,
                warnings,
            };
        }
    };

    let dest = match common::installed_component_manifest_path(layout, component, command) {
        Ok(p) => p,
        Err(err) => {
            warnings.push(format!(
                "could not resolve snapshot path for component '{component}': {err}"
            ));
            return ContractRefreshOutcome {
                state: ContractRefreshState::Failed,
                warnings,
            };
        }
    };

    let provenance = ContractProvenance {
        schema_version: 1,
        source_kind: ContractSourceKind::Datadir,
        source_path: contract.source_path,
        datadir_root: contract.datadir_root,
    };
    if let Err(msg) = publish_contract_pair(&dest, &contract.content, &provenance) {
        crate::progress::suspend_output(|| eprintln!("warning: {msg}"));
        warnings.push(msg);
        return ContractRefreshOutcome {
            state: ContractRefreshState::Failed,
            warnings,
        };
    }

    ContractRefreshOutcome {
        state: ContractRefreshState::Refreshed,
        warnings,
    }
}

fn publish_contract_pair(
    snapshot_path: &Path,
    snapshot_content: &str,
    provenance: &ContractProvenance,
) -> Result<(), String> {
    let previous_snapshot = match std::fs::read(snapshot_path) {
        Ok(content) => Some(content),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(format!(
                "failed to preserve existing component contract at {}: {err}",
                snapshot_path.display()
            ));
        }
    };
    let provenance_content = toml::to_string_pretty(provenance).map_err(|err| {
        format!(
            "failed to serialize contract provenance for {}: {err}",
            snapshot_path.display()
        )
    })?;

    write_atomic_text(snapshot_path, snapshot_content).map_err(|err| {
        format!(
            "failed to snapshot component contract to {}: {err}",
            snapshot_path.display()
        )
    })?;

    let provenance_path = FsLayout::provenance_path_for_snapshot(snapshot_path);
    if let Err(err) = write_atomic_text(&provenance_path, &provenance_content) {
        let rollback = restore_snapshot(snapshot_path, previous_snapshot.as_deref());
        let rollback_detail = match rollback {
            Ok(()) => "the previous snapshot was restored".to_string(),
            Err(rollback_err) => format!("snapshot rollback also failed: {rollback_err}"),
        };
        return Err(format!(
            "failed to publish contract provenance at {}: {err}; {rollback_detail}",
            provenance_path.display()
        ));
    }

    Ok(())
}

fn restore_snapshot(path: &Path, previous: Option<&[u8]>) -> std::io::Result<()> {
    match previous {
        Some(content) => write_atomic(path, content),
        None => match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        },
    }
}

pub(crate) fn write_atomic_text(path: &Path, content: &str) -> std::io::Result<()> {
    write_atomic(path, content.as_bytes())
}

fn write_atomic(path: &Path, content: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        create_install_dir_all(parent)?;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("component.toml");
    let nanos = Utc::now().timestamp_nanos_opt().unwrap_or_default();
    let tmp = parent.join(format!(".{name}.tmp-{}-{nanos}", std::process::id()));

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o644);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(content)?;
    drop(file);
    if let Err(err) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

pub(crate) fn rollback_installed_manifest(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// ISO 8601 UTC timestamp with second precision.
pub(crate) fn now_iso8601() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anolisa_core::{FakeServiceManager, ServiceOp, ServiceRunOutcome};

    #[test]
    fn rollback_activated_services_only_cleans_touched_units() {
        let manager = FakeServiceManager::new();
        let service_run = ServiceRunOutcome {
            enabled_units: vec!["enabled.service".to_string()],
            started_units: vec!["started.service".to_string(), "enabled.service".to_string()],
            warnings: Vec::new(),
        };

        let warnings =
            rollback_activated_services(&manager, &service_run, None, "agentsight", "op", "system");

        assert!(warnings.is_empty());
        assert_eq!(
            manager.calls(),
            vec![
                (ServiceOp::Stop, "enabled.service".to_string()),
                (ServiceOp::Disable, "enabled.service".to_string()),
                (ServiceOp::Stop, "started.service".to_string()),
                (ServiceOp::Disable, "started.service".to_string()),
            ]
        );
    }
}

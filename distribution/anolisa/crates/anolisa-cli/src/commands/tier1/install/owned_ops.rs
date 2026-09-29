//! [`OwnedOps`] ports over the raw backend.
//!
//! [`RawReplayOps`] serves replay plans — re-placing an owned installation at
//! its recorded version (backup, download+verify, remove, place,
//! capabilities, restart, record). [`RawTeardownOps`] serves uninstall plans
//! (X1: hooks, stop services, remove files, drop record). Both adapt the raw
//! backend's existing primitives — the download cache, the install runner,
//! capability/service application, and the v5 state store — to the owned
//! executor's step vocabulary, and hold the working state their steps share.
//!
//! Only each plan family's subset is wired. Steps a plan never contains
//! return an honest error instead of a silent no-op, so a future plan that
//! reaches them fails loudly at the exact step rather than committing a
//! record that lies.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anolisa_core::central_log::CentralLog;
use anolisa_core::domain::{
    Installation, InstallationScope, LifecycleStatus, OwnedArtifact, ProviderBinding,
};
use anolisa_core::facts::JournalInventory;
use anolisa_core::install_runner::{InstallRunner, InstalledFile, PreparedFileSet};
use anolisa_core::lifecycle::prepare_backup;
use anolisa_core::owned_executor::{OwnedOpError, OwnedOps, StepSuccess};
use anolisa_core::path_safety::{lexical_roots, validate_owned_path};
use anolisa_core::planner::{HookKind, RecordWrite};
use anolisa_core::state::{FileOwner, ObjectKind, OwnedFile, OwnedFileKind, ServiceRef};
use anolisa_core::state_store::StateStore;
use anolisa_core::transaction::restore_backup_file;
use anolisa_core::{
    CapabilityRequest, FileKind, ResolvedLifecycleHooks, ServiceActivation, ServiceRequest,
    ServiceRunOutcome, ServiceScope, apply_capabilities, apply_services,
    capability_for_install_mode, deactivate_services, run_hooks, service_for_install_mode,
    user_service_for_install_mode,
};
use anolisa_platform::fs_layout::FsLayout;

use crate::context::CliContext;
use crate::response::CliError;

use super::io_util::{
    rollback_activated_services, rollback_installed_files, rollback_installed_manifest,
    service_cleanup_suffix, write_installed_component_manifest,
};
use super::provision::{retained_packages_note, run_provision};
use super::raw::{InstallHooks, prepare_raw_execution, resolve_install_hooks};
use super::render::artifact_type_wire;
use super::types::{PreparedInstall, RawResolution};

/// Late-bound effect backends shared by raw execution and compensation.
#[derive(Clone, Copy)]
pub(crate) struct RawEffectFactories<'a> {
    /// Select the existing service backend for the requested scope.
    pub(crate) service: &'a dyn Fn(
        &str,
        &anolisa_env::EnvFacts,
        ServiceScope,
    ) -> Box<dyn anolisa_core::ServiceManager>,
    /// Select the existing capability backend without changing support policy.
    pub(crate) capability:
        &'a dyn Fn(&str, &anolisa_env::EnvFacts) -> Box<dyn anolisa_core::CapabilityManager>,
}

impl RawEffectFactories<'static> {
    /// Production factories remain lazy until support observation or execution.
    pub(crate) fn system() -> Self {
        Self {
            service: &|mode, env, scope| match scope {
                ServiceScope::System => service_for_install_mode(mode, env),
                ServiceScope::User => user_service_for_install_mode(mode, env),
            },
            capability: &capability_for_install_mode,
        }
    }
}

impl RawEffectFactories<'_> {
    /// Use the execution backend's support policy when inferring legacy grants.
    pub(crate) fn hydrate_owned_file_contracts(
        self,
        store: &mut StateStore,
        layout: &FsLayout,
    ) -> usize {
        let env = anolisa_env::EnvService::detect();
        let install_mode = match layout.mode {
            anolisa_platform::fs_layout::InstallMode::System => "system",
            anolisa_platform::fs_layout::InstallMode::User => "user",
        };
        let supported = (self.capability)(install_mode, &env).supported();
        crate::commands::common::hydrate_owned_file_contracts_with_capability_support(
            store, layout, supported,
        )
    }
}

struct ReplayBackup {
    source: PathBuf,
    dest: PathBuf,
    sha256: Option<String>,
    /// Permission bits `dest` carried before the replay, so restore puts
    /// the file back executable if it was executable.
    mode: Option<u32>,
}

fn rollback_capability_requests(
    prior_files: &[OwnedFile],
    backups: &[ReplayBackup],
) -> Vec<CapabilityRequest> {
    prior_files
        .iter()
        .filter(|file| {
            file.owner == FileOwner::Anolisa
                && file.kind != OwnedFileKind::Symlink
                && !file.capabilities.is_empty()
                && backups.iter().any(|backup| backup.dest == file.path)
        })
        .map(|file| CapabilityRequest {
            path: file.path.clone(),
            caps: file.capabilities.clone(),
            // A recorded grant was confirmed during the prior installation;
            // rollback must surface failure to restore that contract.
            optional: false,
        })
        .collect()
}

/// Raw-backend [`OwnedOps`] for one replay operation.
pub(crate) struct RawReplayOps<'a> {
    ctx: &'a CliContext,
    layout: &'a FsLayout,
    component: String,
    scope: InstallationScope,
    now: String,
    operation_id: String,
    env: anolisa_env::EnvFacts,
    effects: RawEffectFactories<'a>,
    log: CentralLog,
    /// Resolution to download from; consumed by [`OwnedOps::download_verify`].
    resolution: Option<RawResolution>,
    /// The record's artifact at plan time: what to back up, remove, and
    /// preserve (provisioned packages, service enablement) across the replay.
    prior: OwnedArtifact,
    /// Prepared artifact + resolved contract, set by `download_verify`.
    prepared: Option<PreparedInstall>,
    prepared_files: Option<PreparedFileSet>,
    /// Files this run placed, set by `place_files`.
    placed: Vec<InstalledFile>,
    /// Manifest snapshot this run wrote, set by `place_files`.
    manifest_path: Option<PathBuf>,
    /// Capability assignments the backend confirmed this run.
    applied_capabilities: Vec<CapabilityRequest>,
    backups: Vec<ReplayBackup>,
    backup_root: PathBuf,
    /// Activation result, read back by the record commit.
    service_run: Option<ServiceRunOutcome>,
    /// Probe the new artifact's runtime dependencies during download-verify.
    runtime_preflight: bool,
    store: &'a mut StateStore,
    state_path: &'a Path,
}

impl<'a> RawReplayOps<'a> {
    /// Bind the port to one operation. `prior` is the record's artifact as
    /// re-validated under the install lock — its file list drives backup and
    /// removal, so it must never come from a pre-lock snapshot.
    #[expect(clippy::too_many_arguments)]
    pub(crate) fn new(
        ctx: &'a CliContext,
        effects: RawEffectFactories<'a>,
        layout: &'a FsLayout,
        component: String,
        scope: InstallationScope,
        now: String,
        operation_id: String,
        resolution: RawResolution,
        prior: OwnedArtifact,
        store: &'a mut StateStore,
        state_path: &'a Path,
    ) -> Self {
        let backup_root = layout.backup_dir.join(&operation_id);
        Self {
            ctx,
            layout,
            component,
            scope,
            now,
            env: anolisa_env::EnvService::detect(),
            effects,
            log: CentralLog::open(layout.central_log.clone()),
            resolution: Some(resolution),
            prior,
            prepared: None,
            prepared_files: None,
            placed: Vec::new(),
            manifest_path: None,
            applied_capabilities: Vec::new(),
            backups: Vec::new(),
            backup_root,
            service_run: None,
            runtime_preflight: false,
            store,
            state_path,
            operation_id,
        }
    }

    /// Probe the new artifact's runtime dependencies during download-verify.
    ///
    /// Update plans opt in: a newer artifact may declare dependencies the
    /// installed version did not, and replacing files on a host that misses
    /// them strands the component exactly like a fresh install would. Replay
    /// plans skip it — the recorded version's dependencies were satisfied
    /// when it was installed.
    pub(crate) fn with_runtime_preflight(mut self) -> Self {
        self.runtime_preflight = true;
        self
    }

    /// Remove the per-operation backup scratch. Call after the plan
    /// committed; a failed plan keeps its backups on disk for forensics.
    pub(crate) fn discard_backups(&self) {
        let _ = fs::remove_dir_all(&self.backup_root);
    }

    fn prepared(&self) -> Result<&PreparedInstall, OwnedOpError> {
        self.prepared.as_ref().ok_or_else(|| {
            OwnedOpError("internal: step ran before the download-verify step".to_string())
        })
    }

    /// Service rows for the record. Restart does not change enablement, so
    /// each unit keeps the enabled flag the prior record gave it.
    fn service_refs(&self, services: &[ServiceRequest]) -> Vec<ServiceRef> {
        services
            .iter()
            .map(|svc| ServiceRef {
                name: svc.unit.clone(),
                manager: svc.scope.manager_label().to_string(),
                restartable: true,
                enabled: self
                    .prior
                    .services
                    .iter()
                    .any(|prior| prior.name == svc.unit && prior.enabled),
                scope: svc.scope,
            })
            .collect()
    }

    /// Re-apply the file capabilities recorded as active before the replay.
    ///
    /// Restoring a file writes a new inode, and a new inode carries no
    /// `security.capability` xattr — so a rollback silently strips the
    /// capabilities the installed version was running with, the same way it
    /// used to strip the executable bit. Ownership of that repair belongs
    /// here rather than in a compensation of its own: capabilities attach to
    /// the file, so they can only be re-applied once the file is back, and
    /// `restore_backup` is the last compensation the executor replays.
    ///
    /// Only grants confirmed in the prior state are restored. Reconstructing
    /// requests from the manifest could grant an optional capability that
    /// failed during the original installation, expanding privilege during
    /// a failed operation.
    fn reapply_prior_capabilities(&self) -> Vec<String> {
        let requests = rollback_capability_requests(&self.prior.files, &self.backups);
        if requests.is_empty() {
            return Vec::new();
        }

        let manager = (self.effects.capability)(self.ctx.install_mode.as_str(), &self.env);
        let outcome = apply_capabilities(
            manager.as_ref(),
            &requests,
            Some(&self.log),
            &self.component,
            &self.operation_id,
            "cli",
            self.ctx.install_mode.as_str(),
        );
        let mut warnings = outcome.warnings;
        // A required capability that would not apply aborts the *install*
        // path; during rollback there is nothing left to abort, so it
        // downgrades to a warning naming what the restored file lost.
        if let Some(reason) = outcome.aborted {
            warnings.push(format!(
                "restored files but could not re-apply their file capabilities: {reason}"
            ));
        }
        warnings
    }

    fn not_wired(what: &str) -> Result<StepSuccess, OwnedOpError> {
        Err(OwnedOpError(format!(
            "{what} is not wired for owned replay yet"
        )))
    }
}

impl OwnedOps for RawReplayOps<'_> {
    fn download_verify(&mut self) -> Result<StepSuccess, OwnedOpError> {
        let resolution = self.resolution.take().ok_or_else(|| {
            OwnedOpError("internal: download-verify ran twice in one plan".to_string())
        })?;
        let prepared = prepare_raw_execution(self.ctx, self.layout, resolution)
            .map_err(|err| OwnedOpError(err.to_string()))?;
        let prepared_files = InstallRunner::new(self.layout)
            .prepare_replacement_files(
                artifact_type_wire(&prepared.resolution.entry.artifact_type),
                &prepared.artifact_path,
                &prepared.files,
            )
            .map_err(|err| OwnedOpError(format!("failed to inspect verified payload: {err}")))?;
        let mut warnings = Vec::new();
        if self.runtime_preflight {
            let manifest = anolisa_core::ComponentManifest::from_toml_str(&prepared.manifest_toml)
                .map_err(|err| {
                    OwnedOpError(format!(
                        "failed to parse component manifest for preflight: {err}"
                    ))
                })?;
            warnings = super::provision::run_runtime_preflight(&manifest, &self.env, "update")
                .map_err(|err| OwnedOpError(err.reason()))?;
        }
        self.prepared = Some(prepared);
        self.prepared_files = Some(prepared_files);
        Ok(StepSuccess::with_warnings(warnings))
    }

    fn provision_runtime_deps(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("runtime-dependency provisioning")
    }

    fn run_hook(&mut self, kind: HookKind) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired(&format!("the {kind:?} hook phase"))
    }

    fn backup_files(&mut self) -> Result<StepSuccess, OwnedOpError> {
        for (idx, file) in self.prior.files.iter().enumerate() {
            validate_owned_path(self.layout, &file.path).map_err(|err| {
                OwnedOpError(format!(
                    "recorded owned file {} is outside ANOLISA-owned roots: {err}",
                    file.path.display()
                ))
            })?;
            let backup_path = self.backup_root.join(format!("{idx}.bak"));
            match prepare_backup(&file.path, &backup_path) {
                Ok(Some(artifact)) => self.backups.push(ReplayBackup {
                    source: backup_path,
                    dest: file.path.clone(),
                    mode: artifact.mode(),
                    sha256: artifact.into_sha256(),
                }),
                // Already gone from disk — nothing to preserve; placement
                // recreates it.
                Ok(None) => {}
                Err(err) => {
                    return Err(OwnedOpError(format!(
                        "failed to back up {}: {err}",
                        file.path.display()
                    )));
                }
            }
        }
        Ok(StepSuccess::clean())
    }

    fn place_files(&mut self) -> Result<StepSuccess, OwnedOpError> {
        let prepared_files = self.prepared_files.take().ok_or_else(|| {
            OwnedOpError("internal: placement ran before payload preparation".to_string())
        })?;
        let runner = InstallRunner::new(self.layout);
        let outcome = runner
            .install_prepared(prepared_files)
            .map_err(|err| OwnedOpError(format!("placing files failed: {err}")))?;
        let prepared = self.prepared()?;
        match write_installed_component_manifest(
            self.layout,
            &self.component,
            &prepared.manifest_toml,
        ) {
            Ok(path) => {
                self.placed = outcome.files;
                self.manifest_path = Some(path);
                Ok(StepSuccess::clean())
            }
            Err(err) => {
                // The step did not complete: clean up its own partial work so
                // the executor never registers an undo for half-placed files.
                rollback_installed_files(&outcome.files);
                Err(OwnedOpError(err.to_string()))
            }
        }
    }

    fn set_capabilities(&mut self) -> Result<StepSuccess, OwnedOpError> {
        let prepared = self.prepared()?;
        let manager = (self.effects.capability)(self.ctx.install_mode.as_str(), &self.env);
        let outcome = apply_capabilities(
            manager.as_ref(),
            &prepared.capabilities,
            Some(&self.log),
            &self.component,
            &self.operation_id,
            "cli",
            self.ctx.install_mode.as_str(),
        );
        if let Some(reason) = outcome.aborted {
            return Err(OwnedOpError(format!(
                "required capability application failed: {reason}"
            )));
        }
        self.applied_capabilities = outcome.applied_requests;
        Ok(StepSuccess::with_warnings(outcome.warnings))
    }

    fn enable_services(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("service enablement")
    }

    fn restart_services(&mut self) -> Result<StepSuccess, OwnedOpError> {
        let prepared = self.prepared()?;
        let services = prepared.services.clone();
        let mode = self.ctx.install_mode.as_str();
        // Scope-matched backend, as install/update pick it: an all-user-scope
        // contract restarts through `systemctl --user`.
        let manager: Box<dyn anolisa_core::ServiceManager> =
            if !services.is_empty() && services.iter().all(|s| s.scope == ServiceScope::User) {
                (self.effects.service)(mode, &self.env, ServiceScope::User)
            } else {
                (self.effects.service)(mode, &self.env, ServiceScope::System)
            };
        let run = apply_services(
            manager.as_ref(),
            &services,
            ServiceActivation::Restart,
            Some(&self.log),
            &self.component,
            &self.operation_id,
            "cli",
            mode,
        );
        let warnings = run.warnings.clone();
        self.service_run = Some(run);
        Ok(StepSuccess::with_warnings(warnings))
    }

    fn stop_services(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("service stop")
    }

    fn remove_owned_files(&mut self) -> Result<StepSuccess, OwnedOpError> {
        for file in &self.prior.files {
            match fs::remove_file(&file.path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(OwnedOpError(format!(
                        "failed to remove {}: {err}",
                        file.path.display()
                    )));
                }
            }
        }
        Ok(StepSuccess::clean())
    }

    fn write_record(&mut self, write: RecordWrite) -> Result<StepSuccess, OwnedOpError> {
        if write != RecordWrite::Owned {
            return Err(OwnedOpError(format!(
                "raw replay cannot write a {} record",
                write.label()
            )));
        }
        let prepared = self.prepared()?;
        let manifest_path = self.manifest_path.clone().ok_or_else(|| {
            OwnedOpError("internal: record commit ran before files were placed".to_string())
        })?;
        let artifact = OwnedArtifact {
            version: prepared.resolution.entry.version.clone(),
            distribution_source: Some(prepared.resolution.artifact_url.clone()),
            raw_package: Some(prepared.resolution.package.clone()),
            // Digest verification of the embedded manifest is future work;
            // recording an unverified digest would overstate what ran.
            manifest_digest: None,
            files: owned_file_rows(
                &self.placed,
                &manifest_path,
                &prepared.manifest_toml,
                &self.applied_capabilities,
            ),
            services: self.service_refs(&prepared.services),
            // A clean replay leaves no externally-modified files behind.
            external_modified_files: Vec::new(),
            // Provisioning did not run; the packages the original install
            // provisioned are still this installation's responsibility.
            provisioned_packages: self.prior.provisioned_packages.clone(),
        };

        match self.store.find_mut(ObjectKind::Component, &self.component) {
            Some(existing) => {
                existing.binding = ProviderBinding::Owned { artifact };
                existing.status = LifecycleStatus::Installed;
                existing.last_operation_id = Some(self.operation_id.clone());
                existing.health = Vec::new();
            }
            None => {
                self.store.upsert(Installation {
                    kind: ObjectKind::Component,
                    name: self.component.clone(),
                    scope: self.scope,
                    binding: ProviderBinding::Owned { artifact },
                    status: LifecycleStatus::Installed,
                    installed_at: self.now.clone(),
                    last_operation_id: Some(self.operation_id.clone()),
                    subscription_scope: Default::default(),
                    enabled_features: Vec::new(),
                    health: Vec::new(),
                });
            }
        }
        self.store
            .save(self.state_path)
            .map_err(|err| OwnedOpError(format!("failed to save state: {err}")))?;
        Ok(StepSuccess::clean())
    }

    fn drop_record(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("record removal")
    }

    fn undo_place_files(&mut self) -> Vec<String> {
        rollback_installed_files(&self.placed);
        self.placed.clear();
        if let Some(path) = self.manifest_path.take() {
            rollback_installed_manifest(&path);
        }
        Vec::new()
    }

    fn undo_enable_services(&mut self) -> Vec<String> {
        // Replay plans restart services, they never enable them, so the
        // executor never registers this compensation against this port.
        Vec::new()
    }

    fn restore_backup(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();
        for backup in &self.backups {
            match restore_backup_file(
                &backup.source,
                &backup.dest,
                backup.sha256.as_deref(),
                backup.mode,
            ) {
                Ok(restore_warnings) => warnings.extend(restore_warnings),
                Err(err) => warnings.push(format!(
                    "failed to restore {} from its backup: {err}",
                    backup.dest.display()
                )),
            }
        }
        // The restored inodes carry no capability xattrs; put the prior
        // contract's back before the executor calls the rollback done.
        if !self.backups.is_empty() {
            warnings.extend(self.reapply_prior_capabilities());
        }
        warnings
    }
}

/// Raw-backend [`OwnedOps`] for one uninstall (X1) operation: contract
/// hooks, service stop/disable, owned-file removal, and the record drop.
pub(crate) struct RawTeardownOps<'a> {
    layout: &'a FsLayout,
    component: String,
    install_mode: String,
    operation_id: String,
    env: anolisa_env::EnvFacts,
    effects: RawEffectFactories<'a>,
    log: CentralLog,
    /// The record's artifact as re-validated under the install lock: its
    /// file list drives removal and its service list drives the stop.
    prior: OwnedArtifact,
    /// Pre/post-uninstall hooks resolved from the installed manifest
    /// snapshot (best-effort: a missing snapshot means no hooks).
    hooks: ResolvedLifecycleHooks,
    store: &'a mut StateStore,
    state_path: &'a Path,
}

impl<'a> RawTeardownOps<'a> {
    #[expect(clippy::too_many_arguments)]
    pub(crate) fn new(
        ctx: &CliContext,
        effects: RawEffectFactories<'a>,
        layout: &'a FsLayout,
        component: String,
        operation_id: String,
        prior: OwnedArtifact,
        hooks: ResolvedLifecycleHooks,
        store: &'a mut StateStore,
        state_path: &'a Path,
    ) -> Self {
        Self {
            layout,
            component,
            install_mode: ctx.install_mode.as_str().to_string(),
            operation_id,
            env: anolisa_env::EnvService::detect(),
            effects,
            log: CentralLog::open(layout.central_log.clone()),
            prior,
            hooks,
            store,
            state_path,
        }
    }

    /// Turn a hook-run result into the step result: a strict failure is an
    /// `Err`, everything else succeeds with the collected warnings.
    fn hook_step(&self, run: anolisa_core::HookRunResult) -> Result<StepSuccess, OwnedOpError> {
        if let Some(failure) = run.hard_failure {
            return Err(OwnedOpError(failure.summary()));
        }
        Ok(StepSuccess::with_warnings(run.warnings))
    }

    fn not_wired(what: &str) -> Result<StepSuccess, OwnedOpError> {
        Err(OwnedOpError(format!(
            "{what} is not wired for owned teardown yet"
        )))
    }
}

impl OwnedOps for RawTeardownOps<'_> {
    fn download_verify(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("artifact download")
    }

    fn provision_runtime_deps(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("runtime-dependency provisioning")
    }

    fn run_hook(&mut self, kind: HookKind) -> Result<StepSuccess, OwnedOpError> {
        let specs = match kind {
            HookKind::PreUninstall => &self.hooks.pre_uninstall,
            HookKind::PostUninstall => &self.hooks.post_uninstall,
            other => return Self::not_wired(&format!("the {other:?} hook phase")),
        };
        let run = run_hooks(
            specs,
            self.layout,
            Some(&self.log),
            &self.operation_id,
            "cli",
            &self.install_mode,
        );
        self.hook_step(run)
    }

    fn backup_files(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("file backup")
    }

    fn place_files(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("file placement")
    }

    fn set_capabilities(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("capability application")
    }

    fn enable_services(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("service enablement")
    }

    fn restart_services(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("service restart")
    }

    /// Stop AND disable every recorded unit before the files go, so a
    /// running daemon shuts down cleanly and no orphan `enabled` symlink
    /// survives the uninstall. Best-effort by convention: trouble surfaces
    /// as warnings, never as a failed uninstall.
    fn stop_services(&mut self) -> Result<StepSuccess, OwnedOpError> {
        let mut sys_units: Vec<(String, String)> = Vec::new();
        let mut user_units: Vec<(String, String)> = Vec::new();
        for service in &self.prior.services {
            let row = (self.component.clone(), service.name.clone());
            match service.scope {
                ServiceScope::User => user_units.push(row),
                ServiceScope::System => sys_units.push(row),
            }
        }
        let mut warnings = Vec::new();
        for (units, manager) in [
            (
                sys_units,
                (self.effects.service)(&self.install_mode, &self.env, ServiceScope::System),
            ),
            (
                user_units,
                (self.effects.service)(&self.install_mode, &self.env, ServiceScope::User),
            ),
        ] {
            if units.is_empty() {
                continue;
            }
            let outcome = deactivate_services(
                manager.as_ref(),
                &units,
                Some(&self.log),
                &self.operation_id,
                "cli",
                &self.install_mode,
            );
            warnings.extend(outcome.warnings);
        }
        Ok(StepSuccess::with_warnings(warnings))
    }

    /// Remove the recorded owned files. A path outside the ANOLISA-owned
    /// roots is skipped with a warning instead of failing the teardown: a
    /// forged or stale record must not turn uninstall into an
    /// arbitrary-delete primitive, and skipping keeps the operation moving
    /// on the legitimate files.
    fn remove_owned_files(&mut self) -> Result<StepSuccess, OwnedOpError> {
        let mut warnings = Vec::new();
        let mut removed_parents: Vec<PathBuf> = Vec::new();
        for file in &self.prior.files {
            if let Err(boundary) = validate_owned_path(self.layout, &file.path) {
                warnings.push(format!(
                    "skipped {}: outside ANOLISA-owned roots ({boundary})",
                    file.path.display()
                ));
                continue;
            }
            match fs::remove_file(&file.path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(OwnedOpError(format!(
                        "failed to remove {}: {err}",
                        file.path.display()
                    )));
                }
            }
            if let Some(parent) = file.path.parent() {
                removed_parents.push(parent.to_path_buf());
            }
        }
        prune_emptied_dirs(self.layout, removed_parents);
        Ok(StepSuccess::with_warnings(warnings))
    }

    fn write_record(&mut self, _write: RecordWrite) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("record write")
    }

    fn drop_record(&mut self) -> Result<StepSuccess, OwnedOpError> {
        self.store.remove(ObjectKind::Component, &self.component);
        self.store
            .save(self.state_path)
            .map_err(|err| OwnedOpError(format!("failed to save state: {err}")))?;
        // The manifest snapshot directory travels with the record; its file
        // was already removed with the owned files, the empty directory goes
        // here. Best-effort: the record is already dropped and committed.
        let mut warnings = Vec::new();
        if let Ok(dir) = crate::commands::common::installed_component_manifest_dir(
            self.layout,
            &self.component,
            "uninstall",
        ) && let Err(err) = fs::remove_dir_all(&dir)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            warnings.push(format!(
                "failed to remove component manifest snapshot at {}: {err}",
                dir.display()
            ));
        }
        Ok(StepSuccess::with_warnings(warnings))
    }

    fn undo_place_files(&mut self) -> Vec<String> {
        // X1 plans never place files, so this compensation is never
        // registered against this port.
        Vec::new()
    }

    fn undo_enable_services(&mut self) -> Vec<String> {
        Vec::new()
    }

    fn restore_backup(&mut self) -> Vec<String> {
        Vec::new()
    }
}

/// Best-effort removal of directories a file teardown emptied, walking
/// each start directory upward. Without this, an uninstall leaves a bare
/// directory skeleton behind, and a skeleton under one scope's datadir
/// (e.g. `/usr/local/share/anolisa/extensions/<component>/`) can shadow
/// adapter source probing for the same component in another scope.
///
/// Safety: `fs::remove_dir` refuses non-empty directories, and the climb
/// stops at the *deepest* owned root containing the start path. The
/// boundary must be per-path, not "parent is owned": owned roots can nest
/// (user mode places `libexec_dir` inside `lib_dir`), so a parent check
/// alone would delete the inner root itself. Errors are swallowed:
/// pruning is cosmetic and must never fail an otherwise-successful
/// teardown.
fn prune_emptied_dirs(layout: &FsLayout, start_dirs: Vec<PathBuf>) {
    let roots = lexical_roots(layout);
    for start in start_dirs {
        // Deepest owned root containing this path — the hard boundary
        // that must survive the climb.
        let Some(boundary) = roots
            .iter()
            .filter(|root| start.starts_with(root))
            .max_by_key(|root| root.components().count())
        else {
            continue;
        };
        let mut dir = start;
        while dir != **boundary {
            if validate_owned_path(layout, &dir).is_err() {
                break;
            }
            match fs::remove_dir(&dir) {
                Ok(()) => {}
                // Already pruned via another file's parent chain — keep
                // climbing so shared ancestors are still considered.
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                // Non-empty or unremovable: everything above is also
                // non-empty, stop here.
                Err(_) => break,
            }
            let Some(parent) = dir.parent() else { break };
            dir = parent.to_path_buf();
        }
    }
}

/// [`OwnedOps`] for the quarantine-restore exit (repair R6): a quarantined
/// legacy record whose owned files verified intact is rebuilt into an active
/// owned record. The only wired step is the record write — the files are
/// already on disk and already verified, so nothing else may touch the host.
pub(crate) struct QuarantineRestoreOps<'a> {
    component: String,
    scope: InstallationScope,
    operation_id: String,
    store: &'a mut StateStore,
    state_path: &'a Path,
}

impl<'a> QuarantineRestoreOps<'a> {
    pub(crate) fn new(
        component: String,
        scope: InstallationScope,
        operation_id: String,
        store: &'a mut StateStore,
        state_path: &'a Path,
    ) -> Self {
        Self {
            component,
            scope,
            operation_id,
            store,
            state_path,
        }
    }

    fn not_wired(what: &str) -> Result<StepSuccess, OwnedOpError> {
        Err(OwnedOpError(format!(
            "{what} is not wired for quarantine restore"
        )))
    }
}

impl OwnedOps for QuarantineRestoreOps<'_> {
    fn download_verify(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("artifact download")
    }

    fn provision_runtime_deps(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("runtime-dependency provisioning")
    }

    fn run_hook(&mut self, kind: HookKind) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired(&format!("the {kind:?} hook phase"))
    }

    fn backup_files(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("file backup")
    }

    fn place_files(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("file placement")
    }

    fn set_capabilities(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("capability application")
    }

    fn enable_services(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("service enablement")
    }

    fn restart_services(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("service restart")
    }

    fn stop_services(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("service stop")
    }

    fn remove_owned_files(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("owned-file removal")
    }

    fn write_record(&mut self, write: RecordWrite) -> Result<StepSuccess, OwnedOpError> {
        if write != RecordWrite::Owned {
            return Err(OwnedOpError(format!(
                "quarantine restore cannot write a {} record",
                write.label()
            )));
        }
        let quarantined = self
            .store
            .quarantined
            .iter()
            .find(|q| q.record.kind == ObjectKind::Component && q.record.name == self.component)
            .ok_or_else(|| {
                OwnedOpError(format!(
                    "no quarantined record for '{}' remains to restore",
                    self.component
                ))
            })?;
        let mut installation =
            anolisa_core::state_migration::owned_installation(&quarantined.record, self.scope);
        // The restore is itself the verdict that this installation is
        // healthy again — the legacy status must not survive it.
        installation.status = LifecycleStatus::Installed;
        installation.last_operation_id = Some(self.operation_id.clone());
        // Upsert consumes the same-identity quarantined record.
        self.store.upsert(installation);
        self.store
            .save(self.state_path)
            .map_err(|err| OwnedOpError(format!("failed to save state: {err}")))?;
        Ok(StepSuccess::clean())
    }

    fn drop_record(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("record removal")
    }

    fn undo_place_files(&mut self) -> Vec<String> {
        Vec::new()
    }

    fn undo_enable_services(&mut self) -> Vec<String> {
        Vec::new()
    }

    fn restore_backup(&mut self) -> Vec<String> {
        Vec::new()
    }
}

/// A fresh install's artifact and contract, downloaded, verified, and
/// validated before any lock or journal exists.
pub(crate) struct ValidatedInstall {
    prepared: PreparedInstall,
    prepared_files: PreparedFileSet,
    manifest: anolisa_core::ComponentManifest,
    hooks: InstallHooks,
}

impl ValidatedInstall {
    /// Package name the resolution settled on.
    pub(crate) fn package(&self) -> &str {
        &self.prepared.resolution.package
    }

    /// Version the resolution settled on.
    pub(crate) fn version(&self) -> &str {
        &self.prepared.resolution.entry.version
    }

    /// Warnings accumulated during resolution.
    pub(crate) fn warnings(&self) -> &[String] {
        &self.prepared.resolution.warnings
    }
}

/// Download+verify the resolved artifact and validate its install contract —
/// install modes, component conflicts, hook declarations — with typed
/// errors. Runs before the install lock: everything here is side-effect free
/// outside the download cache, and a contract refusal must surface as
/// INVALID_ARGUMENT rather than a failed transaction.
pub(crate) fn validate_owned_install(
    ctx: &CliContext,
    layout: &FsLayout,
    store: &StateStore,
    resolution: RawResolution,
    command: &str,
) -> Result<ValidatedInstall, CliError> {
    let component = resolution.component.clone();
    let prepared = prepare_raw_execution(ctx, layout, resolution)?;
    let manifest = anolisa_core::ComponentManifest::from_toml_str(&prepared.manifest_toml)
        .map_err(|err| CliError::Runtime {
            command: command.to_string(),
            reason: format!("failed to parse component manifest: {err}"),
        })?;
    validate_component_conflict(&manifest, store, &HashSet::new(), command)?;
    let hooks = resolve_install_hooks(&manifest, layout, &component)?;
    let prepared_files = InstallRunner::new(layout)
        .prepare_files(
            artifact_type_wire(&prepared.resolution.entry.artifact_type),
            &prepared.artifact_path,
            &prepared.files,
        )
        .map_err(|err| CliError::Runtime {
            command: command.to_string(),
            reason: format!("failed to inspect verified install payload: {err}"),
        })?;
    Ok(ValidatedInstall {
        prepared,
        prepared_files,
        manifest,
        hooks,
    })
}

/// Reject an incoming component whose manifest conflicts with active state
/// or an earlier successful member of the same dry-run batch.
pub(crate) fn validate_component_conflict(
    manifest: &anolisa_core::ComponentManifest,
    store: &StateStore,
    planned_components: &HashSet<String>,
    command: &str,
) -> Result<(), CliError> {
    if let Some(reason) = component_conflict(manifest, store) {
        return Err(CliError::InvalidArgument {
            command: command.to_string(),
            reason,
        });
    }
    for conflict in &manifest.component.conflicts {
        if planned_components.contains(conflict) {
            return Err(CliError::InvalidArgument {
                command: command.to_string(),
                reason: format!(
                    "component '{}' conflicts with component '{}' planned earlier in this batch — remove one component from the batch, then retry",
                    manifest.component.name, conflict
                ),
            });
        }
    }
    Ok(())
}

/// Component-level mutual exclusion (the raw equivalent of RPM's
/// `Conflicts:` tag): the refusal message when the contract conflicts with
/// an installed component.
fn component_conflict(
    manifest: &anolisa_core::ComponentManifest,
    store: &StateStore,
) -> Option<String> {
    for conflict in &manifest.component.conflicts {
        if let Some(installed) = store.find(ObjectKind::Component, conflict) {
            return Some(format!(
                "component '{}' conflicts with installed component '{}' (v{}) — uninstall '{}' first, then retry",
                manifest.component.name,
                conflict,
                installed_version_label(installed),
                conflict,
            ));
        }
    }
    None
}

/// [`OwnedOps`] for one fresh owned install (I1): download+verify (with
/// conflict and hook-contract validation), runtime-dependency provisioning,
/// pre/post-install hooks, file placement, capabilities, service activation,
/// and the record write.
///
/// The contract's `post_enable` hooks run inside the service-activation step
/// (the planner's step vocabulary has no post-enable phase): a strict
/// failure deactivates the units this step just brought up and fails the
/// step, so the executor compensates the earlier steps normally.
pub(crate) struct RawInstallOps<'a> {
    ctx: &'a CliContext,
    layout: &'a FsLayout,
    component: String,
    scope: InstallationScope,
    now: String,
    operation_id: String,
    env: anolisa_env::EnvFacts,
    effects: RawEffectFactories<'a>,
    log: CentralLog,
    /// Prepared artifact + resolved contract, validated pre-lock.
    prepared: Option<PreparedInstall>,
    prepared_files: Option<PreparedFileSet>,
    /// Parsed contract manifest, validated pre-lock.
    manifest: Option<anolisa_core::ComponentManifest>,
    /// Install hooks resolved from the contract, validated pre-lock.
    hooks: Option<InstallHooks>,
    /// System packages auto-installed by `provision_runtime_deps`.
    /// Intentionally never rolled back.
    provisioned_packages: Vec<String>,
    /// Journal inventory validated under the install lock; provisioning
    /// refuses packages a pending RPM install journal reserves.
    journals: &'a JournalInventory,
    /// Files this run placed, set by `place_files`.
    placed: Vec<InstalledFile>,
    /// Manifest snapshot this run wrote, set by `place_files`.
    manifest_path: Option<PathBuf>,
    /// Capability assignments the backend confirmed this run.
    applied_capabilities: Vec<CapabilityRequest>,
    /// Activation result, read back by the record commit and the undo.
    service_run: Option<ServiceRunOutcome>,
    store: &'a mut StateStore,
    state_path: &'a Path,
}

impl<'a> RawInstallOps<'a> {
    #[expect(clippy::too_many_arguments)]
    pub(crate) fn new(
        ctx: &'a CliContext,
        effects: RawEffectFactories<'a>,
        layout: &'a FsLayout,
        component: String,
        scope: InstallationScope,
        now: String,
        operation_id: String,
        validated: ValidatedInstall,
        store: &'a mut StateStore,
        state_path: &'a Path,
        journals: &'a JournalInventory,
    ) -> Self {
        let ValidatedInstall {
            prepared,
            prepared_files,
            manifest,
            hooks,
        } = validated;
        Self {
            ctx,
            layout,
            component,
            scope,
            now,
            env: anolisa_env::EnvService::detect(),
            effects,
            log: CentralLog::open(layout.central_log.clone()),
            prepared: Some(prepared),
            prepared_files: Some(prepared_files),
            manifest: Some(manifest),
            hooks: Some(hooks),
            provisioned_packages: Vec::new(),
            journals,
            placed: Vec::new(),
            manifest_path: None,
            applied_capabilities: Vec::new(),
            service_run: None,
            store,
            state_path,
            operation_id,
        }
    }

    /// The note appended to failure reports when system packages were
    /// provisioned but the install did not complete (they are retained).
    pub(crate) fn retained_packages_note(&self) -> String {
        retained_packages_note(&self.provisioned_packages)
    }

    fn prepared(&self) -> Result<&PreparedInstall, OwnedOpError> {
        self.prepared.as_ref().ok_or_else(|| {
            OwnedOpError("internal: step ran before the download-verify step".to_string())
        })
    }

    fn hooks(&self) -> Result<&InstallHooks, OwnedOpError> {
        self.hooks.as_ref().ok_or_else(|| {
            OwnedOpError("internal: hook step ran before the download-verify step".to_string())
        })
    }

    /// Scope-matched service manager, as install/update pick it: an
    /// all-user-scope contract drives `systemctl --user`.
    fn service_manager(
        &self,
        services: &[ServiceRequest],
    ) -> Box<dyn anolisa_core::ServiceManager> {
        let mode = self.ctx.install_mode.as_str();
        if !services.is_empty() && services.iter().all(|s| s.scope == ServiceScope::User) {
            (self.effects.service)(mode, &self.env, ServiceScope::User)
        } else {
            (self.effects.service)(mode, &self.env, ServiceScope::System)
        }
    }

    fn not_wired(what: &str) -> Result<StepSuccess, OwnedOpError> {
        Err(OwnedOpError(format!(
            "{what} is not wired for fresh install yet"
        )))
    }
}

impl OwnedOps for RawInstallOps<'_> {
    fn download_verify(&mut self) -> Result<StepSuccess, OwnedOpError> {
        // Fetch, digest check, and contract validation already ran pre-lock
        // (`validate_owned_install`). Only the conflict check repeats here,
        // against the store re-read under the lock, closing the window where
        // a conflicting component landed between validation and locking.
        let manifest = self.manifest.as_ref().ok_or_else(|| {
            OwnedOpError("internal: install ops built without a validated contract".to_string())
        })?;
        if let Some(reason) = component_conflict(manifest, self.store) {
            return Err(OwnedOpError(reason));
        }
        Ok(StepSuccess::clean())
    }

    fn provision_runtime_deps(&mut self) -> Result<StepSuccess, OwnedOpError> {
        let manifest = self.manifest.as_ref().ok_or_else(|| {
            OwnedOpError("internal: provisioning ran before the download-verify step".to_string())
        })?;
        let mut warnings = Vec::new();
        self.provisioned_packages = run_provision(
            manifest,
            &self.env,
            self.ctx,
            super::COMMAND,
            &mut warnings,
            self.journals,
            self.layout,
        )
        .map_err(|err| OwnedOpError(err.reason()))?;
        Ok(StepSuccess::with_warnings(warnings))
    }

    fn run_hook(&mut self, kind: HookKind) -> Result<StepSuccess, OwnedOpError> {
        let hooks = self.hooks()?;
        let specs = match kind {
            HookKind::PreInstall => hooks.pre_install.clone(),
            HookKind::PostInstall => hooks.post_install.clone(),
            other => return Self::not_wired(&format!("the {other:?} hook phase")),
        };
        let run = run_hooks(
            &specs,
            self.layout,
            Some(&self.log),
            &self.operation_id,
            "cli",
            self.ctx.install_mode.as_str(),
        );
        if let Some(failure) = run.hard_failure {
            return Err(OwnedOpError(failure.summary()));
        }
        Ok(StepSuccess::with_warnings(run.warnings))
    }

    fn backup_files(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("file backup")
    }

    fn place_files(&mut self) -> Result<StepSuccess, OwnedOpError> {
        let prepared_files = self.prepared_files.take().ok_or_else(|| {
            OwnedOpError("internal: placement ran before payload preparation".to_string())
        })?;
        let runner = InstallRunner::new(self.layout);
        let outcome = runner
            .install_prepared(prepared_files)
            .map_err(|err| OwnedOpError(format!("placing files failed: {err}")))?;
        let prepared = self.prepared()?;
        match write_installed_component_manifest(
            self.layout,
            &self.component,
            &prepared.manifest_toml,
        ) {
            Ok(path) => {
                self.placed = outcome.files;
                self.manifest_path = Some(path);
                Ok(StepSuccess::clean())
            }
            Err(err) => {
                // The step did not complete: clean up its own partial work so
                // the executor never registers an undo for half-placed files.
                rollback_installed_files(&outcome.files);
                Err(OwnedOpError(err.to_string()))
            }
        }
    }

    fn set_capabilities(&mut self) -> Result<StepSuccess, OwnedOpError> {
        let prepared = self.prepared()?;
        let manager = (self.effects.capability)(self.ctx.install_mode.as_str(), &self.env);
        let outcome = apply_capabilities(
            manager.as_ref(),
            &prepared.capabilities,
            Some(&self.log),
            &self.component,
            &self.operation_id,
            "cli",
            self.ctx.install_mode.as_str(),
        );
        if let Some(reason) = outcome.aborted {
            return Err(OwnedOpError(format!(
                "required capability application failed: {reason}"
            )));
        }
        self.applied_capabilities = outcome.applied_requests;
        Ok(StepSuccess::with_warnings(outcome.warnings))
    }

    fn enable_services(&mut self) -> Result<StepSuccess, OwnedOpError> {
        let prepared = self.prepared()?;
        let services = prepared.services.clone();
        let post_enable_specs = self.hooks()?.post_enable.clone();
        let manager = self.service_manager(&services);
        let mode = self.ctx.install_mode.as_str();
        // Activation is best-effort by convention: a failed enable/start is
        // a warning, not an abort — the files are installed and an operator
        // can fix a unit out of band.
        let run = apply_services(
            manager.as_ref(),
            &services,
            ServiceActivation::Start,
            Some(&self.log),
            &self.component,
            &self.operation_id,
            "cli",
            mode,
        );
        let mut warnings = run.warnings.clone();
        // The contract's post_enable hooks belong to this step: services are
        // up, the record is not yet written. A strict failure deactivates
        // the units this step just activated (its own partial work), then
        // fails the step so the executor unwinds the earlier steps.
        let post_enable = run_hooks(
            &post_enable_specs,
            self.layout,
            Some(&self.log),
            &self.operation_id,
            "cli",
            mode,
        );
        warnings.extend(post_enable.warnings);
        if let Some(failure) = post_enable.hard_failure {
            let cleanup = rollback_activated_services(
                manager.as_ref(),
                &run,
                Some(&self.log),
                &self.component,
                &self.operation_id,
                mode,
            );
            return Err(OwnedOpError(format!(
                "post_enable hook failed; stopped/disabled activated services{}: {}",
                service_cleanup_suffix(&cleanup),
                failure.summary()
            )));
        }
        self.service_run = Some(run);
        Ok(StepSuccess::with_warnings(warnings))
    }

    fn restart_services(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("service restart")
    }

    fn stop_services(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("service stop")
    }

    fn remove_owned_files(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("owned-file removal")
    }

    fn write_record(&mut self, write: RecordWrite) -> Result<StepSuccess, OwnedOpError> {
        if write != RecordWrite::Owned {
            return Err(OwnedOpError(format!(
                "fresh install cannot write a {} record",
                write.label()
            )));
        }
        let hooks = self.hooks()?;
        if !hooks.post_install.is_empty() || !hooks.post_enable.is_empty() {
            // Installation hooks may rewrite payload bytes. Capture their final
            // content while retaining the declared modes and symlink referents.
            for file in &mut self.placed {
                if file.referent.is_none() {
                    file.sha256 =
                        installed_file_digest(self.layout, &file.path).map_err(|err| {
                            OwnedOpError(format!(
                                "failed to record post-hook digest for {}: {err}",
                                file.path.display()
                            ))
                        })?;
                }
            }
        }
        let prepared = self.prepared()?;
        let manifest_path = self.manifest_path.clone().ok_or_else(|| {
            OwnedOpError("internal: record commit ran before files were placed".to_string())
        })?;
        let enabled_units: Vec<String> = self
            .service_run
            .as_ref()
            .map(|run| run.enabled_units.clone())
            .unwrap_or_default();
        let artifact = OwnedArtifact {
            version: prepared.resolution.entry.version.clone(),
            distribution_source: Some(prepared.resolution.artifact_url.clone()),
            raw_package: Some(prepared.resolution.package.clone()),
            // Digest verification of the embedded manifest is future work;
            // recording an unverified digest would overstate what ran.
            manifest_digest: None,
            files: owned_file_rows(
                &self.placed,
                &manifest_path,
                &prepared.manifest_toml,
                &self.applied_capabilities,
            ),
            services: prepared
                .services
                .iter()
                .map(|svc| ServiceRef {
                    name: svc.unit.clone(),
                    manager: svc.scope.manager_label().to_string(),
                    restartable: true,
                    // Reflect what the executor actually enabled this run.
                    enabled: enabled_units.contains(&svc.unit),
                    scope: svc.scope,
                })
                .collect(),
            external_modified_files: Vec::new(),
            provisioned_packages: self.provisioned_packages.clone(),
        };
        // I1 only fires on an absent record, so this is a fresh insert;
        // upsert also consumes a same-name quarantined record, which the
        // planner has already refused (I10) before this step could run.
        self.store.upsert(Installation {
            kind: ObjectKind::Component,
            name: self.component.clone(),
            scope: self.scope,
            binding: ProviderBinding::Owned { artifact },
            status: LifecycleStatus::Installed,
            installed_at: self.now.clone(),
            last_operation_id: Some(self.operation_id.clone()),
            subscription_scope: Default::default(),
            enabled_features: Vec::new(),
            health: Vec::new(),
        });
        self.store
            .save(self.state_path)
            .map_err(|err| OwnedOpError(format!("failed to save state: {err}")))?;
        Ok(StepSuccess::clean())
    }

    fn drop_record(&mut self) -> Result<StepSuccess, OwnedOpError> {
        Self::not_wired("record removal")
    }

    fn undo_place_files(&mut self) -> Vec<String> {
        rollback_installed_files(&self.placed);
        self.placed.clear();
        if let Some(path) = self.manifest_path.take() {
            rollback_installed_manifest(&path);
        }
        Vec::new()
    }

    fn undo_enable_services(&mut self) -> Vec<String> {
        let Some(run) = self.service_run.take() else {
            return Vec::new();
        };
        let services = self
            .prepared
            .as_ref()
            .map(|p| p.services.clone())
            .unwrap_or_default();
        let manager = self.service_manager(&services);
        rollback_activated_services(
            manager.as_ref(),
            &run,
            Some(&self.log),
            &self.component,
            &self.operation_id,
            self.ctx.install_mode.as_str(),
        )
    }

    fn restore_backup(&mut self) -> Vec<String> {
        // I1 plans never back up files, so this compensation is never
        // registered against this port.
        Vec::new()
    }
}

/// Version label for an installed record in a conflict report: the owned
/// artifact's version, or a delegated record's last observation.
pub(crate) fn installed_version_label(installation: &Installation) -> String {
    match &installation.binding {
        ProviderBinding::Owned { artifact } => artifact.version.clone(),
        ProviderBinding::Delegated { last_observed, .. } => last_observed
            .as_ref()
            .map(|o| o.version.clone())
            .unwrap_or_else(|| "unknown".to_string()),
    }
}

/// Owned-file rows for a record commit: what this run placed, plus the
/// manifest snapshot it wrote.
fn owned_file_rows(
    placed: &[InstalledFile],
    manifest_path: &Path,
    manifest_toml: &str,
    applied_capabilities: &[CapabilityRequest],
) -> Vec<OwnedFile> {
    let mut files: Vec<OwnedFile> = placed
        .iter()
        .map(|f| OwnedFile {
            path: f.path.clone(),
            owner: FileOwner::Anolisa,
            sha256: if f.referent.is_some() {
                None
            } else {
                Some(f.sha256.clone())
            },
            kind: if f.referent.is_some() {
                OwnedFileKind::Symlink
            } else if f.kind == FileKind::Config {
                OwnedFileKind::Config
            } else {
                OwnedFileKind::File
            },
            referent: f.referent.clone(),
            mode: expected_mode(f),
            capabilities: capabilities_for_path(&f.path, applied_capabilities),
        })
        .collect();
    files.push(OwnedFile {
        path: manifest_path.to_path_buf(),
        owner: FileOwner::Anolisa,
        sha256: Some(sha256_hex(manifest_toml.as_bytes())),
        kind: OwnedFileKind::File,
        referent: None,
        mode: recorded_mode(manifest_path),
        capabilities: Vec::new(),
    });
    files
}

fn installed_file_digest(layout: &FsLayout, path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::{Error, Read};

    validate_owned_path(layout, path).map_err(Error::other)?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Refuse replacement links and avoid blocking on a replacement FIFO.
        options.custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(Error::other("installed path is not a regular file"));
    }
    let mut hasher = Sha256::new();
    // Bound the read by the observed size, as the integrity probe does.
    let bytes = std::io::copy(
        &mut file.take(metadata.len().saturating_add(1)),
        &mut hasher,
    )?;
    if bytes != metadata.len() {
        return Err(Error::other(
            "installed file changed size while recording its digest",
        ));
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn expected_mode(file: &InstalledFile) -> Option<String> {
    if file.referent.is_some() {
        return None;
    }
    let raw = file.mode.as_deref().unwrap_or("0755");
    let octal = raw.trim().strip_prefix("0o").unwrap_or(raw.trim());
    u32::from_str_radix(octal, 8)
        .ok()
        .filter(|mode| *mode <= 0o7777)
        .map(|mode| format!("{mode:04o}"))
}

#[cfg(unix)]
fn recorded_mode(path: &Path) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    fs::symlink_metadata(path)
        .ok()
        .map(|meta| format!("{:04o}", meta.permissions().mode() & 0o7777))
}

#[cfg(not(unix))]
fn recorded_mode(_path: &Path) -> Option<String> {
    None
}

fn capabilities_for_path(path: &Path, applied: &[CapabilityRequest]) -> Vec<String> {
    let mut capabilities = applied
        .iter()
        .filter(|request| request.path == path)
        .flat_map(|request| request.caps.iter().cloned())
        .collect::<Vec<_>>();
    capabilities.sort_by_key(|cap| cap.to_ascii_lowercase());
    capabilities.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    capabilities
}

/// Lowercase-hex sha256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(bytes);
    hash.iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollback_capabilities_restore_only_recorded_backed_up_grants() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let restored = tmp.path().join("restored");
        let never_granted = tmp.path().join("never-granted");
        let not_backed_up = tmp.path().join("not-backed-up");
        let owned = |path: PathBuf, capabilities: Vec<&str>| OwnedFile {
            path,
            owner: FileOwner::Anolisa,
            sha256: None,
            kind: OwnedFileKind::File,
            referent: None,
            mode: None,
            capabilities: capabilities.into_iter().map(str::to_string).collect(),
        };
        let prior_files = vec![
            owned(restored.clone(), vec!["CAP_BPF"]),
            owned(never_granted.clone(), Vec::new()),
            owned(not_backed_up, vec!["CAP_NET_ADMIN"]),
        ];
        let backups = vec![
            ReplayBackup {
                source: tmp.path().join("0.bak"),
                dest: restored.clone(),
                sha256: None,
                mode: Some(0o755),
            },
            ReplayBackup {
                source: tmp.path().join("1.bak"),
                dest: never_granted,
                sha256: None,
                mode: Some(0o755),
            },
        ];

        let requests = rollback_capability_requests(&prior_files, &backups);

        assert_eq!(
            requests,
            vec![CapabilityRequest {
                path: restored,
                caps: vec!["CAP_BPF".to_string()],
                optional: false,
            }]
        );
    }

    #[test]
    #[cfg(unix)]
    fn owned_file_rows_record_mode_and_applied_capabilities() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let binary = tmp.path().join("tool");
        let manifest = tmp.path().join("component.toml");
        fs::write(&binary, b"payload").expect("binary");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("chmod");
        fs::write(&manifest, b"[component]\nname = \"tool\"\n").expect("manifest");
        let placed = vec![InstalledFile {
            path: binary.clone(),
            kind: FileKind::Executable,
            mode: Some("0755".into()),
            sha256: "deadbeef".to_string(),
            referent: None,
        }];
        let capabilities = vec![CapabilityRequest {
            path: binary.clone(),
            caps: vec!["CAP_BPF".to_string()],
            optional: false,
        }];

        let rows = owned_file_rows(
            &placed,
            &manifest,
            "[component]\nname = \"tool\"\n",
            &capabilities,
        );
        let row = rows
            .iter()
            .find(|row| row.path == binary)
            .expect("binary row");

        assert_eq!(row.mode.as_deref(), Some("0755"));
        assert_eq!(row.capabilities, vec!["CAP_BPF".to_string()]);
    }

    #[test]
    #[cfg(unix)]
    fn owned_file_rows_record_directory_source_modes_from_placed_files() {
        use anolisa_core::{IntegrityStatus, check_owned_file};
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let layout =
            FsLayout::user_with_overrides(tmp.path().to_path_buf(), None, None, None, None, None);
        let adapter_root = layout.datadir.join("adapters/tokenless/codex");
        let script = adapter_root.join("scripts/tool-ready");
        let hooks = adapter_root.join("hooks/hooks.json");
        let manifest = layout.datadir.join("components/tokenless/component.toml");
        fs::create_dir_all(script.parent().expect("script parent")).expect("script parent");
        fs::create_dir_all(hooks.parent().expect("hooks parent")).expect("hooks parent");
        fs::create_dir_all(manifest.parent().expect("manifest parent")).expect("manifest parent");
        fs::write(&script, b"#!/usr/bin/env python3\n").expect("script");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod script");
        fs::write(&hooks, br#"{"hooks":{}}"#).expect("hooks");
        fs::set_permissions(&hooks, fs::Permissions::from_mode(0o644)).expect("chmod hooks");
        fs::write(&manifest, b"[component]\nname = \"tokenless\"\n").expect("manifest");

        let placed = vec![
            InstalledFile {
                path: script.clone(),
                kind: FileKind::Data,
                mode: Some("0755".into()),
                sha256: sha256_hex(b"#!/usr/bin/env python3\n"),
                referent: None,
            },
            InstalledFile {
                path: hooks.clone(),
                kind: FileKind::Data,
                mode: Some("0644".into()),
                sha256: sha256_hex(br#"{"hooks":{}}"#),
                referent: None,
            },
        ];

        let rows = owned_file_rows(
            &placed,
            &manifest,
            "[component]\nname = \"tokenless\"\n",
            &[],
        );

        let script_row = rows
            .iter()
            .find(|row| row.path == script)
            .expect("script row");
        let hooks_row = rows
            .iter()
            .find(|row| row.path == hooks)
            .expect("hooks row");
        assert_eq!(script_row.mode.as_deref(), Some("0755"));
        assert_eq!(hooks_row.mode.as_deref(), Some("0644"));
        assert_eq!(check_owned_file(&layout, script_row), IntegrityStatus::Ok);
        assert_eq!(check_owned_file(&layout, hooks_row), IntegrityStatus::Ok);
    }

    /// Teardown pruning must remove the directory chain the file removal
    /// emptied (so no skeleton is left to shadow adapter source probing in
    /// another scope), while keeping the ANOLISA-owned roots and any
    /// directory that still has contents.
    #[test]
    fn prune_emptied_dirs_removes_skeleton_but_keeps_roots_and_nonempty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let datadir = layout.datadir.clone();

        // Emptied chain: datadir/extensions/tokenless/hooks (files removed).
        let emptied = datadir.join("extensions").join("tokenless").join("hooks");
        fs::create_dir_all(&emptied).expect("emptied chain");
        // Sibling that must survive because it still holds a file.
        let kept = datadir.join("extensions").join("other");
        fs::create_dir_all(&kept).expect("kept dir");
        fs::write(kept.join("keep.txt"), b"keep").expect("keep file");

        prune_emptied_dirs(&layout, vec![emptied.clone()]);

        assert!(!emptied.exists(), "emptied chain must be pruned");
        assert!(
            !datadir.join("extensions").join("tokenless").exists(),
            "emptied parent must be pruned"
        );
        assert!(
            datadir.join("extensions").exists(),
            "shared ancestor with surviving content must stay"
        );
        assert!(
            kept.join("keep.txt").is_file(),
            "unrelated content must stay"
        );
        assert!(datadir.exists(), "the owned datadir root itself must stay");
    }

    /// The owned root itself is never pruned, even when the teardown
    /// emptied it completely: it is the per-path climb boundary.
    #[test]
    fn prune_emptied_dirs_never_removes_owned_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let datadir = layout.datadir.clone();
        let only = datadir.join("components").join("tokenless");
        fs::create_dir_all(&only).expect("chain");

        prune_emptied_dirs(&layout, vec![only]);

        assert!(
            datadir.exists(),
            "datadir root must survive even when fully emptied"
        );
        assert!(!datadir.join("components").exists(), "chain below pruned");
    }

    /// Owned roots can nest: the user layout places `libexec_dir` inside
    /// `lib_dir`. Pruning the last libexec component must stop at
    /// `libexec_dir` itself — a parent-is-owned check alone would climb
    /// through it because `lib_dir` is also owned.
    #[test]
    fn prune_emptied_dirs_keeps_nested_user_mode_roots() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).expect("home");
        let layout = FsLayout::user(home);
        assert!(
            layout.libexec_dir.starts_with(&layout.lib_dir),
            "precondition: user-mode libexec nests inside lib"
        );

        let emptied = layout.libexec_dir.join("tokenless");
        fs::create_dir_all(&emptied).expect("chain");

        prune_emptied_dirs(&layout, vec![emptied.clone()]);

        assert!(!emptied.exists(), "component dir must be pruned");
        assert!(
            layout.libexec_dir.exists(),
            "nested libexec root must survive the climb"
        );
        assert!(layout.lib_dir.exists(), "outer lib root must survive");
    }
}

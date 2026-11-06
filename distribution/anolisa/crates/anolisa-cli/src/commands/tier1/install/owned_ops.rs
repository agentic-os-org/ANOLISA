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
    CapabilityRequest, FileKind, HookSpec, ResolvedLifecycleHooks, ServiceActivation,
    ServiceRequest, ServiceRunOutcome, ServiceScope, apply_capabilities, apply_services,
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
use super::raw::{
    InstallHooks, prepare_raw_execution, resolve_install_hooks,
    strict_unrunnable_hook_phase_reason, unrunnable_hook_phase_warnings,
};
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
            if let Some(reason) = component_conflict(&manifest, self.store) {
                return Err(OwnedOpError(reason));
            }
            // The same hook-phase contract a fresh install enforces
            // (`validate_owned_install`): a strict declaration of a phase
            // no command runs is an authoring defect the update path must
            // refuse before replacing files or writing the new installation
            // record. The artifact is already cached, and the update plan
            // has already run its backup step. A non-strict declaration proceeds
            // with a warning naming the phase: a hook the current CLI will not
            // execute is a capability limitation to surface, not a reason to block
            // the whole upgrade. Replay (repair) skips the preflight and
            // this check with it: it re-places the recorded manifest and
            // must not turn a legacy dead-phase declaration into a
            // recovery blocker.
            if let Some(reason) = strict_unrunnable_hook_phase_reason(&manifest, &self.component) {
                return Err(OwnedOpError(reason));
            }
            let mut preflight_warnings =
                super::provision::run_runtime_preflight(&manifest, &self.env, "update")
                    .map_err(|err| OwnedOpError(err.reason()))?;
            preflight_warnings.extend(unrunnable_hook_phase_warnings(&manifest, &self.component));
            warnings = preflight_warnings;
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
    /// Copies of payload-shipped post-uninstall scripts staged before the
    /// owned files are removed (see [`Self::stage_post_uninstall_hooks`]).
    staged_post_uninstall: Vec<PathBuf>,
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
            staged_post_uninstall: Vec::new(),
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

    /// Stage the `post_uninstall` scripts this uninstall is about to
    /// delete, before the owned files are removed.
    ///
    /// The plan runs `RunHook(PostUninstall)` after `RemoveOwnedFiles`,
    /// and hook scripts ship in the component payload as owned layout
    /// files (the contract's canonical placement), so the removal step
    /// deletes them before the post phase resolves - without staging,
    /// every payload-shipped post_uninstall hook would be silently
    /// skipped as `Missing`. Staging is scoped to the deletion list
    /// (`prior.files`, the very list `remove_owned_files` iterates): a
    /// hook the removal will not delete keeps its original path, because
    /// the staging copy travels without the script's neighbors and would
    /// break a hook that loads a same-directory helper (round-5 review).
    fn stage_post_uninstall_hooks(&mut self) {
        self.staged_post_uninstall = stage_post_uninstall_hook_copies(
            self.layout,
            &self.component,
            &mut self.hooks.post_uninstall,
            &self.prior.files,
        );
    }

    /// Remove the staged post-uninstall copies (best effort) once the post
    /// phase has run. `remove_dir` only succeeds when empty, so leftovers
    /// from another uninstall are never destroyed.
    fn cleanup_staged_post_uninstall_hooks(&mut self) {
        let staged = std::mem::take(&mut self.staged_post_uninstall);
        remove_staged_hook_copies(&staged, &self.layout.state_dir);
    }
}

impl Drop for RawTeardownOps<'_> {
    /// The teardown can abort between staging the post-uninstall copies
    /// (`remove_owned_files`) and the post phase that consumes them - for
    /// instance when a later owned-file removal fails. The executor then
    /// returns without running `RunHook(PostUninstall)`, and if the hook's
    /// own payload file was already deleted, a retry finds no source and
    /// never re-stages, so an executable copy would sit in the state
    /// directory forever. Dropping purges whatever the success path
    /// ([`RawTeardownOps::cleanup_staged_post_uninstall_hooks`] empties
    /// the list first) did not consume.
    fn drop(&mut self) {
        remove_staged_hook_copies(&self.staged_post_uninstall, &self.layout.state_dir);
    }
}

/// Stage the `post_uninstall` scripts this uninstall is about to delete,
/// before the owned files are removed.
///
/// The plan runs `RunHook(PostUninstall)` after `RemoveOwnedFiles`, and hook
/// scripts ship in the component payload as owned layout files (the
/// contract's canonical placement), so the removal step deletes them before
/// the post phase resolves - without staging, every payload-shipped
/// post_uninstall hook would be silently skipped as `Missing`.
///
/// Staging is scoped to `removal_list` - the `prior.files` list the removal
/// loop iterates (round-5 review): the hook contract only requires the
/// expanded script path to pass the owned-roots check, not that the file be
/// owned by the uninstalling component, so a shared hook that the removal
/// will not delete survives in place. Rewriting such a hook to a staging
/// copy would be pure damage: the copy travels without the script's
/// neighbors, and a hook that loads a same-directory helper via
/// `dirname "$0"` breaks. A script not on the deletion list keeps its
/// original path and `run_hook` executes it exactly as before.
///
/// Each listed script is copied into an owned staging directory under
/// `state_dir` and the spec is pointed at the copy; the copy keeps the
/// source's executable mode, because `run_hook` executes it. A script that
/// is already missing or unreadable stays untouched so the run reports
/// `Missing` exactly as before; a staging failure falls back to the original
/// path rather than failing the uninstall. A source that fails the
/// owned-roots boundary check - outside the roots, or a symlink under them
/// pointing out - is likewise left untouched, so `run_hook` rejects the path
/// exactly as it would without staging. A source that is itself a symlink is
/// never read through (the copy opens it `O_NOFOLLOW`): the staged copy must
/// run under its own name, not smuggle another file's bytes.
///
/// The copy itself follows the install-time hardening doctrine (see
/// `anolisa_core::lifecycle::prepare_backup`): the destination leaf is
/// created `create_new` + `O_NOFOLLOW`, so a pre-planted symlink at the
/// staging path is never written through, and a copy that dies mid-stream
/// leaves no partial residue behind. Both properties are pinned by
/// regression tests in this module.
///
/// Returns every destination that was registered for cleanup, including
/// entries whose copy later failed: the caller installs the list before the
/// teardown can abort, so the success cleanup and the `Drop` guard purge a
/// partial copy even when staging bailed out on it.
///
/// A staging path behind a symlinked parent is refused entirely: the link
/// would redirect the copy and the cleanup into whatever it points at, and
/// a link between owned roots is invisible to the boundary checks above
/// (round-4 review), so nothing is written, deleted, or registered and the
/// spec keeps its original path.
fn stage_post_uninstall_hook_copies(
    layout: &FsLayout,
    component: &str,
    hooks: &mut [HookSpec],
    removal_list: &[OwnedFile],
) -> Vec<PathBuf> {
    let mut staged_copies = Vec::new();
    if hooks.is_empty() {
        return staged_copies;
    }
    let staging_root = layout.state_dir.join("uninstall-hooks").join(component);
    for (index, spec) in hooks.iter_mut().enumerate() {
        let source = spec.script.clone();
        // Scope before any stat or copy (round-5 review): staging exists
        // only so a hook the removal loop is about to delete survives to
        // the post phase. A hook outside `removal_list` is not deleted -
        // the contract requires only that its expanded path pass the
        // owned-roots check, not that the uninstalling component own it -
        // so rewriting it to a staging copy would break its
        // same-directory dependencies (`dirname "$0"`) for no benefit. It
        // keeps running from its original location, exactly as before
        // staging existed.
        if !removal_list.iter().any(|file| file.path == source) {
            continue;
        }
        // Boundary before any stat or copy: the spec came from the
        // persisted contract, so a source outside the owned roots -
        // or a symlink under them pointing out - must not be copied
        // into the state directory, trusted territory `run_hook`
        // executes from without further questions. Leaving the spec
        // untouched keeps the pre-staging behavior: `run_hook`
        // rejects the path itself instead of running a smuggled
        // script.
        if validate_owned_path(layout, &source).is_err() {
            continue;
        }
        if !source.is_file() {
            continue;
        }
        let Some(name) = source.file_name() else {
            continue;
        };
        let staged = staging_root.join(format!("{index}-{}", name.to_string_lossy()));
        if validate_owned_path(layout, &staged).is_err() {
            continue;
        }
        // A symlinked parent below the state root redirects every leaf
        // operation into its referent - `O_NOFOLLOW` protects only the
        // leaf - and a roots-to-roots link passes the boundary check
        // above, so refuse the whole entry: no copy, no cleanup
        // registration, and the spec keeps its original path.
        if parent_chain_contains_symlink_below(&layout.state_dir, &staged) {
            continue;
        }
        // Register the destination before any bytes move, so both the
        // success cleanup and the `Drop` guard cover a partial copy even
        // when the copy fails partway or the teardown aborts between
        // staging and the post phase. Entries whose copy failed point at
        // an already-removed path, which the best-effort cleanup ignores.
        staged_copies.push(staged.clone());
        if stage_hook_copy(&source, &staged, &layout.state_dir).is_err() {
            continue;
        }
        spec.script = staged;
    }
    staged_copies
}

/// Whether any ancestor of `path` strictly below `root` is a symlink.
///
/// Leaf operations on the staging namespace (`open` with `O_NOFOLLOW`,
/// `unlink`, `rmdir`) resolve every parent component, so a symlinked
/// parent redirects them into whatever it points at - and a link between
/// owned roots (`state_dir/uninstall-hooks/<component> -> bin_dir`, the
/// round-4 review's plant) passes `validate_owned_path`, because every
/// owned root is an equally valid containment target. The staging
/// namespace must be reached through real directories only: an entry
/// behind a symlinked parent is not ours to write or delete, and every
/// layer of the staging machinery refuses it (fail safe). The `root`
/// itself is not inspected: the layout's owned roots are the trust
/// anchor, and a deployment may legitimately place one behind a symlink.
/// Components that cannot be inspected are left for the following
/// open/remove to fail on their own.
fn parent_chain_contains_symlink_below(root: &Path, path: &Path) -> bool {
    let mut current = match path.parent() {
        Some(parent) => parent,
        None => return false,
    };
    while current.starts_with(root) && current != root {
        if fs::symlink_metadata(current)
            .map(|meta| meta.is_symlink())
            .unwrap_or(false)
        {
            return true;
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    false
}

/// Copy a hook script to its staging path with the install-time hardening
/// doctrine, so staging can never damage a file it does not own:
///
///   * The source is opened `O_NOFOLLOW` and its mode is read off the open
///     descriptor, then pinned on the copy with `fchmod`, so an executable
///     hook stays executable regardless of the umask (unlike
///     `prepare_backup`'s owner-only scratch copies, the staged file is
///     *executed*, not merely read back).
///   * The destination leaf is opened `create_new` + `O_NOFOLLOW`, so a
///     pre-planted symlink at the staging path fails the open instead of
///     being written through. Writing through such a link would clobber
///     whatever it points at - `validate_owned_path` accepts in-roots
///     links, and the cleanup would only delete the link, leaving the
///     clobbered referent behind.
///   * A stale non-directory entry at the staging path (residue of a
///     hard-killed earlier uninstall) is removed first - a symlink as a
///     link, never its referent - so a retry can stage again. A directory
///     occupying the path is refused outright.
///   * A staging parent that is itself a symlink refuses the copy
///     outright (`O_NOFOLLOW` protects only the leaf): the link would
///     redirect this copy and the later cleanup into whatever it points
///     at, and between owned roots the boundary check sees nothing wrong
///     (round-4 review).
///   * Any failure after the leaf was created removes the partial copy: a
///     truncated executable in the state directory is worse than no
///     staging at all.
fn stage_hook_copy(source: &Path, staged: &Path, state_root: &Path) -> std::io::Result<()> {
    use std::io::{Read, Write};

    if parent_chain_contains_symlink_below(state_root, staged) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a symlinked parent redirects the hook staging path",
        ));
    }

    let mut src_opts = fs::OpenOptions::new();
    src_opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        src_opts.custom_flags(nix::libc::O_NOFOLLOW);
    }
    let mut src = src_opts.open(source)?;
    let src_mode = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            Some(src.metadata()?.permissions().mode() & 0o7777)
        }
        #[cfg(not(unix))]
        None
    };

    if let Some(parent) = staged.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let mut dst_opts = fs::OpenOptions::new();
    dst_opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        dst_opts.custom_flags(nix::libc::O_NOFOLLOW);
    }
    let mut dst = match dst_opts.open(staged) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            // Retry once through a stale-entry clear: `create_new` refuses
            // *any* existing entry, and the only legitimate one is our own
            // residue from a hard-killed earlier run.
            clear_stale_staging_entry(staged, state_root)?;
            dst_opts.open(staged)?
        }
        Err(err) => return Err(err),
    };

    let outcome = (|| -> std::io::Result<()> {
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n])?;
        }
        dst.flush()?;
        if let Some(mode) = src_mode {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                dst.set_permissions(fs::Permissions::from_mode(mode))?;
            }
            let _ = mode;
        }
        Ok(())
    })();
    if outcome.is_err() {
        // The leaf may hold a partial copy (the stream can fail mid-write,
        // e.g. `EFBIG` under an `RLIMIT_FSIZE` ceiling). Neither the
        // success cleanup nor the `Drop` guard would remove it on the
        // failure path by itself, so purge it here.
        let _ = fs::remove_file(staged);
    }
    outcome
}

/// Remove a pre-existing non-directory entry at a staging path so the
/// `create_new` copy can proceed. A regular file is residue of a
/// hard-killed earlier staging run (the staging namespace holds only hook
/// copies, ours by construction); a symlink is removed as a *link* - its
/// referent is never touched. A directory is refused: it is not ours to
/// remove, and staging must fail rather than destroy it. A symlinked
/// parent is refused the same way (round-4 review): the delete would land
/// in whatever the link points at, not in the staging namespace.
fn clear_stale_staging_entry(staged: &Path, state_root: &Path) -> std::io::Result<()> {
    if parent_chain_contains_symlink_below(state_root, staged) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a symlinked parent redirects the hook staging path",
        ));
    }
    match fs::symlink_metadata(staged) {
        Ok(meta) if meta.is_dir() => Err(std::io::Error::new(
            std::io::ErrorKind::IsADirectory,
            "a directory occupies the hook staging path",
        )),
        Ok(_) => fs::remove_file(staged),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Delete a staged-copy list and its now-empty directories, best effort.
/// Entries behind a symlinked parent are skipped: the link would redirect
/// the delete into whatever it points at (round-4 review), so a staging
/// namespace that cannot be reached through real directories is not ours
/// to clean.
fn remove_staged_hook_copies(staged: &[PathBuf], state_root: &Path) {
    let reachable: Vec<&PathBuf> = staged
        .iter()
        .filter(|path| !parent_chain_contains_symlink_below(state_root, path))
        .collect();
    for path in &reachable {
        let _ = fs::remove_file(path);
    }
    if let Some(component_dir) = reachable.first().and_then(|path| path.parent()) {
        let _ = fs::remove_dir(component_dir);
        if let Some(root) = component_dir.parent() {
            let _ = fs::remove_dir(root);
        }
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
        if matches!(kind, HookKind::PostUninstall) {
            self.cleanup_staged_post_uninstall_hooks();
        }
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
        // The post-uninstall phase runs after this step and its scripts
        // ship in the payload as owned files, so they are about to be
        // deleted by the loop below - stage surviving copies first.
        self.stage_post_uninstall_hooks();
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
    let mut prepared = prepare_raw_execution(ctx, layout, resolution)?;
    let manifest = anolisa_core::ComponentManifest::from_toml_str(&prepared.manifest_toml)
        .map_err(|err| CliError::Runtime {
            command: command.to_string(),
            reason: format!("failed to parse component manifest: {err}"),
        })?;
    validate_component_conflict(&manifest, store, &HashSet::new(), command)?;
    let hooks = resolve_install_hooks(&manifest, layout, &component)?;
    // Non-strict declarations of phases no command runs ride the
    // resolution's warning channel so the install output names exactly
    // which declared hook this CLI will not execute.
    prepared
        .resolution
        .warnings
        .extend(unrunnable_hook_phase_warnings(&manifest, &component));
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

    /// Environment variable carrying the child half's scratch directory for
    /// `staged_hook_copy_leaves_no_partial_residue_when_the_stream_dies`.
    const STAGING_RESIDUE_CHILD_DIR: &str = "ANOLISA_TEST_STAGING_RESIDUE_CHILD_DIR";

    /// Minimal owned-record entry for the staging tests: a payload file at
    /// `path` on this uninstall's deletion list.
    fn owned_layout_file(path: PathBuf) -> OwnedFile {
        OwnedFile {
            path,
            owner: FileOwner::Anolisa,
            sha256: None,
            kind: OwnedFileKind::File,
            referent: None,
            mode: None,
            capabilities: Vec::new(),
        }
    }

    /// Regression (review round 2, P2): a pre-planted symlink at the
    /// deterministic staging path must never be written through. The old
    /// `fs::copy` followed the link and clobbered whatever it pointed at -
    /// a referent inside the owned roots passes `validate_owned_path`, so
    /// the boundary check offered no protection - and the cleanup then
    /// deleted only the link, leaving the clobbered referent behind.
    #[test]
    #[cfg(unix)]
    fn staging_replaces_a_planted_symlink_without_touching_its_referent() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        use anolisa_core::HookPhase;

        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let component = "ws-ckpt";

        // A payload-shipped hook script inside an owned root.
        let hook_dir = layout.datadir.join("components").join(component);
        fs::create_dir_all(&hook_dir).expect("hook dir");
        let source = hook_dir.join("post-uninstall.sh");
        fs::write(&source, b"#!/bin/sh\n# payload hook\n").expect("hook script");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).expect("chmod hook");

        // The victim: another component's file under `bin_dir` - inside the
        // owned roots, so a link pointing at it passes the boundary check
        // exactly as in the review's diagnostic.
        fs::create_dir_all(&layout.bin_dir).expect("bin dir");
        let victim = layout.bin_dir.join("anolisa-report");
        fs::write(&victim, b"original victim bytes").expect("victim");
        let victim_bytes = fs::read(&victim).expect("victim readable");

        // The planted symlink exactly where staging will copy.
        let staged = layout
            .state_dir
            .join("uninstall-hooks")
            .join(component)
            .join("0-post-uninstall.sh");
        fs::create_dir_all(staged.parent().expect("staging parent")).expect("staging dir");
        symlink(&victim, &staged).expect("plant the staging symlink");

        let mut hooks = vec![HookSpec::new(
            component,
            HookPhase::PostUninstall,
            source.clone(),
        )];
        // The hook ships in the payload, so its script is on this
        // uninstall's deletion list.
        let removal_list = vec![owned_layout_file(source.clone())];
        let staged_copies =
            stage_post_uninstall_hook_copies(&layout, component, &mut hooks, &removal_list);

        // The referent is untouched: staging replaced the link itself.
        assert_eq!(
            fs::read(&victim).expect("victim readable"),
            victim_bytes,
            "staging must never write through a planted symlink"
        );
        // The hook still staged, under its own name, executable.
        assert_eq!(
            hooks[0].script, staged,
            "hook spec must point at the staged copy"
        );
        assert!(staged.is_file(), "the staged copy must be a regular file");
        assert_eq!(
            fs::read(&staged).expect("staged copy readable"),
            b"#!/bin/sh\n# payload hook\n",
        );
        assert_eq!(
            fs::symlink_metadata(&staged)
                .expect("staged metadata")
                .permissions()
                .mode()
                & 0o7777,
            0o755,
            "the staged copy must keep the hook's executable mode"
        );
        assert_eq!(staged_copies, vec![staged], "cleanup registration");
    }

    /// Regression (review round 5, P2): staging must be scoped to the
    /// hooks on this uninstall's deletion list. A shared hook the removal
    /// loop will not delete survives in place, so rewriting its spec to a
    /// staging copy would be pure damage - the copy travels without the
    /// script's neighbors, and a hook that loads a same-directory helper
    /// via `dirname "$0"` breaks. The spec must keep its original path,
    /// and nothing may be staged or registered for cleanup.
    #[test]
    #[cfg(unix)]
    fn staging_skips_a_hook_outside_the_removal_list() {
        use anolisa_core::HookPhase;

        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let component = "ws-ckpt";

        // A shared hook script inside an owned root, but not on this
        // uninstall's deletion list.
        let hook_dir = layout.datadir.join("hooks").join("shared");
        fs::create_dir_all(&hook_dir).expect("hook dir");
        let source = hook_dir.join("post-uninstall.sh");
        fs::write(&source, b"#!/bin/sh\n# shared hook\n").expect("hook script");

        let mut hooks = vec![HookSpec::new(
            component,
            HookPhase::PostUninstall,
            source.clone(),
        )];
        // The deletion list names only the binary; the shared hook is not
        // on it.
        let removal_list = vec![owned_layout_file(layout.bin_dir.join("ws-ckpt"))];
        let staged_copies =
            stage_post_uninstall_hook_copies(&layout, component, &mut hooks, &removal_list);

        assert_eq!(
            hooks[0].script, source,
            "a hook outside the removal list must keep its original path"
        );
        assert!(
            staged_copies.is_empty(),
            "a hook outside the removal list must not be registered for cleanup"
        );
        assert!(
            !layout.state_dir.join("uninstall-hooks").exists(),
            "a hook outside the removal list must not be staged at all"
        );
    }

    /// Regression (review round 2, P2): a copy that dies mid-stream must
    /// leave no partial residue. The old `fs::copy` created the destination
    /// and could write part of it before failing, and the destination only
    /// joined the cleanup list after a *successful* copy - so neither the
    /// success cleanup nor the `Drop` guard removed the truncated file. The
    /// review reproduced it with a 128 KiB `0755` hook, `RLIMIT_FSIZE` at
    /// 32 KiB and `SIGXFSZ` ignored: the non-strict uninstall succeeded,
    /// the original hook was deleted, and a 32 KiB `0755` copy stayed in
    /// the staging directory.
    ///
    /// The fault is injected in a re-executed child process, because the
    /// rlimit and the signal disposition belong to the whole process and
    /// must not leak into the other tests in this binary. The child caps
    /// only the *soft* ceiling and restores it before exiting: the
    /// coverage runtime writes its `.profraw` at exit, and a 32 KiB
    /// ceiling left in place truncates it into a profile the CI's merge
    /// step rejects (round-4 review) - while a lowered *hard* ceiling
    /// could not be raised again by an unprivileged process at all.
    #[test]
    #[cfg(unix)]
    fn staged_hook_copy_leaves_no_partial_residue_when_the_stream_dies() {
        // Child half: cap the writable file size, ignore SIGXFSZ, then
        // stage a hook that cannot fit. The staging must fail AND leave
        // nothing behind at the staged path.
        if let Some(dir) = std::env::var_os(STAGING_RESIDUE_CHILD_DIR) {
            let dir = PathBuf::from(dir);
            // Cap only the *soft* ceiling and restore it before exiting:
            // the coverage runtime writes this process's `.profraw` at
            // exit, and a 32 KiB `RLIMIT_FSIZE` left in place truncates
            // it into a profile the CI's merge step rejects (the round-4
            // review diagnosed exactly that). Lowering the *hard*
            // ceiling instead would be irreversible for an unprivileged
            // process.
            let mut inherited = nix::libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            unsafe {
                assert_eq!(
                    nix::libc::getrlimit(nix::libc::RLIMIT_FSIZE, &mut inherited),
                    0,
                    "child: getrlimit(RLIMIT_FSIZE)"
                );
                let ceiling = nix::libc::rlimit {
                    rlim_cur: 32 * 1024,
                    rlim_max: inherited.rlim_max,
                };
                assert_eq!(
                    nix::libc::setrlimit(nix::libc::RLIMIT_FSIZE, &ceiling),
                    0,
                    "child: setrlimit(RLIMIT_FSIZE)"
                );
                nix::libc::signal(nix::libc::SIGXFSZ, nix::libc::SIG_IGN);
            }
            let source = dir.join("hook.sh");
            let staged = dir
                .join("uninstall-hooks")
                .join("ws-ckpt")
                .join("0-hook.sh");
            let outcome = stage_hook_copy(&source, &staged, &dir);
            let residue = fs::symlink_metadata(&staged);
            unsafe {
                assert_eq!(
                    nix::libc::setrlimit(nix::libc::RLIMIT_FSIZE, &inherited),
                    0,
                    "child: restore the soft ceiling for the coverage runtime"
                );
            }
            let code = match (outcome.is_err(), residue) {
                (true, Err(err)) if err.kind() == std::io::ErrorKind::NotFound => 0,
                (true, Ok(meta)) => {
                    eprintln!(
                        "[staging child] copy failed but left a residue: {:?}, {} bytes",
                        meta.file_type(),
                        meta.len()
                    );
                    3
                }
                (true, Err(err)) => {
                    eprintln!("[staging child] stat error: {err}");
                    5
                }
                (false, _) => {
                    eprintln!("[staging child] staging unexpectedly succeeded under the cap");
                    4
                }
            };
            std::process::exit(code);
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("hook.sh");
        let payload = vec![b'#'; 128 * 1024];
        fs::write(&source, &payload).expect("hook script");
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).expect("chmod hook");
        }

        let spawned =
            std::process::Command::new(std::env::current_exe().expect("locate this test binary"))
                // A plain substring filter runs only this test in the child (its
                // name is unique in the binary) without the module-qualified path
                // that `--exact` would require.
                .arg("staged_hook_copy_leaves_no_partial_residue_when_the_stream_dies")
                .arg("--nocapture")
                .env(STAGING_RESIDUE_CHILD_DIR, dir.path().as_os_str())
                .spawn()
                .expect("spawn the staging child");
        // Never leak the child, even when an assertion below fails.
        struct ChildGuard(std::process::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = ChildGuard(spawned);

        let status = child.0.wait().expect("wait for the staging child");
        assert!(
            status.success(),
            "the capped staging child must exit cleanly, got {status}              (3 = partial copy left behind, 4 = staging unexpectedly              succeeded, 5 = stat error)"
        );
        // The failed staging must not damage the source hook either.
        assert_eq!(
            fs::read(&source).expect("source readable"),
            payload,
            "the failed staging must leave the source hook untouched"
        );
    }

    /// Regression (review round 4, P2): a symlinked *parent* of the staging
    /// path must refuse the whole staging entry. `O_NOFOLLOW` guards only
    /// the leaf, so a `state_dir/uninstall-hooks/<component>` symlink aimed
    /// at `bin_dir` - another owned root, hence invisible to
    /// `validate_owned_path` - let the stale-entry clear delete
    /// `bin_dir/0-post-uninstall.sh`, the copy replace it, and the cleanup
    /// delete the copy again: an out-of-manifest file destroyed with no
    /// race and no escape out of the owned roots.
    #[test]
    #[cfg(unix)]
    fn staging_refuses_a_symlinked_parent_directory() {
        use std::os::unix::fs::symlink;

        use anolisa_core::HookPhase;

        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let component = "ws-ckpt";

        // A payload-shipped hook script inside an owned root.
        let hook_dir = layout.datadir.join("components").join(component);
        fs::create_dir_all(&hook_dir).expect("hook dir");
        let source = hook_dir.join("post-uninstall.sh");
        fs::write(
            &source,
            b"#!/bin/sh
# payload hook
",
        )
        .expect("hook script");

        // The victim: another component's file under `bin_dir`, sitting
        // exactly where the staging leaf resolves to once the component
        // staging directory is a symlink there - inside the owned roots,
        // so the round-4 diagnostic passes every boundary check.
        fs::create_dir_all(&layout.bin_dir).expect("bin dir");
        let victim = layout.bin_dir.join("0-post-uninstall.sh");
        fs::write(&victim, b"original victim bytes").expect("victim");

        // The planted parent: the component staging directory itself
        // points at `bin_dir`.
        let staging_parent = layout.state_dir.join("uninstall-hooks").join(component);
        fs::create_dir_all(staging_parent.parent().expect("uninstall-hooks"))
            .expect("uninstall-hooks dir");
        symlink(&layout.bin_dir, &staging_parent).expect("plant the parent symlink");

        let mut hooks = vec![HookSpec::new(
            component,
            HookPhase::PostUninstall,
            source.clone(),
        )];
        // The hook ships in the payload, so its script is on this
        // uninstall's deletion list.
        let removal_list = vec![owned_layout_file(source.clone())];
        let staged_copies =
            stage_post_uninstall_hook_copies(&layout, component, &mut hooks, &removal_list);

        // Nothing was deleted or written through the link: the victim
        // keeps its bytes.
        assert_eq!(
            fs::read(&victim).expect("victim readable"),
            b"original victim bytes",
            "staging behind a symlinked parent must not touch the referent"
        );
        // Fail safe: the spec keeps its original path instead of pointing
        // through the link at another component's territory...
        assert_eq!(
            hooks[0].script, source,
            "the hook spec must keep its original path when staging is refused"
        );
        // ... and nothing was registered for cleanup.
        assert!(
            staged_copies.is_empty(),
            "no staging entry may be registered behind a symlinked parent"
        );
    }

    /// The cleanup side of the same guarantee: an entry registered behind a
    /// symlinked parent - planted after staging, or left by any older path -
    /// must be skipped, never deleted through the link.
    #[test]
    #[cfg(unix)]
    fn staged_hook_cleanup_skips_entries_behind_a_symlinked_parent() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let component = "ws-ckpt";

        fs::create_dir_all(&layout.bin_dir).expect("bin dir");
        let victim = layout.bin_dir.join("0-post-uninstall.sh");
        fs::write(&victim, b"original victim bytes").expect("victim");

        let staging_parent = layout.state_dir.join("uninstall-hooks").join(component);
        fs::create_dir_all(staging_parent.parent().expect("uninstall-hooks"))
            .expect("uninstall-hooks dir");
        symlink(&layout.bin_dir, &staging_parent).expect("plant the parent symlink");

        // An entry exactly as a previous run would have registered: the
        // staging leaf behind the symlinked parent.
        let staged = staging_parent.join("0-post-uninstall.sh");
        remove_staged_hook_copies(&[staged], &layout.state_dir);

        assert_eq!(
            fs::read(&victim).expect("victim readable"),
            b"original victim bytes",
            "cleanup must not delete through a symlinked parent"
        );
    }
}

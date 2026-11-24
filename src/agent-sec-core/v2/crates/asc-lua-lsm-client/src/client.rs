//! Classifies management results and owns local object/process initialization.

mod confirmation;
mod file_targets;
mod preflight;
mod process_tree;
use confirmation::{Confirmation, valid_instance};
use preflight::MappingPreflight;
use process_tree::{boot_id, process_family, process_tasks, verify_process};

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use asc_policy_target_contracts::TargetDeploymentClient;
use asc_policy_types::target::{
    DeploymentReport, Failure, FailureKind, Observation, PreparedApply, Presence,
    TargetBindingPlan, TargetRef,
};
use sha2::{Digest, Sha256};

use crate::{NativeSecurityFs, SecurityFs};
use asc_lua_lsm_protocol::{
    DEFAULT_STATE_DIRECTORY, LUA_LSM_PLAN_FORMAT, LUA_LSM_ROUTE, LuaLsmPlan, is_supported_path,
    is_supported_pattern, module_name, render_module, render_module_with_views,
};

const MAX_BYTES: usize = 1024 * 1024;
const MAX_OBJECTS: usize = 65_536;
// The kernel ignores unknown return-table keys; every generated hook is required.
const BASE_HOOKS: u64 = 15;
// The previous write template exported create/mkdir and implements the same handover commands.
const PREVIOUS_WRITE_HOOKS: u64 = 17;
const PREVIOUS_FULL_HOOKS: u64 = 19;
const HOOKS_WITH_CREATION: u64 = BASE_HOOKS + 3;

/// Synchronous Client; dropping it never unregisters kernel modules.
pub struct LuaLsmClient {
    kernel: Arc<dyn SecurityFs>,
    directory: PathBuf,
}

impl Default for LuaLsmClient {
    fn default() -> Self {
        Self::new(
            Arc::new(NativeSecurityFs::default()),
            DEFAULT_STATE_DIRECTORY,
        )
    }
}

impl LuaLsmClient {
    /// Injects a securityfs transport and a private absolute state directory.
    pub fn new(kernel: Arc<dyn SecurityFs>, directory: impl Into<PathBuf>) -> Self {
        Self {
            kernel,
            directory: directory.into(),
        }
    }

    fn decode(&self, format: &str, content: &[u8]) -> Result<LuaLsmPlan, Failure> {
        if format != LUA_LSM_PLAN_FORMAT
            || content.len() > MAX_BYTES
            || !self.directory.is_absolute()
        {
            return Err(rejected("LUA_LSM_INVALID_PLAN"));
        }
        let plan: LuaLsmPlan =
            serde_json::from_slice(content).map_err(|_| rejected("LUA_LSM_INVALID_PLAN"))?;
        if plan.process.pid == 0
            || plan.process.start_time == 0
            || plan.process.boot_id.is_empty()
            || plan.process.pid_namespace.is_empty()
            || plan.policy.version != 1
            || plan.policy.targets.is_empty()
            || plan.policy.targets.len() > 64
            || plan.policy.targets.iter().any(|target| {
                !is_supported_path(&target.path)
                    || target.exclusions.len() > 64
                    || target
                        .exclusions
                        .iter()
                        .any(|excluded| !is_supported_path(&excluded.path))
            })
            || plan.patterns.iter().any(|(index, patterns)| {
                *index >= plan.policy.targets.len()
                    || patterns.is_empty()
                    || patterns.len() > 64
                    || !patterns.iter().all(is_supported_pattern)
            })
            || plan.source != render_module(&plan, &self.directory)
        {
            return Err(rejected("LUA_LSM_INVALID_PLAN"));
        }
        Ok(plan)
    }

    fn target(plan: &LuaLsmPlan) -> TargetRef {
        let name = module_name(&plan.binding_id, plan.binding_revision);
        TargetRef {
            route: LUA_LSM_ROUTE.into(),
            cleanup: name.as_bytes().to_vec(),
            id: name,
        }
    }

    fn validate_target(target: &TargetRef) -> Result<(), Failure> {
        let identity = target
            .id
            .strip_prefix("asc_")
            .and_then(|value| value.rsplit_once("_r"))
            .and_then(|(id, revision)| {
                Some((
                    asc_policy_types::identifiers::ResourceId::new(id).ok()?,
                    asc_policy_types::identifiers::Revision::new(revision.parse().ok()?).ok()?,
                ))
            });
        if target.route != LUA_LSM_ROUTE
            || target.cleanup != target.id.as_bytes()
            || !target.id.starts_with("asc_")
            || target.id.len() > 160
            || !target
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
            || identity.is_none_or(|(id, revision)| module_name(&id, revision) != target.id)
        {
            return Err(rejected("LUA_LSM_INVALID_TARGET"));
        }
        Ok(())
    }

    // Keep registration, initialization and membership handover ordered together.
    #[allow(clippy::too_many_lines)]
    fn apply(&self, prepared: &PreparedApply, previous: &[TargetRef]) -> DeploymentReport {
        let result = (|| {
            let plan = self.decode(&prepared.format, &prepared.content)?;
            if Self::target(&plan) != prepared.target {
                return Err(rejected("LUA_LSM_INVALID_PREPARED"));
            }
            Self::ensure_directory(&self.directory).map_err(local_error)?;
            let version = self
                .kernel
                .read("version")
                .map_err(|e| kernel_error(&e, "QUERY"))?;
            if std::str::from_utf8(&version)
                .ok()
                .and_then(|v| v.trim().parse::<u32>().ok())
                != Some(1)
            {
                return Err(rejected("LUA_LSM_UNSUPPORTED_VERSION"));
            }
            let names = self.module_names()?;
            let boot = boot_id().map_err(local_error)?;
            // Bind the receipt to the subject identity as well as the Lua source.
            let digest = format!("{:x}", Sha256::digest(&prepared.content));
            let registered = names.contains_key(&prepared.target.id);
            let required_hooks = if plan.protects_creation() {
                HOOKS_WITH_CREATION
            } else {
                BASE_HOOKS
            };
            if registered && names.get(&prepared.target.id) != Some(&required_hooks) {
                return Err(rejected("LUA_LSM_UNSUPPORTED_HOOK_SET"));
            }
            let controls = self.directory.join(&prepared.target.id);
            let confirmation = if registered {
                // A name alone does not expose LIVE state or identify the source.
                let record = self
                    .read_confirmation(&prepared.target)
                    .filter(|record| {
                        record.boot == boot
                            && record.digest == digest
                            && valid_instance(&record.instance)
                    })
                    .ok_or_else(|| retryable("LUA_LSM_EXISTING_MODULE_UNCONFIRMED"))?;
                Some(record)
            } else {
                None
            };
            if fs::read_link("/proc/self").map_err(local_error)?
                != Path::new(&std::process::id().to_string())
            {
                return Err(rejected("LUA_LSM_PROCFS_VIEW_MISMATCH"));
            }
            let views = file_targets::path_views(&plan)?;
            if confirmation
                .as_ref()
                .is_some_and(|record| record.initialized)
            {
                return Ok(());
            }
            let instance = confirmation.map_or_else(
                || uuid::Uuid::new_v4().simple().to_string(),
                |record| record.instance,
            );
            let family = if previous.is_empty() {
                verify_process(&plan.process)?;
                process_family(&plan.process)?
            } else {
                Vec::new()
            };
            let mapping_preflight = MappingPreflight::collect(&plan, None)?;
            for process in &family {
                mapping_preflight.check(process.pid)?;
            }
            Self::ensure_directory(&controls).map_err(local_error)?;
            let mut record = Confirmation {
                boot,
                digest,
                initialized: false,
                instance,
            };
            if !registered {
                let source = render_module_with_views(&plan, &self.directory, &views);
                if source.len() > MAX_BYTES {
                    return Err(rejected("LUA_LSM_MODULE_SIZE_LIMIT"));
                }
                // Persist the instance before registration so retries can resume initialization.
                self.save_confirmation(&prepared.target, &record)
                    .map_err(local_error)?;
                let count = match self.kernel.write("register", source.as_bytes()) {
                    Ok(count) => count,
                    Err(error) => {
                        if error.kind() == io::ErrorKind::AlreadyExists {
                            self.clear_confirmation(&prepared.target)
                                .map_err(local_error)?;
                        }
                        return Err(kernel_error(&error, "REGISTER"));
                    }
                };
                if count != source.len() {
                    return Err(retryable("LUA_LSM_REGISTER_SHORT_WRITE"));
                }
                if self.module_names()?.get(&prepared.target.id) != Some(&required_hooks) {
                    return Err(retryable("LUA_LSM_REGISTRATION_UNCONFIRMED"));
                }
            }
            self.label_existing_processes(&plan.process, &controls, &family)?;
            let matches = |index, pinned: &File| match self
                .request(&controls, &format!("match/{index}/{}", pinned.as_raw_fd()))
            {
                Ok(()) => Ok(true),
                Err(e) if e.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error()) => {
                    Ok(false)
                }
                Err(_) => Err(retryable("LUA_LSM_PATTERN_INSPECTION_FAILED")),
            };
            let mapping_preflight = MappingPreflight::collect(&plan, Some(&matches))?;
            self.transfer(
                previous,
                &controls,
                &plan,
                &record.boot,
                &record.instance,
                &mapping_preflight,
            )?;
            if previous.is_empty() {
                // Catch children born after the initial snapshot but before
                // their parent's membership was installed. Later forks inherit.
                self.label_current_process_tree(
                    &controls,
                    &plan.process,
                    &mapping_preflight,
                    family,
                )?;
                verify_process(&plan.process)?;
            }
            record.initialized = true;
            self.save_confirmation(&prepared.target, &record)
                .map_err(local_error)?;
            Ok(())
        })();
        report(&prepared.target, result.map(|()| Presence::Present))
    }

    fn transfer(
        &self,
        previous: &[TargetRef],
        controls: &Path,
        plan: &LuaLsmPlan,
        boot: &str,
        instance: &str,
        mapping_preflight: &MappingPreflight,
    ) -> Result<(), Failure> {
        if previous.is_empty() {
            return Ok(());
        }
        let names = self.module_names()?;
        let mut owners = Vec::new();
        for target in previous {
            if !target.id.starts_with(&format!("asc_{}_r", plan.binding_id)) {
                return Err(rejected("LUA_LSM_INVALID_PREVIOUS_BINDING"));
            }
            if !names.contains_key(&target.id) {
                continue;
            }
            let record = self
                .read_confirmation(target)
                .filter(|record| record.boot == boot)
                .ok_or_else(|| retryable("LUA_LSM_PREVIOUS_MODULE_UNCONFIRMED"))?;
            // Both template generations implement the same membership handover.
            if !matches!(
                names.get(&target.id),
                Some(
                    &BASE_HOOKS
                        | &PREVIOUS_WRITE_HOOKS
                        | &PREVIOUS_FULL_HOOKS
                        | &HOOKS_WITH_CREATION
                )
            ) || !valid_instance(&record.instance)
            {
                return Err(retryable("LUA_LSM_PREVIOUS_MODULE_UNCONFIRMED"));
            }
            let owner = self.directory.join(&target.id);
            self.command(&owner, &format!("begin/{instance}"))?;
            owners.push(owner);
        }
        if owners.is_empty() {
            verify_process(&plan.process)?;
            return self.label_current_process_tree(
                controls,
                &plan.process,
                mapping_preflight,
                Vec::new(),
            );
        }
        for process in process_tasks()? {
            for owner in &owners {
                match self.request(owner, &format!("member/{}", process.pid)) {
                    Err(error)
                        if error.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error()) =>
                    {
                        continue;
                    }
                    Err(_) => return Err(retryable("LUA_LSM_MEMBERSHIP_UNCONFIRMED")),
                    Ok(()) => {}
                }
                verify_process(&process).map_err(|_| retryable("LUA_LSM_MEMBERSHIP_CHANGED"))?;
                mapping_preflight.check(process.pid)?;
                self.label_process(controls, &plan.process, &process)?;
                verify_process(&process).map_err(|_| retryable("LUA_LSM_MEMBERSHIP_CHANGED"))?;
                self.command(owner, &format!("sent/{}", process.pid))?;
                verify_process(&process).map_err(|_| retryable("LUA_LSM_MEMBERSHIP_CHANGED"))?;
            }
        }
        for owner in owners {
            self.command(&owner, &format!("complete/{instance}"))
                .map_err(|_| retryable("LUA_LSM_MEMBERSHIP_TRANSFER_PENDING"))?;
        }
        Ok(())
    }

    fn module_names(&self) -> Result<BTreeMap<String, u64>, Failure> {
        modules(
            &self
                .kernel
                .read("modules")
                .map_err(|error| kernel_error(&error, "QUERY"))?,
        )
    }

    fn command(&self, controls: &Path, command: &str) -> Result<(), Failure> {
        self.request(controls, command)
            .map_err(|_| retryable("LUA_LSM_INITIALIZATION_UNCONFIRMED"))
    }

    fn request(&self, controls: &Path, command: &str) -> io::Result<()> {
        let path = controls.join(command);
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("invalid Lua-LSM command path"))?;
        Self::ensure_directory(parent)?;
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => drop(file),
            // Creation itself can call file_open and acknowledge the command.
            Err(error)
                if error.raw_os_error() == Some(rustix::io::Errno::ALREADY.raw_os_error()) =>
            {
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let metadata = fs::symlink_metadata(&path)?;
                if !metadata.is_file()
                    || metadata.uid() != rustix::process::geteuid().as_raw()
                    || metadata.mode() & 0o077 != 0
                {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "unsafe Lua-LSM command file",
                    ));
                }
            }
            Err(error) => return Err(error),
        }
        self.kernel.bootstrap(&path)
    }
}

impl TargetDeploymentClient for LuaLsmClient {
    fn prepare_apply(&self, plan: &TargetBindingPlan) -> Result<PreparedApply, Failure> {
        let decoded = self.decode(&plan.format, &plan.content)?;
        Ok(PreparedApply {
            target: Self::target(&decoded),
            format: plan.format.clone(),
            content: plan.content.clone(),
        })
    }
    fn create(&self, prepared: &PreparedApply) -> DeploymentReport {
        self.apply(prepared, &[])
    }
    fn update(&self, previous: &[TargetRef], prepared: &PreparedApply) -> DeploymentReport {
        if previous.iter().any(|target| {
            Self::validate_target(target).is_err() || target.same_identity(&prepared.target)
        }) {
            return report(&prepared.target, Err(rejected("LUA_LSM_INVALID_TARGET")));
        }
        let mut current = self.apply(prepared, previous);
        if current.error.is_some() || previous.is_empty() {
            return current;
        }
        let old = self.delete(previous);
        if let Some(error) = old.error {
            current.error = Some(error);
        } else {
            let presence = self.module_names().and_then(|names| {
                names
                    .contains_key(&prepared.target.id)
                    .then_some(Presence::Present)
                    .ok_or_else(|| retryable("LUA_LSM_REGISTRATION_UNCONFIRMED"))
            });
            current = report(&prepared.target, presence);
        }
        current.observations.extend(old.observations);
        current
    }
    fn delete(&self, targets: &[TargetRef]) -> DeploymentReport {
        let mut result = DeploymentReport {
            observations: vec![],
            error: None,
        };
        if let Err(error) = targets.iter().try_for_each(Self::validate_target) {
            result.error = Some(error);
            return result;
        }
        if !targets.is_empty()
            && let Err(error) = Self::ensure_directory(&self.directory)
        {
            result.error = Some(local_error(error));
            return result;
        }
        for target in targets {
            let operation = (|| {
                self.clear_confirmation(target).map_err(local_error)?;
                match self.kernel.write("unregister", target.id.as_bytes()) {
                    Ok(count) if count != target.id.len() => {
                        return Err(retryable("LUA_LSM_UNREGISTER_SHORT_WRITE"));
                    }
                    Ok(_) => {}
                    // ENOENT can mean either the module or the control node is missing.
                    Err(error)
                        if error.raw_os_error()
                            == Some(rustix::io::Errno::NOENT.raw_os_error()) => {}
                    Err(error) => return Err(kernel_error(&error, "UNREGISTER")),
                }
                if self.module_names()?.contains_key(&target.id) {
                    return Err(retryable("LUA_LSM_UNREGISTER_ABSENCE_UNCONFIRMED"));
                }
                Ok(Presence::Absent)
            })();
            let mut partial = report(target, operation);
            if partial.error.is_none()
                && let Err(error) = fs::remove_dir_all(self.directory.join(&target.id))
                && error.kind() != io::ErrorKind::NotFound
            {
                partial.error = Some(local_error(error));
            }
            result.observations.extend(partial.observations);
            if partial.error.is_some() {
                result.error = partial.error;
            }
        }
        result
    }
}

fn modules(bytes: &[u8]) -> Result<BTreeMap<String, u64>, Failure> {
    let text = std::str::from_utf8(bytes).map_err(|_| retryable("LUA_LSM_INVALID_MODULES"))?;
    let mut names = BTreeMap::new();
    let mut header = false;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let columns: Vec<_> = line.split_whitespace().collect();
        if columns.starts_with(&[
            "name", "license", "size", "nlsm", "nload", "shdict", "kvnode", "author",
        ]) {
            header = true;
            continue;
        }
        if !header {
            continue;
        }
        if columns
            .iter()
            .all(|column| column.bytes().all(|byte| byte == b'-' || byte == b'='))
        {
            continue;
        }
        if columns.len() < 8
            || columns[2..7].iter().any(|v| v.parse::<u64>().is_err())
            || names
                .insert(
                    columns[0].into(),
                    columns[3]
                        .parse()
                        .map_err(|_| retryable("LUA_LSM_INVALID_MODULES"))?,
                )
                .is_some()
        {
            return Err(retryable("LUA_LSM_INVALID_MODULES"));
        }
    }
    if !header {
        return Err(retryable("LUA_LSM_INVALID_MODULES"));
    }
    Ok(names)
}

fn report(target: &TargetRef, result: Result<Presence, Failure>) -> DeploymentReport {
    match result {
        Ok(presence) => DeploymentReport {
            observations: vec![Observation {
                target: target.clone(),
                presence,
            }],
            error: None,
        },
        Err(error) => DeploymentReport {
            observations: vec![Observation {
                target: target.clone(),
                presence: Presence::Unknown,
            }],
            error: Some(error),
        },
    }
}

fn kernel_error(error: &io::Error, operation: &str) -> Failure {
    let (kind, suffix) = match error.raw_os_error() {
        Some(1 | 13 | 30) => (FailureKind::Rejected, "PERMISSION_DENIED"),
        Some(22 | 8 | 9 | 33 | 95) => (FailureKind::Rejected, "INVALID_MODULE"),
        Some(17) => (FailureKind::Retryable, "NAME_CONFLICT"),
        Some(16) => (FailureKind::Retryable, "BUSY_UNCONFIRMED"),
        Some(12 | 28) => (FailureKind::Retryable, "RESOURCE_EXHAUSTED"),
        Some(2) => (FailureKind::Retryable, "INTERFACE_UNAVAILABLE"),
        _ => (FailureKind::Retryable, "UNCONFIRMED"),
    };
    Failure::new(kind, &format!("LUA_LSM_{operation}_{suffix}"))
}
fn local_error(error: io::Error) -> Failure {
    if error.kind() == io::ErrorKind::PermissionDenied {
        return rejected("LUA_LSM_LOCAL_PERMISSION_DENIED");
    }
    let failure = kernel_error(&error, "LOCAL");
    drop(error);
    failure
}
fn rejected(code: &str) -> Failure {
    Failure::new(FailureKind::Rejected, code)
}
fn retryable(code: &str) -> Failure {
    Failure::new(FailureKind::Retryable, code)
}

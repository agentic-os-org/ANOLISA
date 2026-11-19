//! Classifies management results and owns local object/process initialization.

mod preflight;
use preflight::MappingPreflight;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use asc_file_policy::FileProtectionTargetKind;
use asc_policy_target_contracts::TargetDeploymentClient;
use asc_policy_types::process_discovery::ProcessIdentity;
use asc_policy_types::target::{
    DeploymentReport, Failure, FailureKind, Observation, PreparedApply, Presence,
    TargetBindingPlan, TargetRef,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{NativeSecurityFs, SecurityFs};
use asc_lua_lsm_protocol::{
    DEFAULT_STATE_DIRECTORY, LUA_LSM_PLAN_FORMAT, LUA_LSM_ROUTE, LuaLsmPlan, is_supported_path,
    is_supported_pattern, module_name, render_module,
};

const MAX_BYTES: usize = 1024 * 1024;
const MAX_OBJECTS: usize = 65_536;
// The kernel ignores unknown return-table keys; every generated hook is required.
const BASE_HOOKS: u64 = 15;
const HOOKS_WITH_CREATION: u64 = BASE_HOOKS + 2;

/// Synchronous Client; dropping it never unregisters kernel modules.
pub struct LuaLsmClient {
    kernel: Arc<dyn SecurityFs>,
    directory: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirmation {
    boot: String,
    digest: String,
    active: bool,
    instance: String,
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

    // Keep evidence invalidation, mutation, membership handover and activation ordered together.
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
            let receipt = self.confirmation(&prepared.target);
            let mut instance = uuid::Uuid::new_v4().simple().to_string();
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
            if registered {
                // A name alone does not expose LIVE state or identify the source.
                let record = fs::read(&receipt)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Confirmation>(&bytes).ok());
                if let Some(record) = record.filter(|record| {
                    record.boot == boot
                        && record.digest == digest
                        && valid_instance(&record.instance)
                }) {
                    instance = record.instance;
                    let status = self.request(&controls, "status");
                    if status.is_ok() {
                        self.save_confirmation(
                            &receipt,
                            &Confirmation {
                                boot,
                                digest,
                                active: true,
                                instance,
                            },
                        )
                        .map_err(local_error)?;
                        return Ok(());
                    }
                    if record.active
                        || status.err().and_then(|error| error.raw_os_error())
                            != Some(rustix::io::Errno::AGAIN.raw_os_error())
                    {
                        return Err(retryable("LUA_LSM_EXISTING_MODULE_UNCONFIRMED"));
                    }
                } else {
                    return Err(retryable("LUA_LSM_EXISTING_MODULE_UNCONFIRMED"));
                }
            }
            if fs::read_link("/proc/self").map_err(local_error)?
                != Path::new(&std::process::id().to_string())
            {
                return Err(rejected("LUA_LSM_PROCFS_VIEW_MISMATCH"));
            }
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
            if !registered {
                // Invalidate old evidence before the first modifying syscall.
                self.clear_confirmation(&prepared.target)
                    .map_err(local_error)?;
                let count = self
                    .kernel
                    .write("register", plan.source.as_bytes())
                    .map_err(|e| kernel_error(&e, "REGISTER"))?;
                if count != plan.source.len() {
                    return Err(retryable("LUA_LSM_REGISTER_SHORT_WRITE"));
                }
                if self.module_names()?.get(&prepared.target.id) != Some(&required_hooks) {
                    return Err(retryable("LUA_LSM_REGISTRATION_UNCONFIRMED"));
                }
                self.save_confirmation(
                    &receipt,
                    &Confirmation {
                        boot: boot.clone(),
                        digest: digest.clone(),
                        active: false,
                        instance: instance.clone(),
                    },
                )
                .map_err(local_error)?;
            }
            let _pins = self.initialize(&plan, &controls, family)?;
            let matches = |index, pinned: &File| {
                let handle = format!("/proc/self/fd/{}", pinned.as_raw_fd());
                match self.request(
                    &controls,
                    &format!("match/{index}/_/at/{}", encode(handle.as_bytes())),
                ) {
                    Ok(()) => Ok(true),
                    Err(e) if e.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error()) => {
                        Ok(false)
                    }
                    Err(_) => Err(retryable("LUA_LSM_PATTERN_INSPECTION_FAILED")),
                }
            };
            let mapping_preflight = MappingPreflight::collect(&plan, Some(&matches))?;
            self.transfer(
                previous,
                &controls,
                &plan,
                &boot,
                &instance,
                &mapping_preflight,
            )?;
            if previous.is_empty() {
                // Catch children born after the initial snapshot but before
                // their parent's membership was installed. Later forks inherit.
                for process in process_family(&plan.process)? {
                    verify_process(&process)?;
                    mapping_preflight.check(process.pid)?;
                    self.command(&controls, &format!("seed/{}", process.pid))?;
                    verify_process(&process)?;
                }
                verify_process(&plan.process)?;
            }
            self.command(&controls, "activate")?;
            self.confirm_active(&controls)?;
            self.save_confirmation(
                &receipt,
                &Confirmation {
                    boot,
                    digest,
                    active: true,
                    instance,
                },
            )
            .map_err(local_error)?;
            Ok(())
        })();
        report(&prepared.target, result.map(|()| Presence::Present))
    }

    fn initialize(
        &self,
        plan: &LuaLsmPlan,
        controls: &Path,
        family: Vec<ProcessIdentity>,
    ) -> Result<Vec<File>, Failure> {
        for process in family {
            match verify_process(&process) {
                Ok(()) => {
                    self.command(controls, &format!("seed/{}", process.pid))?;
                    verify_process(&process)?;
                }
                Err(_) if process != plan.process => {}
                Err(error) => return Err(error),
            }
        }
        let mut count = 0;
        let mut pins = Vec::new();
        for (index, target) in plan.policy.targets.iter().enumerate() {
            let path = fs::canonicalize(&target.path).map_err(local_error)?;
            let pinned = OpenOptions::new()
                .read(true)
                .custom_flags(rustix::fs::OFlags::PATH.bits().cast_signed())
                .open(&path)
                .map_err(local_error)?;
            let metadata = pinned.metadata().map_err(local_error)?;
            if match target.kind {
                FileProtectionTargetKind::ExactFile => !metadata.is_file(),
                FileProtectionTargetKind::DirectoryTree => !metadata.is_dir(),
            } {
                return Err(rejected("LUA_LSM_TARGET_TYPE_MISMATCH"));
            }
            self.descriptor_command(controls, "anchor", index + 1, b"", &pinned)?;
            self.tag_tree(
                controls,
                index + 1,
                &path,
                &path,
                target.kind == FileProtectionTargetKind::DirectoryTree,
                &mut count,
            )?;
            if let Some(parent) = path.parent()
                && let Some(name) = path.file_name()
            {
                self.object_command(controls, "slot", index + 1, name.as_bytes(), parent)?;
            }
            if target.protections.delete
                || target.protections.read
                || target.protections.ordinary_write
            {
                let mut ancestor = Some(path.as_path());
                while let Some(path) = ancestor {
                    self.object_command(controls, "stable", index + 1, b"", path)?;
                    ancestor = path.parent();
                }
            }
            let current = fs::metadata(&target.path).map_err(local_error)?;
            if current.dev() != metadata.dev() || current.ino() != metadata.ino() {
                return Err(retryable("LUA_LSM_TARGET_CHANGED"));
            }
            pins.push(pinned);
        }
        Ok(pins)
    }

    fn confirmation(&self, target: &TargetRef) -> PathBuf {
        self.directory.join(format!("{}.json", target.id))
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
            let record: Confirmation = fs::read(self.confirmation(target))
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .filter(|record: &Confirmation| record.boot == boot)
                .ok_or_else(|| retryable("LUA_LSM_PREVIOUS_MODULE_UNCONFIRMED"))?;
            // Both template generations implement the same membership handover.
            if !matches!(
                names.get(&target.id),
                Some(&BASE_HOOKS | &HOOKS_WITH_CREATION)
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
            for process in process_family(&plan.process)? {
                verify_process(&process)?;
                mapping_preflight.check(process.pid)?;
                self.command(controls, &format!("seed/{}", process.pid))?;
                verify_process(&process)?;
            }
            return Ok(());
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
                self.command(controls, &format!("seed/{}", process.pid))?;
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

    fn clear_confirmation(&self, target: &TargetRef) -> io::Result<()> {
        match fs::remove_file(self.confirmation(target)) {
            Ok(()) => File::open(&self.directory)?.sync_all(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn save_confirmation(&self, path: &Path, record: &Confirmation) -> io::Result<()> {
        let temporary = self
            .directory
            .join(format!("receipt-{}", uuid::Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        let bytes = serde_json::to_vec(record).map_err(io::Error::other)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(&self.directory)?.sync_all()
    }

    fn ensure_directory(path: &Path) -> io::Result<()> {
        use std::path::Component;
        if !path.is_absolute() {
            return Err(io::Error::other("Lua-LSM state directory must be absolute"));
        }
        let uid = rustix::process::geteuid().as_raw();
        let mut parent = PathBuf::new();
        for component in path.components() {
            if !matches!(component, Component::RootDir | Component::Normal(_)) {
                return Err(io::Error::other("invalid Lua-LSM state directory"));
            }
            parent.push(component.as_os_str());
            match fs::symlink_metadata(&parent) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    match fs::DirBuilder::new().mode(0o700).create(&parent) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(error),
                    }
                }
                Err(error) => return Err(error),
                Ok(_) => {}
            }
            let metadata = fs::symlink_metadata(&parent)?;
            let sticky_root = metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
            if !metadata.is_dir()
                || metadata.uid() != 0 && metadata.uid() != uid
                || metadata.mode() & 0o022 != 0 && !sticky_root
                || parent == path && (metadata.uid() != uid || metadata.mode() & 0o077 != 0)
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "unsafe Lua-LSM state directory",
                ));
            }
        }
        Ok(())
    }

    fn command(&self, controls: &Path, command: &str) -> Result<(), Failure> {
        self.request(controls, command)
            .map_err(|_| retryable("LUA_LSM_INITIALIZATION_UNCONFIRMED"))
    }

    fn confirm_active(&self, controls: &Path) -> Result<Presence, Failure> {
        self.request(controls, "status")
            .map(|()| Presence::Present)
            .map_err(|_| retryable("LUA_LSM_ACTIVATION_UNCONFIRMED"))
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

    fn object_command(
        &self,
        controls: &Path,
        kind: &str,
        index: usize,
        relative: &[u8],
        path: &Path,
    ) -> Result<(), Failure> {
        // Resolve long relative paths inside the callback; command filenames
        // stay bounded and can be reused for each pinned descriptor number.
        let (kind, relative) = if kind == "tag" {
            (
                if relative.is_empty() { "root" } else { "tag" },
                b"".as_slice(),
            )
        } else {
            (kind, relative)
        };
        let flags = rustix::fs::OFlags::PATH | rustix::fs::OFlags::NOFOLLOW;
        let pinned = OpenOptions::new()
            .read(true)
            .custom_flags(flags.bits().cast_signed())
            .open(path)
            .map_err(local_error)?;
        self.descriptor_command(controls, kind, index, relative, &pinned)
    }

    fn descriptor_command(
        &self,
        controls: &Path,
        kind: &str,
        index: usize,
        relative: &[u8],
        pinned: &File,
    ) -> Result<(), Failure> {
        let reference = format!("/proc/self/fd/{}", pinned.as_raw_fd());
        let command = format!(
            "{kind}/{index}/{}/at/{}",
            encode(relative),
            encode(reference.as_bytes())
        );
        self.command(controls, &command)
    }

    fn tag_tree(
        &self,
        controls: &Path,
        index: usize,
        root: &Path,
        path: &Path,
        tree: bool,
        count: &mut usize,
    ) -> Result<(), Failure> {
        *count += 1;
        if *count > MAX_OBJECTS {
            return Err(rejected("LUA_LSM_OBJECT_LIMIT_EXCEEDED"));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| rejected("LUA_LSM_INVALID_PATH"))?;
        self.object_command(
            controls,
            "tag",
            index,
            relative.as_os_str().as_bytes(),
            path,
        )?;
        if tree && fs::symlink_metadata(path).map_err(local_error)?.is_dir() {
            for child in fs::read_dir(path).map_err(local_error)? {
                self.tag_tree(
                    controls,
                    index,
                    root,
                    &child.map_err(local_error)?.path(),
                    true,
                    count,
                )?;
            }
        }
        Ok(())
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
            let controls = self.directory.join(&prepared.target.id);
            current = report(&prepared.target, self.confirm_active(&controls));
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

fn valid_instance(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    if bytes.is_empty() {
        return "_".into();
    }
    let mut value = String::new();
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 && index % 100 == 0 {
            value.push('/');
        }
        write!(&mut value, "{byte:02x}")
            .unwrap_or_else(|_| unreachable!("String formatting is infallible"));
    }
    value
}

fn boot_id() -> io::Result<String> {
    Ok(fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .into())
}

fn identity(pid: u32) -> io::Result<(ProcessIdentity, u32)> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let end = stat
        .rfind(')')
        .ok_or_else(|| io::Error::other("malformed stat"))?;
    let fields: Vec<_> = stat[end + 1..].split_whitespace().collect();
    let parse = |index| {
        fields
            .get(index)
            .and_then(|v: &&str| v.parse::<u64>().ok())
            .ok_or_else(|| io::Error::other("malformed stat field"))
    };
    Ok((
        ProcessIdentity {
            boot_id: boot_id()?,
            pid,
            start_time: parse(19)?,
            pid_namespace: match fs::read_link(format!("/proc/{pid}/ns/pid")) {
                Ok(namespace) => namespace.to_string_lossy().into_owned(),
                Err(_) if matches!(fields.first().copied(), Some("Z" | "X")) => "<exited>".into(),
                Err(error) => return Err(error),
            },
        },
        u32::try_from(parse(1)?).map_err(io::Error::other)?,
    ))
}

fn verify_process(expected: &ProcessIdentity) -> Result<(), Failure> {
    if identity(expected.pid)
        .map(|(actual, _)| actual == *expected)
        .unwrap_or(false)
    {
        Ok(())
    } else {
        Err(rejected("LUA_LSM_PROCESS_IDENTITY_MISMATCH"))
    }
}

fn process_family(root: &ProcessIdentity) -> Result<Vec<ProcessIdentity>, Failure> {
    let mut processes = BTreeMap::new();
    for entry in fs::read_dir("/proc").map_err(local_error)? {
        let entry = entry.map_err(local_error)?;
        if let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
            && let Ok(record) = identity(pid)
        {
            processes.insert(pid, record);
        }
    }
    let mut family = BTreeSet::from([root.pid]);
    loop {
        let before = family.len();
        for (&pid, (_, parent)) in &processes {
            if family.contains(parent) {
                family.insert(pid);
            }
        }
        if before == family.len() {
            break;
        }
    }
    let mut selected = vec![root.clone()];
    selected.extend(
        family
            .into_iter()
            .filter(|pid| *pid != root.pid)
            .filter_map(|pid| processes.remove(&pid).map(|(process, _)| process)),
    );
    Ok(selected)
}

fn process_tasks() -> Result<Vec<ProcessIdentity>, Failure> {
    let mut tasks = BTreeMap::new();
    for entry in fs::read_dir("/proc").map_err(local_error)? {
        let entry = entry.map_err(local_error)?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(threads) = fs::read_dir(format!("/proc/{pid}/task")) else {
            continue;
        };
        for thread in threads {
            let thread = thread.map_err(local_error)?;
            if let Some(tid) = thread
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u32>().ok())
                && let Ok((process, _)) = identity(tid)
            {
                tasks.insert(tid, process);
            }
            if tasks.len() > MAX_OBJECTS {
                return Err(rejected("LUA_LSM_TASK_LIMIT_EXCEEDED"));
            }
        }
    }
    Ok(tasks.into_values().collect())
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

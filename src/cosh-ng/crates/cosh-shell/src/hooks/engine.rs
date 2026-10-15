#[cfg(test)]
use crate::hooks::model::HookTrigger;
use crate::hooks::model::{HookInput, HookMatcher};
#[cfg(test)]
use crate::types::HookProvenance;
use crate::types::{
    BuiltinFindingFacts, CommandBlock, CommandOrigin, EvaluatedHookFinding, FindingSeverity,
    HookFinding,
};
use loader::load_external_hook_configs;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::Arc;

// Each Unix registration owns a descriptor for the engine lifetime. Keep a
// fixed budget so project discovery cannot starve later process and log I/O.
const MAX_EXTERNAL_HOOKS: usize = 64;
// Total bytes the project-hook fingerprint streams before refusing. Hooks are
// small scripts, so the cap only exists to keep a project that can write its
// own `.cosh/hooks` from making startup or `doctor` read without bound.
pub(crate) const MAX_PROJECT_HOOK_FINGERPRINT_BYTES: u64 = 4 * 1024 * 1024;
const FINGERPRINT_CHUNK_BYTES: usize = 64 * 1024;
// Hold these descriptors while pinning hooks, then release them so later
// process, pipe, PTY, and logging operations retain real kernel headroom.
#[cfg(unix)]
const EXTERNAL_HOOK_FD_HEADROOM: usize = 32;

#[cfg(unix)]
fn reserve_external_hook_headroom(operation: &'static str) -> Option<Vec<std::fs::File>> {
    let reserve = loader::reserve_hook_descriptor_headroom(EXTERNAL_HOOK_FD_HEADROOM);
    if reserve.is_none() {
        tracing::warn!(
            target: "cosh_hook",
            operation,
            reserved_descriptors = EXTERNAL_HOOK_FD_HEADROOM,
            "insufficient descriptor headroom for external hooks"
        );
    }
    reserve
}

#[path = "engine/loader.rs"]
mod loader;
#[path = "engine/matcher.rs"]
mod matcher;
#[path = "engine/runtime.rs"]
mod runtime;

pub trait BuiltinHook: Send + Sync {
    fn id(&self) -> &str;
    fn matcher(&self) -> &HookMatcher;
    fn evaluate(&self, input: &HookInput) -> Option<HookFinding>;
    fn builtin_facts(&self, _input: &HookInput) -> Option<BuiltinFindingFacts> {
        None
    }
}

#[derive(Debug, Clone)]
pub struct ExternalHookConfig {
    pub path: PathBuf,
    pub matcher: HookMatcher,
    pub timeout_ms: u64,
    pub source: ExternalHookSource,
    pub project_root: Option<PathBuf>,
    pub trusted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalHookSource {
    User,
    Project,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredHookInfo {
    pub id: String,
    pub source: HookSourceInfo,
    pub path: Option<PathBuf>,
    pub project_root: Option<PathBuf>,
    pub trusted: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookSourceInfo {
    Builtin,
    ExternalUser,
    ExternalProject,
}

pub struct HookEngine {
    project_load_failed: bool,
    builtin_hooks: Vec<Box<dyn BuiltinHook>>,
    external_hooks: Vec<ExternalHookConfig>,
    #[cfg(unix)]
    external_hook_executables: Vec<Arc<std::fs::File>>,
}

impl Default for HookEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl HookEngine {
    pub fn new() -> Self {
        Self {
            project_load_failed: false,
            builtin_hooks: Vec::new(),
            external_hooks: Vec::new(),
            #[cfg(unix)]
            external_hook_executables: Vec::new(),
        }
    }

    pub fn register(&mut self, hook: Box<dyn BuiltinHook>) {
        self.builtin_hooks.push(hook);
    }

    pub fn register_external(&mut self, config: ExternalHookConfig) {
        if self.external_hooks.len() >= MAX_EXTERNAL_HOOKS {
            tracing::warn!(
                target: "cosh_hook",
                max_external_hooks = MAX_EXTERNAL_HOOKS,
                "external hook registry is full"
            );
            return;
        }
        #[cfg(unix)]
        {
            let Some(_descriptor_headroom) = reserve_external_hook_headroom("register") else {
                return;
            };
            let Some(executable) = loader::open_hook_executable(&config.path) else {
                return;
            };
            self.external_hook_executables.push(Arc::new(executable));
        }
        self.external_hooks.push(config);
    }

    pub fn load_hooks_from_dir(&mut self, dir: &Path) {
        self.load_external_hooks_from_dir(dir, ExternalHookSource::User, None, true);
    }

    pub fn load_project_hooks_from_root(&mut self, project_root: &Path, trusted: bool) {
        #[cfg(unix)]
        let Some(_descriptor_headroom) = reserve_external_hook_headroom("load_project") else {
            return;
        };
        let root = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf());
        let remaining = MAX_EXTERNAL_HOOKS.saturating_sub(self.external_hooks.len());
        let loaded = loader::load_project_external_hook_configs(&root, trusted, remaining);
        match loaded {
            Ok(loaded) => self.extend_external_hooks(loaded),
            Err(()) => self.project_load_failed = true,
        }
    }

    fn extend_external_hooks(&mut self, loaded: Vec<loader::LoadedExternalHookConfig>) {
        for loaded_hook in loaded {
            #[cfg(unix)]
            self.external_hook_executables.push(loaded_hook.executable);
            self.external_hooks.push(loaded_hook.config);
        }
    }

    fn load_external_hooks_from_dir(
        &mut self,
        dir: &Path,
        source: ExternalHookSource,
        project_root: Option<PathBuf>,
        trusted: bool,
    ) {
        #[cfg(unix)]
        let Some(_descriptor_headroom) = reserve_external_hook_headroom("load_external") else {
            return;
        };
        let remaining = MAX_EXTERNAL_HOOKS.saturating_sub(self.external_hooks.len());
        let loaded = load_external_hook_configs(dir, source, project_root, trusted, remaining);
        self.extend_external_hooks(loaded.unwrap_or_default());
    }

    pub fn evaluate(&self, block: &CommandBlock) -> Vec<EvaluatedHookFinding> {
        self.evaluate_with_disabled(block, &HashSet::new())
    }

    pub fn evaluate_with_disabled(
        &self,
        block: &CommandBlock,
        disabled_hooks: &HashSet<String>,
    ) -> Vec<EvaluatedHookFinding> {
        self.evaluate_with_disabled_and_origin(
            block,
            disabled_hooks,
            CommandOrigin::UserInteractive,
        )
    }

    pub fn evaluate_with_disabled_and_origin(
        &self,
        block: &CommandBlock,
        disabled_hooks: &HashSet<String>,
        origin: CommandOrigin,
    ) -> Vec<EvaluatedHookFinding> {
        let input = runtime::hook_input_from_block(block);
        let mut findings = Vec::new();
        for hook in &self.builtin_hooks {
            if disabled_hooks.contains(hook.id()) {
                continue;
            }
            if matcher::matches_command(hook.matcher(), &input) {
                if let Some(finding) = hook.evaluate(&input) {
                    findings.push(EvaluatedHookFinding::builtin_with_facts(
                        hook.id(),
                        finding,
                        hook.builtin_facts(&input),
                    ));
                }
            }
        }
        for (registration_index, ext) in self.external_hooks.iter().enumerate() {
            if disabled_hooks.contains(&ext.matcher.id) {
                continue;
            }
            if ext.source == ExternalHookSource::Project && !ext.trusted {
                continue;
            }
            if !external_hook_allowed_for_origin(ext, origin) {
                continue;
            }
            if matcher::matches_command(&ext.matcher, &input) {
                if let Some(finding) = runtime::run_external_hook(
                    ext,
                    #[cfg(unix)]
                    &self.external_hook_executables[registration_index],
                    &input,
                ) {
                    findings.push(EvaluatedHookFinding::external(
                        format!("external:{registration_index}"),
                        finding,
                    ));
                }
            }
        }
        for finding in &mut findings {
            runtime::redact_hook_finding(&mut finding.finding);
        }
        findings.sort_by_key(|f| match f.severity {
            FindingSeverity::Critical => 0,
            FindingSeverity::Warning => 1,
            FindingSeverity::Info => 2,
        });
        findings
    }

    pub fn registered_hooks(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self.builtin_hooks.iter().map(|h| h.id()).collect();
        for ext in &self.external_hooks {
            ids.push(&ext.matcher.id);
        }
        ids
    }

    pub fn registered_hook_infos(&self) -> Vec<RegisteredHookInfo> {
        let mut hooks = self
            .builtin_hooks
            .iter()
            .map(|hook| RegisteredHookInfo {
                id: hook.id().to_string(),
                source: HookSourceInfo::Builtin,
                path: None,
                project_root: None,
                trusted: None,
            })
            .collect::<Vec<_>>();
        for ext in &self.external_hooks {
            hooks.push(RegisteredHookInfo {
                id: ext.matcher.id.clone(),
                source: match ext.source {
                    ExternalHookSource::User => HookSourceInfo::ExternalUser,
                    ExternalHookSource::Project => HookSourceInfo::ExternalProject,
                },
                path: Some(ext.path.clone()),
                project_root: ext.project_root.clone(),
                trusted: Some(ext.trusted),
            });
        }
        hooks
    }

    pub fn set_project_hooks_trusted(&mut self, trusted: bool) -> usize {
        let mut updated = 0;
        for ext in &mut self.external_hooks {
            if ext.source == ExternalHookSource::Project {
                ext.trusted = trusted;
                updated += 1;
            }
        }
        updated
    }

    pub fn external_hooks(&self) -> &[ExternalHookConfig] {
        &self.external_hooks
    }

    /// Fingerprints the project hooks a fresh load of `project_root` would pin.
    ///
    /// For callers with no live engine — `doctor` reports hook trust without
    /// starting a shell — so the report is measured exactly the way `bootstrap`
    /// measures it. The probe owns the descriptors and drops them on return, so
    /// this pins only for the duration of the digest.
    pub fn project_hooks_pinned_digest_for_root(project_root: &Path) -> Option<String> {
        let mut probe = Self::new();
        probe.load_project_hooks_from_root(project_root, false);
        probe.project_hooks_pinned_digest()
    }

    /// Content fingerprint of the project hooks this engine has already pinned.
    ///
    /// The digest is measured from the same descriptors the engine executes, so
    /// the bytes a user consented to and the bytes that later run are one
    /// object. Fingerprinting by path instead would leave a window a project
    /// that can write its own `.cosh/hooks` could use to swap a file between the
    /// consent check and the load, passing old content while pinning new
    /// content. `None` means the pinned set could not be measured — a read error
    /// or a tree over [`MAX_PROJECT_HOOK_FINGERPRINT_BYTES`] — which callers must
    /// treat as "not consented" rather than as an empty tree.
    pub fn project_hooks_pinned_digest(&self) -> Option<String> {
        use sha2::{Digest, Sha256};

        if self.project_load_failed {
            return None;
        }
        // readdir order varies, so the digest order must not.
        let mut entries: Vec<(String, usize)> = self
            .external_hooks
            .iter()
            .enumerate()
            .filter(|(_, hook)| hook.source == ExternalHookSource::Project)
            .map(|(index, hook)| (hook_name_for_digest(&hook.path), index))
            .collect();
        entries.sort_by(|left, right| left.0.cmp(&right.0));

        let mut digest = Sha256::new();
        let mut remaining = MAX_PROJECT_HOOK_FINGERPRINT_BYTES;
        for (name, index) in entries {
            #[cfg(unix)]
            let hashed = {
                let file = self.external_hook_executables.get(index)?;
                fingerprint_pinned_into(file, &name, &mut digest, &mut remaining)
            };
            #[cfg(not(unix))]
            let hashed = fingerprint_path_into(
                &self.external_hooks[index].path,
                &name,
                &mut digest,
                &mut remaining,
            );
            hashed?;
        }
        Some(format!("sha256:{:x}", digest.finalize()))
    }
}

/// Hook name as it is spelled inside `.cosh/hooks`.
///
/// The loader is not recursive, so this name is unique within the fingerprinted
/// set and is the only identity a consent line needs beyond the content itself.
fn hook_name_for_digest(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Feeds one entry as `name \0 length \0 bytes`, length first.
///
/// The length prefix is what makes the encoding self-delimiting: hook bodies
/// are arbitrary bytes and may contain NULs, so a trailing length would leave
/// two different hook sets free to concatenate into the same digest input.
fn begin_digest_entry(name: &str, length: u64, digest: &mut sha2::Sha256) {
    use sha2::Digest;

    digest.update(name.as_bytes());
    digest.update([0u8]);
    digest.update(length.to_le_bytes());
    digest.update([0u8]);
}

/// Accounts `chunk` against the tree budget, failing closed once it is spent.
fn charge_fingerprint_budget(remaining: &mut u64, chunk: usize) -> Option<()> {
    let chunk = u64::try_from(chunk).ok()?;
    *remaining = remaining.checked_sub(chunk)?;
    Some(())
}

/// Streams an already-pinned hook into `digest` without moving its offset.
///
/// The descriptor is shared with the executor, so it is read positionally;
/// the length comes from the same pinned inode and the stream must match it, so
/// a truncated or over-budget read fails closed instead of fingerprinting a
/// prefix of content that is not what will run.
#[cfg(unix)]
fn fingerprint_pinned_into(
    file: &std::fs::File,
    name: &str,
    digest: &mut sha2::Sha256,
    remaining: &mut u64,
) -> Option<()> {
    use sha2::Digest;
    use std::os::unix::fs::FileExt;

    let declared = file.metadata().ok()?.len();
    begin_digest_entry(name, declared, digest);
    let mut buffer = vec![0u8; FINGERPRINT_CHUNK_BYTES];
    let mut offset = 0u64;
    loop {
        let read = file.read_at(&mut buffer, offset).ok()?;
        if read == 0 {
            break;
        }
        charge_fingerprint_budget(remaining, read)?;
        digest.update(&buffer[..read]);
        offset += u64::try_from(read).ok()?;
    }
    (offset == declared).then_some(())
}

#[cfg(not(unix))]
fn fingerprint_path_into(
    path: &Path,
    name: &str,
    digest: &mut sha2::Sha256,
    remaining: &mut u64,
) -> Option<()> {
    use sha2::Digest;
    use std::io::Read;

    let metadata = std::fs::symlink_metadata(path).ok()?;
    // Only a regular file can be fingerprinted; anything else is refused before
    // it is opened, so a FIFO can never block the digest.
    if !metadata.file_type().is_file() {
        return None;
    }
    let declared = metadata.len();
    begin_digest_entry(name, declared, digest);
    let mut file = std::fs::File::open(path).ok()?;
    let mut buffer = vec![0u8; FINGERPRINT_CHUNK_BYTES];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        charge_fingerprint_budget(remaining, read)?;
        digest.update(&buffer[..read]);
        total += u64::try_from(read).ok()?;
    }
    (total == declared).then_some(())
}

/// Origins where the command was explicitly typed or confirmed by the user
/// at their own shell prompt, as opposed to agent/internal automation paths.
fn is_user_shell_origin(origin: CommandOrigin) -> bool {
    matches!(
        origin,
        CommandOrigin::UserInteractive | CommandOrigin::UserSendToShell
    )
}

fn external_hook_allowed_for_origin(config: &ExternalHookConfig, origin: CommandOrigin) -> bool {
    match config.source {
        ExternalHookSource::User | ExternalHookSource::Project => is_user_shell_origin(origin),
    }
}

#[cfg(test)]
#[path = "engine/tests.rs"]
mod tests;

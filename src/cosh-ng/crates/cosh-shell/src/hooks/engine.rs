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
        self.extend_external_hooks(loaded);
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
        self.extend_external_hooks(loaded);
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

//! Deterministic file intent to a complete Lua-LSM module.

#![forbid(unsafe_code)]

use asc_policy_cedar::file_protection::{self, FileRule, PathScope};
use std::path::PathBuf;

use asc_file_policy::FileProtectionTarget;
use asc_lua_lsm_protocol::{
    DEFAULT_STATE_DIRECTORY, LUA_LSM_PLAN_FORMAT, LuaLsmPlan, is_supported_path,
    is_supported_pattern, render_module,
};
use asc_policy_types::{
    Validate,
    authoring::PolicySource,
    binding::PreparedBinding,
    scope::ScopeSelector,
    target::{AdapterFault, TargetBindingPlan, TranslationOutcome, TranslationRejection},
};

/// Select and validate automatic file assignments using Lua-LSM's path-pattern support.
pub const SELECTOR: asc_policy_cedar::deployment::DeploymentSelector =
    asc_policy_cedar::deployment::DeploymentSelector {
        backend: asc_policy_types::policy::DeploymentBackend::ActPlane,
        select: |input| input.selection(file_protection::select_operations(input)),
        place: file_protection::placement,
        validate: Some(|files| file_protection::translate_pst(files, cedar_target).map(|_| ())),
    };

/// Produces Lua modules from the same file intent used by `AgentSight`.
#[derive(Debug, Clone)]
pub struct LuaLsmTranslator {
    state_directory: PathBuf,
}

impl Default for LuaLsmTranslator {
    fn default() -> Self {
        Self::new(DEFAULT_STATE_DIRECTORY)
    }
}

impl LuaLsmTranslator {
    /// Uses the same private state directory as the matching Client.
    pub fn new(state_directory: impl Into<PathBuf>) -> Self {
        Self {
            state_directory: state_directory.into(),
        }
    }

    /// Translates Cedar file rules from a complete Binding without filesystem or kernel I/O.
    /// Composition consumes network rules through its separate projection.
    ///
    /// # Errors
    /// Returns encoding faults separately from unsupported policy semantics.
    pub fn translate(&self, binding: &PreparedBinding) -> Result<TranslationOutcome, AdapterFault> {
        self.translate_binding(binding, None)
    }

    /// Translates the validated file-domain PST of this immutable Binding snapshot.
    /// The trusted composition caller owns projection and schema validation.
    ///
    /// # Errors
    /// Returns encoding faults separately from unsupported policy semantics.
    pub fn translate_cedar_pst(
        &self,
        binding: &PreparedBinding,
        syntax: &cedar_policy::pst::PolicySet,
    ) -> Result<TranslationOutcome, AdapterFault> {
        self.translate_binding(binding, Some(syntax))
    }

    fn translate_binding(
        &self,
        binding: &PreparedBinding,
        syntax: Option<&cedar_policy::pst::PolicySet>,
    ) -> Result<TranslationOutcome, AdapterFault> {
        let reject = |code: &str| Ok(TranslationOutcome::Rejected(rejection(code)));
        if binding.validate().is_err() {
            return reject("INVALID_BINDING");
        }
        if matches!(binding.scope.selector, ScopeSelector::CgroupId { .. }) {
            return reject("UNSUPPORTED_SCOPE_SELECTOR");
        }
        let PolicySource::Cedar(source) = &binding.policy.template else {
            return reject("LUA_LSM_CEDAR_REQUIRED");
        };
        let policy = match syntax {
            Some(syntax) => file_protection::translate_pst(syntax, cedar_target),
            None => file_protection::translate(source, cedar_target),
        };
        let (policy, patterns) = match policy {
            Ok(policy) => policy,
            Err(error) => return Ok(TranslationOutcome::Rejected(error)),
        };
        let mut plan = LuaLsmPlan {
            binding_id: binding.binding_id.clone(),
            binding_revision: binding.binding_revision,
            process: binding.scope.process.clone(),
            policy,
            patterns,
            source: String::new(),
        };
        plan.source = render_module(&plan, &self.state_directory);
        let content = serde_json::to_vec(&plan).map_err(|_| AdapterFault {
            code: "LUA_LSM_ENCODING_FAILED".into(),
        })?;
        Ok(TranslationOutcome::Translated(TargetBindingPlan {
            format: LUA_LSM_PLAN_FORMAT.into(),
            content,
        }))
    }
}

// Keep the conservative literal prefix used by existing deployments. The full
// predicate, including Cedar escapes and literal question marks, executes in Lua.
fn scan_directory(value: &str) -> Option<&str> {
    if !value.starts_with('/') {
        return None;
    }
    let prefix = value.split(['*', '?', '[', ']', '{', '}', '\\']).next()?;
    let (directory, _) = prefix.rsplit_once('/')?;
    let directory = if directory.is_empty() { "/" } else { directory };
    is_supported_path(directory).then_some(directory)
}

fn cedar_target(rule: &FileRule) -> Result<Option<FileProtectionTarget>, &'static str> {
    if !rule.patterns.iter().all(is_supported_pattern) {
        return Err("UNSUPPORTED_PATH");
    }
    let mut scope = rule.range.clone();
    for pattern in rule.patterns.iter().filter(|p| !p.negated) {
        if let Some(dir) = scan_directory(&pattern.value) {
            let candidate = PathScope::Tree(dir.into());
            match &scope {
                Some(current) if candidate.contains(current) => {}
                Some(current) if !current.contains(&candidate) => return Ok(None),
                _ => scope = Some(candidate),
            }
        }
    }
    rule.target(&scope.ok_or("UNBOUNDED_PATH_SCOPE")?)
}

fn rejection(code: &str) -> TranslationRejection {
    TranslationRejection { code: code.into() }
}

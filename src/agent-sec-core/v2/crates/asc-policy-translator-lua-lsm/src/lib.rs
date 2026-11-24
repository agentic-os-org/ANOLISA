//! Deterministic file intent to a complete Lua-LSM module.

#![forbid(unsafe_code)]

use asc_policy_cedar::file_protection::{self, FileRule, PathScope};
use std::{collections::BTreeMap, path::PathBuf};

use asc_file_policy::{
    FILE_PROTECTION_POLICY_VERSION, FilePathPattern, FilePlan, FileProtectionPolicy,
    FileProtectionTarget, FileProtectionTargetKind, FileProtectionTopologyMode, FileProtections,
    PatternSyntax,
};
use asc_lua_lsm_protocol::{
    DEFAULT_STATE_DIRECTORY, LUA_LSM_PLAN_FORMAT, LuaLsmPlan, is_supported_path,
    is_supported_pattern, render_module,
};
use asc_policy_types::{
    Validate,
    authoring::{
        Action, Category, Comparison, Condition, Effect, PolicySource, Resource, Scalar, Value,
    },
    binding::PreparedBinding,
    scope::ScopeSelector,
    target::{AdapterFault, TargetBindingPlan, TranslationOutcome, TranslationRejection},
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

    /// Translates file rules from a complete Binding without filesystem or kernel I/O.
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
        let policy = match &binding.policy.template {
            PolicySource::Cedar(source) => {
                if let Some(syntax) = syntax {
                    file_protection::translate_pst(syntax, cedar_target)
                } else {
                    file_protection::translate(source, cedar_target)
                }
            }
            PolicySource::SecCore(_) if syntax.is_some() => {
                return reject("INVALID_CEDAR_POLICY");
            }
            PolicySource::SecCore(template) => native(template),
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

fn native(
    template: &asc_policy_types::authoring::PolicyTemplate,
) -> Result<FilePlan, TranslationRejection> {
    let mut patterns = BTreeMap::new();
    let mut targets = Vec::new();
    for (index, rule) in template.rules.iter().enumerate() {
        let reject = |code| rejection(&format!("RULE_{index}_{code}"));
        if rule.effect != Effect::Block
            || rule.category != Category::File
            || rule.previous.is_some()
        {
            return Err(reject("UNSUPPORTED_RULE"));
        }
        let operation = match &rule.condition {
            None if rule.action == Action::Read => "read",
            Some(Condition::Operation(Comparison::Eq(Value::Scalar(Scalar::String(
                operation,
            ))))) => operation,
            _ => return Err(reject("UNSUPPORTED_CONDITION")),
        };
        if !matches!(
            (rule.action, operation),
            (Action::Read, "read") | (Action::Write, "write" | "delete")
        ) {
            return Err(reject("UNSUPPORTED_ACTION"));
        }
        let Resource::File { path } = &rule.target else {
            return Err(reject("UNSUPPORTED_TARGET"));
        };
        if path.contains(['[', ']', '{', '}', '\\']) {
            return Err(reject("UNSUPPORTED_PATH"));
        }
        let (path, kind) = if let Some(prefix) = path.strip_suffix("/**")
            && !prefix.contains(['*', '?'])
        {
            (
                if prefix.is_empty() { "/" } else { prefix }.to_owned(),
                FileProtectionTargetKind::DirectoryTree,
            )
        } else if path.contains(['*', '?']) {
            let pattern = FilePathPattern {
                syntax: PatternSyntax::Glob,
                value: path.clone(),
                negated: false,
            };
            if !is_supported_pattern(&pattern) {
                return Err(reject("UNSUPPORTED_PATH"));
            }
            let root = scan_directory(&pattern.value)
                .ok_or_else(|| reject("UNSUPPORTED_PATH"))?
                .to_owned();
            patterns.insert(targets.len(), vec![pattern]);
            (root, FileProtectionTargetKind::DirectoryTree)
        } else {
            (path.clone(), FileProtectionTargetKind::ExactFile)
        };
        if !is_supported_path(&path) {
            return Err(reject("UNSUPPORTED_PATH"));
        }
        targets.push(FileProtectionTarget {
            path,
            kind,
            protections: FileProtections {
                read: operation == "read",
                ordinary_write: operation == "write",
                delete: operation == "delete",
            },
            exclusions: vec![],
        });
    }
    if targets.is_empty() || targets.len() > 64 {
        return Err(rejection("UNSUPPORTED_TARGET_COUNT"));
    }
    Ok((
        FileProtectionPolicy {
            version: FILE_PROTECTION_POLICY_VERSION,
            topology_mode: FileProtectionTopologyMode::StaticAtApply,
            targets,
        },
        patterns,
    ))
}

fn rejection(code: &str) -> TranslationRejection {
    TranslationRejection { code: code.into() }
}

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use regex::Regex;
use serde::de::{value::MapAccessDeserializer, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::config::{HookDefinition, HooksConfig};
use crate::provider::ToolDeclaration;

// ─── Hook Event Names ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookEventName {
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    UserPromptSubmit,
    SessionStart,
    Stop,
    BeforeModel,
    AfterModel,
}

impl HookEventName {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::PostToolUseFailure => "PostToolUseFailure",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::SessionStart => "SessionStart",
            Self::Stop => "Stop",
            Self::BeforeModel => "BeforeModel",
            Self::AfterModel => "AfterModel",
        }
    }
}

// ─── Hook IO Protocol ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct HookInput {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub cwd: String,
    pub hook_event_name: String,
    pub timestamp: String,
    /// copilot-shell 协议必填字段。cosh-ng 不维护会话 transcript
    /// 文件，用 cwd 派生路径占位，仅保证协议合规。
    pub transcript_path: String,
    #[serde(flatten)]
    pub event_data: Value,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct HookOutput {
    #[serde(rename = "continue")]
    pub should_continue: Option<bool>,
    #[serde(alias = "stopReason")]
    pub stop_reason: Option<String>,
    #[serde(alias = "suppressOutput")]
    pub suppress_output: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_present_string")]
    pub decision: Option<String>,
    pub reason: Option<String>,
    #[serde(alias = "systemMessage")]
    pub system_message: Option<String>,
    #[serde(alias = "hookSpecificOutput")]
    pub hook_specific_output: Option<Value>,
}

fn deserialize_present_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    String::deserialize(deserializer).map(Some)
}

struct ObjectOnlyHookOutput(HookOutput);

impl<'de> Deserialize<'de> for ObjectOnlyHookOutput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ObjectVisitor;

        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = ObjectOnlyHookOutput;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a hook output JSON object")
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                HookOutput::deserialize(MapAccessDeserializer::new(map)).map(ObjectOnlyHookOutput)
            }
        }

        deserializer.deserialize_map(ObjectVisitor)
    }
}

fn decode_hook_output(raw: &[u8]) -> Option<HookOutput> {
    serde_json::from_slice::<ObjectOnlyHookOutput>(raw)
        .ok()
        .map(|output| output.0)
}

/// Classifies why a hook could not produce a valid protocol response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookFailureKind {
    /// The child process could not be started.
    Spawn,
    /// The child process failed during I/O.
    Io,
    /// The hook exceeded its configured deadline.
    Timeout,
    /// The hook exited with an unexpected non-zero status.
    NonZero,
    /// The hook terminated because it received a signal.
    Signaled,
    /// The hook emitted malformed or non-object JSON.
    InvalidJson,
    /// The hook exited successfully without output.
    EmptyOutput,
    /// The hook's output exceeded the per-pipe size limit.
    OutputTruncated,
}

impl HookFailureKind {
    fn reason(self) -> &'static str {
        match self {
            Self::Spawn => "failed to start",
            Self::Io => "failed during execution",
            Self::Timeout => "timed out",
            Self::NonZero => "exited with a non-zero status",
            Self::Signaled => "terminated by signal",
            Self::InvalidJson => "returned invalid JSON",
            Self::EmptyOutput => "returned empty output",
            Self::OutputTruncated => "output exceeded the size limit",
        }
    }
}

/// A hook execution failure captured alongside the aggregate decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookFailure {
    /// Stable configured hook name.
    pub hook_name: String,
    /// Failure category, without exposing hook output or stderr.
    pub kind: HookFailureKind,
}

// ─── Aggregated Results ──────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum HookDecision {
    Allow,
    Block(String),
    /// The hook failed and the action was blocked for safety.
    HookFailure(String),
    Ask,
    Passthrough,
}

/// Notification message produced by a hook for display to the user.
#[derive(Debug, Clone)]
pub struct HookNotification {
    pub hook_name: String,
    pub message: String,
    /// The individual hook's decision (e.g. "allow", "ask", "block", "deny").
    /// Carried through the protocol so cosh-shell can color-code per-hook notices.
    pub decision: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PreToolUseResult {
    pub decision: HookDecision,
    pub tool_input_patch: Option<Value>,
    pub notifications: Vec<HookNotification>,
    /// Hook execution failures, including explicitly configured fail-open hooks.
    pub hook_failures: Vec<HookFailure>,
}

#[derive(Debug, Clone)]
pub struct PostToolUseResult {
    pub decision: HookDecision,
    pub additional_context: Option<String>,
    /// Replacement for the original tool response, emitted by transformation
    /// hooks (e.g. tokenless response compression).  When present and the
    /// decision is not Block, the runtime uses this text as the model-visible
    /// tool result instead of the original output.  `additional_context` is
    /// still appended after the replacement.
    pub updated_tool_response: Option<String>,
    pub notifications: Vec<HookNotification>,
}

#[derive(Debug, Clone)]
pub struct UserPromptResult {
    pub decision: HookDecision,
    pub additional_context: Option<String>,
    pub notifications: Vec<HookNotification>,
}

#[derive(Debug, Clone)]
pub struct SessionStartResult {
    pub additional_context: Option<String>,
    pub notifications: Vec<HookNotification>,
}

#[derive(Debug, Clone)]
pub struct StopResult {
    pub decision: HookDecision,
    pub notifications: Vec<HookNotification>,
}

/// Sandbox bypass request extracted from hook output.
#[derive(Debug, Clone, Deserialize)]
pub struct SandboxBypassRequest {
    pub original_command: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct PostToolUseFailureResult {
    pub notifications: Vec<HookNotification>,
    /// If present, a hook is requesting sandbox bypass approval.
    pub sandbox_bypass_request: Option<SandboxBypassRequest>,
}

#[derive(Debug, Clone)]
pub struct BeforeModelResult {
    pub notifications: Vec<HookNotification>,
    /// Tool declarations rewritten by a BeforeModel hook, e.g. schema
    /// compression. Only ever applies to the provider call that fired the hook:
    /// the ToolRegistry is untouched, so the next turn starts from the original
    /// declarations again. `None` means "use the originals".
    pub updated_tools: Option<Vec<ToolDeclaration>>,
}

#[derive(Debug, Clone)]
pub struct AfterModelResult {
    pub notifications: Vec<HookNotification>,
}

// ─── HookSystem ──────────────────────────────────────────────────────

pub struct HookSystem {
    enabled: bool,
    allow_extension_auto_enable: bool,
    disabled: HashSet<String>,
    hooks: HashMap<HookEventName, Vec<RegisteredHook>>,
    /// Current run_id, set at the start of each agent run.
    run_id: Option<String>,
}

struct RegisteredHook {
    definition: HookDefinition,
    accepts_empty_output: bool,
}

impl std::ops::Deref for RegisteredHook {
    type Target = HookDefinition;

    fn deref(&self) -> &Self::Target {
        &self.definition
    }
}

struct HookExecution {
    output: HookOutput,
    failure: Option<HookFailureKind>,
}

impl HookSystem {
    pub fn from_config(config: &HooksConfig) -> Self {
        let enabled = config.enabled;

        // Read disabled hooks from states/hooks.json
        let disabled = crate::state::load_disabled(crate::state::HOOKS_STATE);

        // Filter out hooks without name (enforced for all sources).
        let filter_named = |defs: &[HookDefinition]| -> Vec<RegisteredHook> {
            defs.iter()
                .filter(|d| {
                    if d.name.is_none() {
                        let command = crate::redaction::redact_text(&d.command);
                        tracing::warn!(target: "cosh_hook", "Skipping config hook without name: {command}");
                        false
                    } else {
                        true
                    }
                })
                .cloned()
                .map(|definition| RegisteredHook {
                    definition,
                    accepts_empty_output: false,
                })
                .collect()
        };

        let mut hooks: HashMap<HookEventName, Vec<RegisteredHook>> = HashMap::new();
        hooks.insert(
            HookEventName::PreToolUse,
            filter_named(&config.pre_tool_use),
        );
        hooks.insert(
            HookEventName::PostToolUse,
            filter_named(&config.post_tool_use),
        );
        hooks.insert(
            HookEventName::PostToolUseFailure,
            filter_named(&config.post_tool_use_failure),
        );
        hooks.insert(
            HookEventName::UserPromptSubmit,
            filter_named(&config.user_prompt_submit),
        );
        hooks.insert(
            HookEventName::SessionStart,
            filter_named(&config.session_start),
        );
        hooks.insert(HookEventName::Stop, filter_named(&config.stop));
        hooks.insert(
            HookEventName::BeforeModel,
            filter_named(&config.before_model),
        );
        hooks.insert(HookEventName::AfterModel, filter_named(&config.after_model));

        if !enabled && hooks.values().any(|defs| !defs.is_empty()) {
            eprintln!(
                "[cosh-core] Warning: config hooks are disabled by hooks.enabled=false; \
                 set hooks.enabled=true to activate them"
            );
            // Extension registration must not reactivate disabled config hooks.
            hooks.clear();
        }

        Self {
            enabled,
            allow_extension_auto_enable: config.enabled_override != Some(false),
            disabled,
            hooks,
            run_id: None,
        }
    }

    pub fn new_disabled() -> Self {
        Self {
            enabled: false,
            allow_extension_auto_enable: true,
            disabled: HashSet::new(),
            hooks: HashMap::new(),
            run_id: None,
        }
    }

    /// Set the current run_id for this agent run (used in hook inputs).
    pub(crate) fn set_run_id(&mut self, run_id: String) {
        self.run_id = Some(run_id);
    }

    /// Dynamically append hook definitions from extensions.
    /// Extension hooks are appended to the end of each event's hook list.
    ///
    /// If extensions provide non-empty hooks, the hook system is automatically
    /// enabled (extensions are explicitly installed by the user, implying intent
    /// to use their hooks). The user can still force-disable via config if needed.
    ///
    /// The extension format uses nested `HookGroup` structures (matching
    /// copilot-shell's format). Groups are flattened into individual
    /// `HookDefinition` entries with group-level matcher/sequential inherited.
    pub fn register_extension_hooks(&mut self, hooks: &crate::extension::ExtensionHooks) {
        use crate::extension::config::flatten_hook_groups;

        if hooks.is_empty() || !self.allow_extension_auto_enable {
            return;
        }

        // Auto-enable: extensions are user-installed, so their hooks should fire.
        self.enabled = true;

        // Helper: flatten hook groups and filter out hooks without a name field.
        let filter_named = |groups: &[crate::extension::config::HookGroup]| -> Vec<HookDefinition> {
            flatten_hook_groups(groups)
                .into_iter()
                .filter(|d| {
                    if d.name.is_none() {
                        let command = crate::redaction::redact_text(&d.command);
                        tracing::warn!(target: "cosh_hook", "Skipping hook without name: {command}");
                        false
                    } else {
                        true
                    }
                })
                .collect()
        };

        for (event, groups) in [
            (HookEventName::PreToolUse, hooks.pre_tool_use.as_slice()),
            (HookEventName::PostToolUse, hooks.post_tool_use.as_slice()),
            (
                HookEventName::UserPromptSubmit,
                hooks.user_prompt_submit.as_slice(),
            ),
            (HookEventName::SessionStart, hooks.session_start.as_slice()),
            (HookEventName::Stop, hooks.stop.as_slice()),
            (
                HookEventName::PostToolUseFailure,
                hooks.post_tool_use_failure.as_slice(),
            ),
            (HookEventName::BeforeModel, hooks.before_model.as_slice()),
            (HookEventName::AfterModel, hooks.after_model.as_slice()),
        ] {
            self.hooks
                .entry(event)
                .or_default()
                .extend(
                    filter_named(groups)
                        .into_iter()
                        .map(|definition| RegisteredHook {
                            definition,
                            accepts_empty_output: true,
                        }),
                );
        }
    }

    fn active_hooks(&self, event: HookEventName) -> Vec<&RegisteredHook> {
        self.hooks
            .get(&event)
            .map(|defs| {
                defs.iter()
                    .filter(|d| {
                        match &d.name {
                            Some(name) => !self.disabled.contains(name),
                            // Defensive: hooks without name from config.toml still execute
                            None => true,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// cosh-ng 内部工具名 ↔ copilot-shell 标准名 双向别名映射。
    /// 仅用于 matcher 匹配阶段，不影响发给 hook 的 tool_name 字段。
    fn tool_name_alias(tool_name: &str) -> Option<&'static str> {
        match tool_name {
            "shell" => Some("run_shell_command"),
            "run_shell_command" => Some("shell"),
            "grep" => Some("grep_search"),
            "grep_search" => Some("grep"),
            "todo" => Some("todo_write"),
            "todo_write" => Some("todo"),
            _ => None,
        }
    }

    /// 从 hook_specific_output 读取 additionalContext，兼容两种字段名。
    /// 优先 snake_case（cosh-ng 原协议），次选 camelCase（copilot-shell 协议）。
    fn pick_additional_context(specific: &Value) -> Option<&str> {
        specific
            .get("additional_context")
            .and_then(|v| v.as_str())
            .or_else(|| specific.get("additionalContext").and_then(|v| v.as_str()))
    }

    /// Read `updated_tool_response` / `updatedToolResponse` from hook output.
    /// Accepts both snake_case (cosh-ng convention) and camelCase
    /// (copilot-shell convention).  Returns `None` when the field is absent,
    /// not a string, or empty — callers treat all three cases as "no
    /// replacement requested".
    fn pick_updated_tool_response(specific: &Value) -> Option<&str> {
        specific
            .get("updated_tool_response")
            .and_then(|v| v.as_str())
            .or_else(|| specific.get("updatedToolResponse").and_then(|v| v.as_str()))
            .filter(|s| !s.is_empty())
    }

    /// 把 tool 输出文本包装为 copilot-shell 协议要求的 JSON 对象。
    /// 对齐 copilot-shell 行为：始终将原始文本作为 llmContent/returnDisplay
    /// 传递，即使文本本身是合法 JSON。copilot-shell 的 coreToolScheduler
    /// 会先提取文本再包装，hook 脚本始终看到统一结构。
    fn wrap_tool_response(tool_response: &str) -> Value {
        serde_json::json!({
            "llmContent": tool_response,
            "returnDisplay": tool_response,
        })
    }

    fn matches_tool(def: &HookDefinition, tool_name: &str) -> bool {
        match &def.matcher {
            None => true,
            Some(pattern) => {
                let matches_one = |name: &str| {
                    if let Ok(re) = Regex::new(pattern) {
                        re.is_match(name)
                    } else {
                        pattern == name
                    }
                };
                matches_one(tool_name) || Self::tool_name_alias(tool_name).is_some_and(matches_one)
            }
        }
    }

    fn is_sequential(defs: &[&RegisteredHook]) -> bool {
        defs.iter().any(|d| d.sequential.unwrap_or(false))
    }

    fn timeout_for(def: &HookDefinition) -> Duration {
        Duration::from_millis(def.timeout.unwrap_or(60_000))
    }

    fn hook_name(def: &HookDefinition, index: usize) -> String {
        def.name.clone().unwrap_or_else(|| format!("hook-{index}"))
    }

    // ─── Fire methods ────────────────────────────────────────────────

    pub async fn fire_pre_tool_use(
        &self,
        session_id: &str,
        cwd: &str,
        tool_use_id: &str,
        tool_name: &str,
        tool_input: &Value,
        skill_context: Option<&Value>,
    ) -> PreToolUseResult {
        if !self.enabled {
            return PreToolUseResult {
                decision: HookDecision::Passthrough,
                tool_input_patch: None,
                notifications: vec![],
                hook_failures: vec![],
            };
        }

        let defs: Vec<&RegisteredHook> = self
            .active_hooks(HookEventName::PreToolUse)
            .into_iter()
            .filter(|d| Self::matches_tool(d, tool_name))
            .collect();

        if defs.is_empty() {
            return PreToolUseResult {
                decision: HookDecision::Passthrough,
                tool_input_patch: None,
                notifications: vec![],
                hook_failures: vec![],
            };
        }

        let mut event_data = serde_json::json!({
            "tool_use_id": tool_use_id,
            "tool_name": tool_name,
            "tool_input": tool_input,
        });
        if let Some(ctx) = skill_context {
            event_data["skill_context"] = ctx.clone();
        }
        let input = self.build_input(session_id, cwd, HookEventName::PreToolUse, event_data);
        let outputs = self.run_hooks(&defs, &input).await;

        self.aggregate_pre_tool_use(outputs, &defs)
    }

    pub async fn fire_post_tool_use(
        &self,
        session_id: &str,
        cwd: &str,
        tool_use_id: &str,
        tool_name: &str,
        tool_input: &Value,
        tool_response: &str,
        skill_context: Option<&Value>,
    ) -> PostToolUseResult {
        if !self.enabled {
            return PostToolUseResult {
                decision: HookDecision::Passthrough,
                additional_context: None,
                updated_tool_response: None,
                notifications: vec![],
            };
        }

        let defs: Vec<&RegisteredHook> = self
            .active_hooks(HookEventName::PostToolUse)
            .into_iter()
            .filter(|d| Self::matches_tool(d, tool_name))
            .collect();

        if defs.is_empty() {
            return PostToolUseResult {
                decision: HookDecision::Passthrough,
                additional_context: None,
                updated_tool_response: None,
                notifications: vec![],
            };
        }

        let mut event_data = serde_json::json!({
            "tool_use_id": tool_use_id,
            "tool_name": tool_name,
            "tool_input": tool_input,
            "tool_response": Self::wrap_tool_response(tool_response),
        });
        if let Some(ctx) = skill_context {
            event_data["skill_context"] = ctx.clone();
        }
        let input = self.build_input(session_id, cwd, HookEventName::PostToolUse, event_data);
        let outputs = self.run_hooks(&defs, &input).await;

        self.aggregate_post_tool_use(outputs, &defs)
    }

    pub async fn fire_user_prompt_submit(
        &self,
        session_id: &str,
        cwd: &str,
        prompt: &str,
    ) -> UserPromptResult {
        if !self.enabled {
            return UserPromptResult {
                decision: HookDecision::Passthrough,
                additional_context: None,
                notifications: vec![],
            };
        }

        let defs = self.active_hooks(HookEventName::UserPromptSubmit);
        if defs.is_empty() {
            return UserPromptResult {
                decision: HookDecision::Passthrough,
                additional_context: None,
                notifications: vec![],
            };
        }

        let event_data = serde_json::json!({ "prompt": prompt });
        let input = self.build_input(session_id, cwd, HookEventName::UserPromptSubmit, event_data);
        let outputs = self.run_hooks(&defs, &input).await;

        self.aggregate_user_prompt(outputs, &defs)
    }

    pub async fn fire_session_start(&self, session_id: &str, cwd: &str) -> SessionStartResult {
        if !self.enabled {
            return SessionStartResult {
                additional_context: None,
                notifications: vec![],
            };
        }

        let defs = self.active_hooks(HookEventName::SessionStart);
        if defs.is_empty() {
            return SessionStartResult {
                additional_context: None,
                notifications: vec![],
            };
        }

        let event_data = serde_json::json!({ "source": "startup" });
        let input = self.build_input(session_id, cwd, HookEventName::SessionStart, event_data);
        let outputs = self.run_hooks(&defs, &input).await;

        self.aggregate_session_start(outputs, &defs)
    }

    pub async fn fire_stop(&self, session_id: &str, cwd: &str, last_message: &str) -> StopResult {
        if !self.enabled {
            return StopResult {
                decision: HookDecision::Passthrough,
                notifications: vec![],
            };
        }

        let defs = self.active_hooks(HookEventName::Stop);
        if defs.is_empty() {
            return StopResult {
                decision: HookDecision::Passthrough,
                notifications: vec![],
            };
        }

        let event_data = serde_json::json!({ "last_assistant_message": last_message });
        let input = self.build_input(session_id, cwd, HookEventName::Stop, event_data);
        let outputs = self.run_hooks(&defs, &input).await;

        self.aggregate_stop(outputs, &defs)
    }

    pub async fn fire_post_tool_use_failure(
        &self,
        session_id: &str,
        cwd: &str,
        tool_use_id: &str,
        tool_name: &str,
        tool_input: &Value,
        error: &str,
        skill_context: Option<&Value>,
    ) -> PostToolUseFailureResult {
        if !self.enabled {
            return PostToolUseFailureResult {
                notifications: vec![],
                sandbox_bypass_request: None,
            };
        }

        let defs: Vec<&RegisteredHook> = self
            .active_hooks(HookEventName::PostToolUseFailure)
            .into_iter()
            .filter(|d| Self::matches_tool(d, tool_name))
            .collect();

        if defs.is_empty() {
            return PostToolUseFailureResult {
                notifications: vec![],
                sandbox_bypass_request: None,
            };
        }

        let mut event_data = serde_json::json!({
            "tool_use_id": tool_use_id,
            "tool_name": tool_name,
            "tool_input": tool_input,
            "error": error,
        });
        if let Some(ctx) = skill_context {
            event_data["skill_context"] = ctx.clone();
        }
        let input = self.build_input(
            session_id,
            cwd,
            HookEventName::PostToolUseFailure,
            event_data,
        );
        let outputs = self.run_hooks(&defs, &input).await;

        let mut notifications = Vec::new();
        let mut sandbox_bypass_request = None;
        for (i, execution) in outputs {
            let out = execution.output;
            let name = Self::hook_name(defs[i], i);
            self.collect_notifications(&out, &name, &mut notifications);
            // Extract sandbox_bypass_request from hookSpecificOutput (last valid wins).
            if let Some(ref specific) = out.hook_specific_output {
                if let Some(req) = specific.get("sandbox_bypass_request") {
                    if let Ok(parsed) = serde_json::from_value::<SandboxBypassRequest>(req.clone())
                    {
                        sandbox_bypass_request = Some(parsed);
                    }
                }
            }
        }
        PostToolUseFailureResult {
            notifications,
            sandbox_bypass_request,
        }
    }

    /// Temporarily disable/enable a hook by name (used for sandbox bypass).
    /// Not persisted to states/hooks.json — only affects the current session.
    pub(crate) fn set_hook_disabled(&mut self, hook_name: &str, disabled: bool) {
        if disabled {
            self.disabled.insert(hook_name.to_string());
        } else {
            self.disabled.remove(hook_name);
        }
    }

    pub async fn fire_before_model(
        &self,
        session_id: &str,
        cwd: &str,
        model: &str,
        messages: &[crate::provider::Message],
        tools: &[ToolDeclaration],
    ) -> BeforeModelResult {
        if !self.enabled {
            return BeforeModelResult {
                notifications: vec![],
                updated_tools: None,
            };
        }

        let defs = self.active_hooks(HookEventName::BeforeModel);
        if defs.is_empty() {
            return BeforeModelResult {
                notifications: vec![],
                updated_tools: None,
            };
        }

        // Mirrors copilot-shell's hookTranslator.toHookLLMRequest shape:
        // llm_request carries the model, the messages array, and the tool
        // declarations at config.tools.
        let hook_messages: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                serde_json::json!({
                    "role": &m.role,
                    "content": m.content.as_text(),
                })
            })
            .collect();

        let event_data = serde_json::json!({
            "llm_request": {
                "model": model,
                "messages": hook_messages,
                "config": {
                    "tools": tools,
                },
            },
        });
        let input = self.build_input(session_id, cwd, HookEventName::BeforeModel, event_data);
        let outputs = self.run_hooks(&defs, &input).await;

        let mut notifications = Vec::new();
        let mut updated_tools = None;
        for (i, execution) in outputs {
            let out = execution.output;
            let name = Self::hook_name(defs[i], i);
            self.collect_notifications(&out, &name, &mut notifications);
            // Last valid full array in configuration order wins; a rejected
            // array leaves an earlier hook's accepted one in place.
            if let Some(candidate) = extract_updated_tools(&out.hook_specific_output) {
                match validate_updated_tools(tools, candidate) {
                    Ok(validated) => updated_tools = Some(validated),
                    Err(reason) => {
                        tracing::warn!(
                            target: "cosh_hook",
                            "Hook '{name}' tool declaration update rejected: {reason}"
                        );
                    }
                }
            }
        }
        BeforeModelResult {
            notifications,
            updated_tools,
        }
    }

    pub async fn fire_after_model(
        &self,
        session_id: &str,
        cwd: &str,
        has_tool_calls: bool,
        response_text: &str,
        model: &str,
        messages: &[crate::provider::Message],
        usage: Option<(u32, u32, u32)>,
    ) -> AfterModelResult {
        if !self.enabled {
            return AfterModelResult {
                notifications: vec![],
            };
        }

        let defs = self.active_hooks(HookEventName::AfterModel);
        if defs.is_empty() {
            return AfterModelResult {
                notifications: vec![],
            };
        }

        // 对齐 copilot-shell hookTranslator 格式：
        // llm_request: {model, messages}
        // llm_response: {text, candidates, usageMetadata}
        let hook_messages: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                serde_json::json!({
                    "role": &m.role,
                    "content": m.content.as_text(),
                })
            })
            .collect();

        let mut llm_response = serde_json::json!({
            "text": response_text,
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [response_text],
                },
                "finishReason": "STOP",
            }],
        });
        if let Some((prompt_tokens, completion_tokens, total_tokens)) = usage {
            llm_response["usageMetadata"] = serde_json::json!({
                "promptTokenCount": prompt_tokens,
                "candidatesTokenCount": completion_tokens,
                "totalTokenCount": total_tokens,
            });
        }

        let event_data = serde_json::json!({
            "has_tool_calls": has_tool_calls,
            "llm_request": {
                "model": model,
                "messages": hook_messages,
            },
            "llm_response": llm_response,
        });
        let input = self.build_input(session_id, cwd, HookEventName::AfterModel, event_data);
        let outputs = self.run_hooks(&defs, &input).await;

        let mut notifications = Vec::new();
        for (i, execution) in outputs {
            let out = execution.output;
            let name = Self::hook_name(defs[i], i);
            self.collect_notifications(&out, &name, &mut notifications);
        }
        AfterModelResult { notifications }
    }

    // ─── Internal helpers ────────────────────────────────────────────

    fn build_input(
        &self,
        session_id: &str,
        cwd: &str,
        event: HookEventName,
        event_data: Value,
    ) -> HookInput {
        HookInput {
            session_id: session_id.to_string(),
            run_id: self.run_id.clone(),
            cwd: cwd.to_string(),
            hook_event_name: event.as_str().to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            transcript_path: format!("{cwd}/.cosh-transcript.jsonl"),
            event_data,
        }
    }

    async fn run_hooks(
        &self,
        defs: &[&RegisteredHook],
        input: &HookInput,
    ) -> Vec<(usize, HookExecution)> {
        let input_json = crate::redaction::to_redacted_json_with_schemas(
            input,
            crate::redaction::TOOL_DECLARATION_PATHS,
        );

        if Self::is_sequential(defs) {
            let mut results = Vec::new();
            for (i, def) in defs.iter().enumerate() {
                let output = Self::run_single_hook(def, &input_json).await;
                results.push((i, output));
            }
            results
        } else {
            let futs: Vec<_> = defs
                .iter()
                .enumerate()
                .map(|(i, def)| {
                    let json = input_json.clone();
                    let cmd = def.command.clone();
                    let env = def.env.clone();
                    let timeout = Self::timeout_for(def);
                    let accepts_empty_output = def.accepts_empty_output;
                    async move {
                        let output =
                            Self::run_hook_cmd(&cmd, &env, &json, timeout, accepts_empty_output)
                                .await;
                        (i, output)
                    }
                })
                .collect();
            futures::future::join_all(futs).await
        }
    }

    async fn run_single_hook(def: &RegisteredHook, input_json: &str) -> HookExecution {
        Self::run_hook_cmd(
            &def.command,
            &def.env,
            input_json,
            Self::timeout_for(def),
            def.accepts_empty_output,
        )
        .await
    }

    async fn run_hook_cmd(
        command: &str,
        env: &BTreeMap<String, String>,
        input_json: &str,
        timeout: Duration,
        accepts_empty_output: bool,
    ) -> HookExecution {
        use tokio::process::Command;

        use crate::process::{output_with_timeout, OutputError, MAX_PIPE_OUTPUT_BYTES};

        let safe_command = crate::redaction::redact_text(command);
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);

        // `Command` inherits the parent environment by default, so declared
        // entries override inherited values of the same name for this child
        // only. Invalid names are dropped with the name (never the value) in
        // the log.
        let (valid, invalid): (Vec<_>, Vec<_>) = env
            .iter()
            .partition(|(name, _)| crate::config::is_valid_env_name(name));
        if !invalid.is_empty() {
            let names: Vec<&str> = invalid.iter().map(|(name, _)| name.as_str()).collect();
            tracing::warn!(
                target: "cosh_hook",
                "Hook '{safe_command}' declares invalid env names, skipped: {}",
                names.join(", ")
            );
        }
        cmd.envs(valid);

        // Host attribution, applied after the declared map so it takes
        // precedence over an `env` entry of the same name. This is a
        // cooperative signal, not a security boundary: the same manifest also
        // owns `command`, so it can reassign or unset these inside the shell it
        // asked for. Nothing security-relevant may depend on them.
        cmd.env("COSH_RUNTIME", "cosh-ng");
        cmd.env("COSH_NG_VERSION", env!("CARGO_PKG_VERSION"));

        // The deadline covers the stdin write as well: a hook that never
        // reads stdin must not stall the session, and on timeout the whole
        // process group is killed instead of leaking grandchildren. Output
        // collection is size-capped per pipe so a runaway hook cannot
        // exhaust memory (issue #2841).
        let result = output_with_timeout(cmd, Some(input_json.as_bytes().to_vec()), timeout).await;

        let output = match result {
            Ok(o) => o,
            Err(OutputError::Spawn(e)) => {
                tracing::error!(target: "cosh_hook", "Failed to spawn hook '{safe_command}': {e}");
                return HookExecution {
                    output: HookOutput::default(),
                    failure: Some(HookFailureKind::Spawn),
                };
            }
            Err(OutputError::Io(e)) => {
                tracing::error!(target: "cosh_hook", "Hook '{safe_command}' execution failed: {e}");
                return HookExecution {
                    output: HookOutput::default(),
                    failure: Some(HookFailureKind::Io),
                };
            }
            Err(OutputError::Timeout) => {
                tracing::warn!(target: "cosh_hook", "Hook '{safe_command}' timed out");
                return HookExecution {
                    output: HookOutput::default(),
                    failure: Some(HookFailureKind::Timeout),
                };
            }
        };

        if output.stdout_truncated || output.stderr_truncated {
            tracing::warn!(
                target: "cosh_hook",
                "Hook '{safe_command}' output exceeded {MAX_PIPE_OUTPUT_BYTES} bytes; \
                 the process group was killed and the output was cut short"
            );
            return HookExecution {
                output: HookOutput::default(),
                failure: Some(HookFailureKind::OutputTruncated),
            };
        }

        let Some(exit_code) = output.status.code() else {
            tracing::warn!(target: "cosh_hook", "Hook '{safe_command}' terminated by signal");
            return HookExecution {
                output: HookOutput::default(),
                failure: Some(HookFailureKind::Signaled),
            };
        };

        match exit_code {
            0 => {
                if output.stdout.iter().all(u8::is_ascii_whitespace) {
                    if accepts_empty_output {
                        return HookExecution {
                            output: HookOutput::default(),
                            failure: None,
                        };
                    }
                    tracing::warn!(target: "cosh_hook", "Hook '{safe_command}' returned empty output");
                    return HookExecution {
                        output: HookOutput::default(),
                        failure: Some(HookFailureKind::EmptyOutput),
                    };
                }
                match decode_hook_output(&output.stdout) {
                    Some(output)
                        if classify_hook_decision(output.decision.as_deref())
                            != HookDecisionClass::Invalid =>
                    {
                        HookExecution {
                            output: Self::redact_hook_output(output),
                            failure: None,
                        }
                    }
                    Some(_) | None => {
                        tracing::warn!(
                            target: "cosh_hook",
                            "Hook '{safe_command}' returned invalid JSON"
                        );
                        HookExecution {
                            output: HookOutput::default(),
                            failure: Some(HookFailureKind::InvalidJson),
                        }
                    }
                }
            }
            2 => {
                // System block via exit code 2
                let stderr = String::from_utf8_lossy(&output.stderr);
                let reason = if stderr.trim().is_empty() {
                    "Blocked by hook (exit 2)".to_string()
                } else {
                    stderr.trim().to_string()
                };
                HookExecution {
                    output: Self::redact_hook_output(HookOutput {
                        decision: Some("block".to_string()),
                        reason: Some(reason),
                        ..Default::default()
                    }),
                    failure: None,
                }
            }
            _ => {
                // Non-zero (not 2) is a hook execution failure. Keep stderr
                // out of the result and logs because hooks may write secrets.
                tracing::warn!(
                    target: "cosh_hook",
                    "Hook '{safe_command}' exited with unexpected status"
                );
                HookExecution {
                    output: HookOutput::default(),
                    failure: Some(HookFailureKind::NonZero),
                }
            }
        }
    }

    fn redact_hook_output(mut output: HookOutput) -> HookOutput {
        output.stop_reason = output
            .stop_reason
            .map(|value| crate::redaction::redact_text(&value));
        output.reason = output
            .reason
            .map(|value| crate::redaction::redact_text(&value));
        output.system_message = output
            .system_message
            .map(|value| crate::redaction::redact_text(&value));
        if let Some(value) = &mut output.hook_specific_output {
            crate::redaction::redact_value_with_schemas(
                value,
                crate::redaction::TOOL_DECLARATION_PATHS,
            );
        }
        output
    }

    // ─── Aggregation ─────────────────────────────────────────────────

    fn aggregate_pre_tool_use(
        &self,
        outputs: Vec<(usize, HookExecution)>,
        defs: &[&RegisteredHook],
    ) -> PreToolUseResult {
        let mut decision = HookDecision::Passthrough;
        let mut tool_input_patch: Option<Value> = None;
        let mut notifications = Vec::new();
        let mut hook_failures = Vec::new();

        for (i, execution) in outputs {
            let name = Self::hook_name(defs[i], i);
            if let Some(kind) = execution.failure {
                hook_failures.push(HookFailure {
                    hook_name: name.clone(),
                    kind,
                });
                notifications.push(HookNotification {
                    hook_name: name,
                    message: format!("Hook failed: {}", kind.reason()),
                    decision: Some("hook_failure".to_string()),
                });
                if !defs[i].fail_open {
                    decision = fold_hook_failure(decision, kind.reason());
                }
                continue;
            }
            let out = execution.output;
            self.collect_notifications(&out, &name, &mut notifications);

            decision = fold_decision(decision, out.decision.as_deref(), out.reason.clone());

            if let Some(ref specific) = out.hook_specific_output {
                if let Some(patch) = specific.get("tool_input") {
                    tool_input_patch = Some(match tool_input_patch {
                        Some(existing) => merge_json(existing, patch.clone()),
                        None => patch.clone(),
                    });
                }
            }
        }

        PreToolUseResult {
            decision,
            tool_input_patch,
            notifications,
            hook_failures,
        }
    }

    fn aggregate_post_tool_use(
        &self,
        outputs: Vec<(usize, HookExecution)>,
        defs: &[&RegisteredHook],
    ) -> PostToolUseResult {
        let mut decision = HookDecision::Passthrough;
        let mut additional_context: Option<String> = None;
        let mut updated_tool_response: Option<String> = None;
        let mut notifications = Vec::new();

        for (i, execution) in outputs {
            let out = execution.output;
            let name = Self::hook_name(defs[i], i);
            self.collect_notifications(&out, &name, &mut notifications);

            decision = fold_decision(decision, out.decision.as_deref(), out.reason.clone());
            fold_additional_context(&mut additional_context, &out.hook_specific_output);

            // Last valid replacement wins (configuration order).
            if let Some(ref specific) = out.hook_specific_output {
                if let Some(replacement) = Self::pick_updated_tool_response(specific) {
                    updated_tool_response = Some(replacement.to_string());
                }
            }
        }

        PostToolUseResult {
            decision,
            additional_context,
            updated_tool_response,
            notifications,
        }
    }

    fn aggregate_user_prompt(
        &self,
        outputs: Vec<(usize, HookExecution)>,
        defs: &[&RegisteredHook],
    ) -> UserPromptResult {
        let mut decision = HookDecision::Passthrough;
        let mut additional_context: Option<String> = None;
        let mut notifications = Vec::new();

        for (i, execution) in outputs {
            let out = execution.output;
            let name = Self::hook_name(defs[i], i);
            self.collect_notifications(&out, &name, &mut notifications);

            decision = fold_decision(decision, out.decision.as_deref(), out.reason.clone());
            fold_additional_context(&mut additional_context, &out.hook_specific_output);
        }

        UserPromptResult {
            decision,
            additional_context,
            notifications,
        }
    }

    fn aggregate_session_start(
        &self,
        outputs: Vec<(usize, HookExecution)>,
        defs: &[&RegisteredHook],
    ) -> SessionStartResult {
        let mut additional_context: Option<String> = None;
        let mut notifications = Vec::new();

        for (i, execution) in outputs {
            let out = execution.output;
            let name = Self::hook_name(defs[i], i);
            self.collect_notifications(&out, &name, &mut notifications);

            fold_additional_context(&mut additional_context, &out.hook_specific_output);
        }

        SessionStartResult {
            additional_context,
            notifications,
        }
    }

    fn aggregate_stop(
        &self,
        outputs: Vec<(usize, HookExecution)>,
        defs: &[&RegisteredHook],
    ) -> StopResult {
        let mut decision = HookDecision::Passthrough;
        let mut notifications = Vec::new();

        for (i, execution) in outputs {
            let out = execution.output;
            let name = Self::hook_name(defs[i], i);
            self.collect_notifications(&out, &name, &mut notifications);

            decision = fold_decision(decision, out.decision.as_deref(), out.reason.clone());
        }

        StopResult {
            decision,
            notifications,
        }
    }

    fn collect_notifications(
        &self,
        output: &HookOutput,
        hook_name: &str,
        notifications: &mut Vec<HookNotification>,
    ) {
        // Use systemMessage if present, otherwise fall back to reason.
        // This avoids duplicate notifications when both fields exist on block/deny.
        let msg = output.system_message.as_ref().or(output.reason.as_ref());
        if let Some(msg) = msg {
            notifications.push(HookNotification {
                hook_name: hook_name.to_string(),
                message: msg.clone(),
                decision: output.decision.clone(),
            });
        }
    }
}

/// Deep-merge two JSON values (b overwrites a for conflicting keys).
fn merge_json(a: Value, b: Value) -> Value {
    match (a, b) {
        (Value::Object(mut map_a), Value::Object(map_b)) => {
            for (k, v) in map_b {
                let merged = if let Some(existing) = map_a.remove(&k) {
                    merge_json(existing, v)
                } else {
                    v
                };
                map_a.insert(k, merged);
            }
            Value::Object(map_a)
        }
        (_, b) => b,
    }
}

/// Public re-export of merge_json for use in core.rs.
pub fn merge_json_pub(a: Value, b: Value) -> Value {
    merge_json(a, b)
}

// ─── Decision Aggregation Primitives ─────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookDecisionClass {
    Passthrough,
    Block,
    Ask,
    Allow,
    Invalid,
}

fn classify_hook_decision(raw: Option<&str>) -> HookDecisionClass {
    match raw {
        None | Some("") => HookDecisionClass::Passthrough,
        Some("block") | Some("deny") | Some("reject") => HookDecisionClass::Block,
        Some("ask") => HookDecisionClass::Ask,
        Some("approve") | Some("allow") => HookDecisionClass::Allow,
        Some(_) => HookDecisionClass::Invalid,
    }
}

fn fold_hook_failure(current: HookDecision, reason: &str) -> HookDecision {
    match current {
        HookDecision::HookFailure(_) => current,
        _ => HookDecision::HookFailure(format!("Hook failure: {reason}")),
    }
}

/// Fold a raw hook output decision string into the running `HookDecision`.
///
/// Priority (highest wins): Block > Ask > Allow > Passthrough.
/// "reject" is treated as equivalent to "block"/"deny" (used by Stop hooks).
fn fold_decision(current: HookDecision, raw: Option<&str>, reason: Option<String>) -> HookDecision {
    match classify_hook_decision(raw) {
        HookDecisionClass::Block => {
            // Preserve the first non-empty block reason; don't let a later
            // hook without a reason overwrite an existing detailed message.
            match (&current, &reason) {
                (HookDecision::HookFailure(_), _) => current,
                (HookDecision::Block(_), None) => current,
                _ => HookDecision::Block(reason.unwrap_or_else(|| "Blocked by hook".to_string())),
            }
        }
        HookDecisionClass::Ask => match current {
            HookDecision::Block(_) | HookDecision::HookFailure(_) => current,
            _ => HookDecision::Ask,
        },
        HookDecisionClass::Allow => match current {
            HookDecision::Passthrough => HookDecision::Allow,
            _ => current,
        },
        HookDecisionClass::Passthrough => current,
        HookDecisionClass::Invalid => fold_hook_failure(current, "returned invalid decision"),
    }
}

/// Extract `additionalContext` from `hook_specific_output` and append-merge
/// into the running accumulator.
fn fold_additional_context(current: &mut Option<String>, specific: &Option<Value>) {
    if let Some(ref specific) = specific {
        if let Some(ctx) = HookSystem::pick_additional_context(specific) {
            *current = Some(match current.take() {
                Some(existing) => format!("{existing}\n{ctx}"),
                None => ctx.to_string(),
            });
        }
    }
}

/// Read a BeforeModel hook's replacement tool array out of its
/// `hook_specific_output`. `llm_request.config.tools` is canonical;
/// `llm_request.tools` is accepted so hooks written against the older position
/// keep working during migration.
fn extract_updated_tools(specific: &Option<Value>) -> Option<&Value> {
    let llm_request = specific.as_ref()?.get("llm_request")?;
    llm_request
        .get("config")
        .and_then(|config| config.get("tools"))
        .or_else(|| llm_request.get("tools"))
}

/// Accept a hook's tool array only if it is the *same tool set* as the host
/// declared — same count, same names, same order. That confines hooks to
/// rewriting `description` and `parameters` (the schema-compression use case)
/// and keeps them from silently adding, dropping, or reordering tools, which
/// would change tool-selection semantics. Tool filtering belongs to a separate
/// protocol.
fn validate_updated_tools(
    original: &[ToolDeclaration],
    candidate: &Value,
) -> Result<Vec<ToolDeclaration>, String> {
    let Some(entries) = candidate.as_array() else {
        return Err("expected a JSON array of tool declarations".to_string());
    };
    if entries.len() != original.len() {
        return Err(format!(
            "expected {} tool declarations, got {}",
            original.len(),
            entries.len()
        ));
    }

    let mut updated = Vec::with_capacity(entries.len());
    for (entry, expected) in entries.iter().zip(original) {
        let tool: ToolDeclaration = serde_json::from_value(entry.clone())
            .map_err(|error| format!("invalid tool declaration: {error}"))?;
        if tool.name != expected.name {
            return Err(format!(
                "tool name changed: expected '{}', got '{}'",
                expected.name, tool.name
            ));
        }
        if !tool.parameters.is_object() {
            return Err(format!("tool '{}' parameters must be an object", tool.name));
        }
        updated.push(tool);
    }

    // The turn's compaction prefix estimate is computed from the original
    // declarations before this hook runs, so a rewrite that grows them would
    // make the runtime under-account the real request, skip an emergency
    // compaction it needed, and overflow the provider context. The 1024-token
    // prefix reserve cannot absorb unbounded growth. Requiring the rewrite to
    // be no larger keeps the estimate conservative and matches the contract
    // this protocol already advertises: compression, not expansion.
    let original_tokens = estimate_declaration_tokens(original);
    let updated_tokens = estimate_declaration_tokens(&updated);
    if updated_tokens > original_tokens {
        return Err(format!(
            "tool declarations grew from ~{original_tokens} to ~{updated_tokens} tokens; \
             BeforeModel may only compress declarations"
        ));
    }

    Ok(updated)
}

/// Estimates the declarations the same way the compaction prefix estimate does,
/// so the two cannot disagree about whether a rewrite fits.
fn estimate_declaration_tokens(tools: &[ToolDeclaration]) -> u64 {
    crate::compaction::estimate_text_tokens(&serde_json::to_string(tools).unwrap_or_default())
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_event_str() {
        assert_eq!(HookEventName::PreToolUse.as_str(), "PreToolUse");
        assert_eq!(HookEventName::PostToolUse.as_str(), "PostToolUse");
        assert_eq!(HookEventName::UserPromptSubmit.as_str(), "UserPromptSubmit");
        assert_eq!(HookEventName::SessionStart.as_str(), "SessionStart");
        assert_eq!(HookEventName::Stop.as_str(), "Stop");
    }

    #[test]
    fn parse_hook_output_block() {
        let json = r#"{"decision":"block","reason":"unsafe command"}"#;
        let out: HookOutput = serde_json::from_str(json).unwrap();
        assert_eq!(out.decision.as_deref(), Some("block"));
        assert_eq!(out.reason.as_deref(), Some("unsafe command"));
    }

    #[test]
    fn parse_hook_output_allow_with_patch() {
        let json =
            r#"{"decision":"allow","hook_specific_output":{"tool_input":{"safe_mode":true}}}"#;
        let out: HookOutput = serde_json::from_str(json).unwrap();
        assert_eq!(out.decision.as_deref(), Some("allow"));
        let patch = out.hook_specific_output.unwrap();
        assert_eq!(patch["tool_input"]["safe_mode"], true);
    }

    #[test]
    fn parse_hook_output_accepts_camel_case_aliases() {
        let json = r#"{"continue":false,"stopReason":"stop","suppressOutput":true,"systemMessage":"notice","hookSpecificOutput":{"additionalContext":"context"}}"#;
        let out = decode_hook_output(json.as_bytes()).unwrap();
        assert_eq!(out.should_continue, Some(false));
        assert_eq!(out.stop_reason.as_deref(), Some("stop"));
        assert_eq!(out.suppress_output, Some(true));
        assert_eq!(out.system_message.as_deref(), Some("notice"));
        assert_eq!(
            out.hook_specific_output.unwrap()["additionalContext"],
            "context"
        );
    }

    #[test]
    fn parse_hook_output_accepts_unknown_fields() {
        let json = r#"{"decision":"block","metadata":{"severity":"high"}}"#;
        let out = decode_hook_output(json.as_bytes()).unwrap();
        assert_eq!(out.decision.as_deref(), Some("block"));
    }

    #[test]
    fn parse_hook_output_rejects_null_decision() {
        let json = r#"{"decision":null}"#;
        assert!(decode_hook_output(json.as_bytes()).is_none());
    }

    #[test]
    fn hook_decision_classification_matches_protocol() {
        for (raw, expected) in [
            (None, HookDecisionClass::Passthrough),
            (Some(""), HookDecisionClass::Passthrough),
            (Some("block"), HookDecisionClass::Block),
            (Some("deny"), HookDecisionClass::Block),
            (Some("reject"), HookDecisionClass::Block),
            (Some("ask"), HookDecisionClass::Ask),
            (Some("approve"), HookDecisionClass::Allow),
            (Some("allow"), HookDecisionClass::Allow),
            (Some("blok"), HookDecisionClass::Invalid),
        ] {
            assert_eq!(classify_hook_decision(raw), expected, "{raw:?}");
        }
    }

    #[test]
    fn hook_fail_open_defaults_closed_and_accepts_explicit_true() {
        let default: HookDefinition = serde_json::from_str(r#"{"command":"true"}"#).unwrap();
        assert!(!default.fail_open);

        let explicit: HookDefinition =
            serde_json::from_str(r#"{"command":"true","fail_open":true}"#).unwrap();
        assert!(explicit.fail_open);
    }

    #[test]
    fn hook_output_is_redacted_before_aggregation() {
        let secret = "short-hook-secret";
        let output = HookOutput {
            stop_reason: Some(format!("token={secret}")),
            decision: Some("block".to_string()),
            reason: Some(format!("password={secret}")),
            system_message: Some(format!("Bearer {secret}")),
            hook_specific_output: Some(serde_json::json!({
                "additionalContext": format!("api_key={secret}"),
                "tool_input": {"token": secret}
            })),
            ..Default::default()
        };

        let output = HookSystem::redact_hook_output(output);
        let serialized = format!(
            "{} {} {} {}",
            output.stop_reason.as_deref().unwrap_or_default(),
            output.reason.as_deref().unwrap_or_default(),
            output.system_message.as_deref().unwrap_or_default(),
            output
                .hook_specific_output
                .as_ref()
                .map(Value::to_string)
                .unwrap_or_default()
        );

        assert!(!serialized.contains(secret), "{serialized}");
        assert!(serialized.contains("<redacted>"), "{serialized}");
    }

    #[test]
    fn merge_json_deep() {
        let a = serde_json::json!({"a": 1, "b": {"x": 10}});
        let b = serde_json::json!({"b": {"y": 20}, "c": 3});
        let merged = merge_json(a, b);
        assert_eq!(merged["a"], 1);
        assert_eq!(merged["b"]["x"], 10);
        assert_eq!(merged["b"]["y"], 20);
        assert_eq!(merged["c"], 3);
    }

    #[test]
    fn disabled_system_returns_passthrough() {
        let sys = HookSystem::new_disabled();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result =
            rt.block_on(sys.fire_pre_tool_use("s1", "/tmp", "tool-1", "shell", &Value::Null, None));
        assert_eq!(result.decision, HookDecision::Passthrough);
    }

    #[test]
    fn config_hook_events_default_to_enabled() {
        for event in [
            HookEventName::PreToolUse,
            HookEventName::PostToolUse,
            HookEventName::PostToolUseFailure,
            HookEventName::UserPromptSubmit,
            HookEventName::SessionStart,
            HookEventName::Stop,
            HookEventName::BeforeModel,
            HookEventName::AfterModel,
        ] {
            let config: HooksConfig = toml::from_str(&format!(
                "[[{}]]\nname = 'config-probe'\ncommand = 'true'\n",
                event.as_str()
            ))
            .unwrap();
            let system = HookSystem::from_config(&config);
            assert!(system.enabled, "{event:?}");
            assert_eq!(system.active_hooks(event).len(), 1);
        }
    }

    fn extension_pre_tool_hook(command: &str) -> crate::extension::ExtensionHooks {
        serde_json::from_value(serde_json::json!({
            "PreToolUse": [{
                "hooks": [{
                    "type": "command",
                    "name": "extension-probe",
                    "command": command
                }]
            }]
        }))
        .unwrap()
    }

    fn extension_post_tool_hook(command: &str) -> crate::extension::ExtensionHooks {
        serde_json::from_value(serde_json::json!({
            "PostToolUse": [{
                "hooks": [{
                    "type": "command",
                    "name": "extension-post-probe",
                    "command": command
                }]
            }]
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn extension_empty_output_is_passthrough() {
        let mut system = HookSystem::from_config(&HooksConfig::default());
        system.register_extension_hooks(&extension_pre_tool_hook("true"));
        assert!(system.enabled, "extension registration must enable hooks");

        let result = system
            .fire_pre_tool_use("s1", "/tmp", "tool-1", "shell", &Value::Null, None)
            .await;

        assert_eq!(result.decision, HookDecision::Passthrough);
        assert!(result.hook_failures.is_empty());
        assert!(result.notifications.is_empty());
    }

    #[tokio::test]
    async fn extension_errors_remain_fail_closed() {
        for (command, expected) in [
            ("printf 'not-json'", HookFailureKind::InvalidJson),
            ("printf '{\"decision\":null}'", HookFailureKind::InvalidJson),
            ("exit 7", HookFailureKind::NonZero),
        ] {
            let mut system = HookSystem::from_config(&HooksConfig::default());
            system.register_extension_hooks(&extension_pre_tool_hook(command));
            assert!(system.enabled, "extension registration must enable hooks");

            let result = system
                .fire_pre_tool_use("s1", "/tmp", "tool-1", "shell", &Value::Null, None)
                .await;

            assert!(matches!(result.decision, HookDecision::HookFailure(_)));
            assert_eq!(result.hook_failures.len(), 1);
            assert_eq!(result.hook_failures[0].kind, expected);
        }
    }

    #[tokio::test]
    async fn extension_output_truncated_returns_failure() {
        let mut system = HookSystem::from_config(&HooksConfig::default());
        // Produce just over 32 MiB on stdout so the per-pipe cap triggers
        // before the natural deadline.
        system.register_extension_hooks(&extension_pre_tool_hook("yes | head -c 33554433"));
        assert!(system.enabled, "extension registration must enable hooks");

        let result = system
            .fire_pre_tool_use("s1", "/tmp", "tool-1", "shell", &Value::Null, None)
            .await;

        assert!(matches!(result.decision, HookDecision::HookFailure(_)));
        assert_eq!(result.hook_failures.len(), 1);
        assert_eq!(
            result.hook_failures[0].kind,
            HookFailureKind::OutputTruncated
        );
        assert!(result.notifications[0]
            .message
            .contains("output exceeded the size limit"));
    }

    #[tokio::test]
    async fn explicit_hooks_disable_prevents_extension_auto_enable() {
        let config = HooksConfig {
            enabled: false,
            enabled_override: Some(false),
            ..Default::default()
        };
        let mut system = HookSystem::from_config(&config);
        system.register_extension_hooks(&extension_pre_tool_hook(
            "printf '{\"decision\":\"block\",\"reason\":\"must not run\"}'",
        ));
        assert!(!system.enabled, "explicit disable must prevent auto-enable");

        let result = system
            .fire_pre_tool_use("s1", "/tmp", "tool-1", "shell", &Value::Null, None)
            .await;

        assert_eq!(result.decision, HookDecision::Passthrough);
        assert!(result.hook_failures.is_empty());
        assert!(result.notifications.is_empty());
    }

    #[tokio::test]
    async fn config_hook_collision_remains_fail_closed() {
        let config = HooksConfig {
            enabled: true,
            pre_tool_use: vec![HookDefinition {
                command: "true".to_string(),
                name: Some("extension-probe".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut system = HookSystem::from_config(&config);
        system.register_extension_hooks(&extension_pre_tool_hook("true"));

        let result = system
            .fire_pre_tool_use("s1", "/tmp", "tool-1", "shell", &Value::Null, None)
            .await;

        assert!(matches!(result.decision, HookDecision::HookFailure(_)));
        assert_eq!(result.hook_failures.len(), 1);
        assert_eq!(result.hook_failures[0].kind, HookFailureKind::EmptyOutput);
    }

    #[tokio::test]
    async fn extension_post_tool_empty_output_has_no_failure() {
        let mut system = HookSystem::from_config(&HooksConfig::default());
        system.register_extension_hooks(&extension_post_tool_hook("true"));
        assert!(system.enabled, "extension registration must enable hooks");

        let definitions = system.active_hooks(HookEventName::PostToolUse);
        let input = system.build_input("s1", "/tmp", HookEventName::PostToolUse, Value::Null);
        let outputs = system.run_hooks(&definitions, &input).await;

        assert_eq!(outputs.len(), 1);
        assert!(outputs[0].1.failure.is_none());
    }

    #[test]
    fn matcher_regex_works() {
        let def = HookDefinition {
            command: "echo".to_string(),
            name: None,
            matcher: Some("run_shell.*".to_string()),
            timeout: None,
            sequential: None,
            fail_open: false,
            env: Default::default(),
        };
        assert!(HookSystem::matches_tool(&def, "run_shell_command"));
        assert!(!HookSystem::matches_tool(&def, "read_file"));
    }

    #[test]
    fn matcher_none_matches_all() {
        let def = HookDefinition {
            command: "echo".to_string(),
            name: None,
            matcher: None,
            timeout: None,
            sequential: None,
            fail_open: false,
            env: Default::default(),
        };
        assert!(HookSystem::matches_tool(&def, "any_tool"));
    }

    #[tokio::test]
    async fn fire_pre_tool_use_with_blocking_hook() {
        let config = HooksConfig {
            enabled: true,
            enabled_override: None,

            pre_tool_use: vec![HookDefinition {
                command: "echo '{\"decision\":\"block\",\"reason\":\"no rm allowed\"}'".to_string(),
                name: Some("block-rm".to_string()),
                matcher: Some("run_shell_command".to_string()),
                timeout: Some(5000),
                sequential: None,
                fail_open: false,
                env: Default::default(),
            }],
            post_tool_use: vec![],
            post_tool_use_failure: vec![],
            user_prompt_submit: vec![],
            session_start: vec![],
            stop: vec![],
            before_model: vec![],
            after_model: vec![],
        };
        let sys = HookSystem::from_config(&config);
        let result = sys
            .fire_pre_tool_use(
                "s1",
                "/tmp",
                "tool-1",
                "run_shell_command",
                &serde_json::json!({"command": "rm -rf /"}),
                None,
            )
            .await;
        assert_eq!(
            result.decision,
            HookDecision::Block("no rm allowed".to_string())
        );
        assert!(!result.notifications.is_empty());
    }

    #[tokio::test]
    async fn fire_pre_tool_use_no_match() {
        let config = HooksConfig {
            enabled: true,
            enabled_override: None,

            pre_tool_use: vec![HookDefinition {
                command: "echo '{\"decision\":\"block\",\"reason\":\"no\"}'".to_string(),
                name: None,
                matcher: Some("run_shell_command".to_string()),
                timeout: None,
                sequential: None,
                fail_open: false,
                env: Default::default(),
            }],
            post_tool_use: vec![],
            post_tool_use_failure: vec![],
            user_prompt_submit: vec![],
            session_start: vec![],
            stop: vec![],
            before_model: vec![],
            after_model: vec![],
        };
        let sys = HookSystem::from_config(&config);
        let result = sys
            .fire_pre_tool_use(
                "s1",
                "/tmp",
                "tool-1",
                "read_file",
                &serde_json::json!({}),
                None,
            )
            .await;
        assert_eq!(result.decision, HookDecision::Passthrough);
    }

    #[tokio::test]
    async fn user_prompt_hook_receives_redacted_input() {
        let secret = "sk-user-prompt-hook-secret";
        let config = HooksConfig {
            enabled: true,
            enabled_override: None,
            pre_tool_use: vec![],
            post_tool_use: vec![],
            post_tool_use_failure: vec![],
            user_prompt_submit: vec![HookDefinition {
                command: format!(
                    r#"python3 -c 'import json,sys; prompt=json.load(sys.stdin)["prompt"]; redacted="<redacted>" in prompt and "{secret}" not in prompt; print(json.dumps({{"decision":"allow" if redacted else "block"}}))'"#
                ),
                name: Some("prompt-redaction-probe".to_string()),
                matcher: None,
                timeout: Some(5000),
                sequential: None,
                fail_open: false,
                env: Default::default(),
            }],
            session_start: vec![],
            stop: vec![],
            before_model: vec![],
            after_model: vec![],
        };
        let system = HookSystem::from_config(&config);

        let result = system
            .fire_user_prompt_submit(
                "session-1",
                "/tmp",
                &format!("write api_key={secret} to the config"),
            )
            .await;

        assert_eq!(result.decision, HookDecision::Allow);
    }

    #[tokio::test]
    async fn exit_code_2_means_block() {
        let config = HooksConfig {
            enabled: true,
            enabled_override: None,

            pre_tool_use: vec![HookDefinition {
                command: "sh -c 'echo blocked: api_key=sk-exit2-secret >&2; exit 2'".to_string(),
                name: Some("exit2-hook".to_string()),
                matcher: None,
                timeout: Some(5000),
                sequential: None,
                fail_open: false,
                env: Default::default(),
            }],
            post_tool_use: vec![],
            post_tool_use_failure: vec![],
            user_prompt_submit: vec![],
            session_start: vec![],
            stop: vec![],
            before_model: vec![],
            after_model: vec![],
        };
        let sys = HookSystem::from_config(&config);
        let result = sys
            .fire_pre_tool_use("s1", "/tmp", "tool-1", "any", &serde_json::json!({}), None)
            .await;
        let HookDecision::Block(reason) = result.decision else {
            panic!("exit code 2 must block");
        };
        assert!(reason.starts_with("blocked:"), "{reason}");
        assert!(reason.contains("<redacted>"), "{reason}");
        assert!(!reason.contains("sk-exit2-secret"), "{reason}");
        assert!(result.hook_failures.is_empty());
    }

    async fn run_pre_hook(
        command: &str,
        timeout: Option<u64>,
        fail_open: bool,
    ) -> PreToolUseResult {
        let config = HooksConfig {
            enabled: true,
            pre_tool_use: vec![HookDefinition {
                command: command.to_string(),
                name: Some("failure-probe".to_string()),
                matcher: None,
                timeout,
                sequential: None,
                fail_open,
                env: Default::default(),
            }],
            ..Default::default()
        };
        HookSystem::from_config(&config)
            .fire_pre_tool_use("s1", "/tmp", "tool-1", "shell", &Value::Null, None)
            .await
    }

    #[tokio::test]
    async fn exit_code_2_without_stderr_uses_fallback_reason() {
        let result = run_pre_hook("sh -c 'exit 2'", Some(5000), false).await;
        assert_eq!(
            result.decision,
            HookDecision::Block("Blocked by hook (exit 2)".to_string())
        );
        assert!(result.hook_failures.is_empty());
    }

    #[tokio::test]
    async fn pre_tool_use_failures_block_without_leaking_output() {
        let cases = [
            ("exit 7", HookFailureKind::NonZero),
            ("printf 'not-json'", HookFailureKind::InvalidJson),
            ("printf '{\"decision\":null}'", HookFailureKind::InvalidJson),
            ("true", HookFailureKind::EmptyOutput),
            (
                "printf 'secret-output' >&2; exit 7",
                HookFailureKind::NonZero,
            ),
        ];
        for (command, kind) in cases {
            let result = run_pre_hook(command, Some(500), false).await;
            assert!(matches!(result.decision, HookDecision::HookFailure(_)));
            assert_eq!(result.hook_failures[0].kind, kind);
            assert!(!result.notifications[0].message.contains("secret-output"));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pre_tool_use_timeout_and_signal_fail_closed() {
        let timeout = run_pre_hook("sleep 2", Some(20), false).await;
        assert!(matches!(timeout.decision, HookDecision::HookFailure(_)));
        assert_eq!(timeout.hook_failures[0].kind, HookFailureKind::Timeout);

        let signal = run_pre_hook("kill -TERM $$", Some(500), false).await;
        assert!(matches!(signal.decision, HookDecision::HookFailure(_)));
        assert_eq!(signal.hook_failures[0].kind, HookFailureKind::Signaled);
    }

    #[tokio::test]
    async fn explicit_fail_open_records_failure_without_authorizing_it() {
        let result = run_pre_hook("printf 'secret-output'", Some(500), true).await;
        assert_eq!(result.decision, HookDecision::Passthrough);
        assert_eq!(result.hook_failures[0].kind, HookFailureKind::InvalidJson);
        assert_eq!(
            result.notifications[0].decision.as_deref(),
            Some("hook_failure")
        );
        assert!(!result.notifications[0].message.contains("secret-output"));
    }

    #[tokio::test]
    async fn valid_allow_has_no_hook_failure_metadata() {
        let result = run_pre_hook("printf '{\"decision\":\"allow\"}'", Some(500), false).await;
        assert_eq!(result.decision, HookDecision::Allow);
        assert!(result.hook_failures.is_empty());
    }

    #[test]
    fn all_hook_failure_kinds_fail_closed_without_detail_leakage() {
        let definition = RegisteredHook {
            definition: HookDefinition {
                command: "probe".to_string(),
                name: Some("failure-probe".to_string()),
                matcher: None,
                timeout: None,
                sequential: None,
                fail_open: false,
                env: Default::default(),
            },
            accepts_empty_output: false,
        };
        let definitions = [&definition];
        let system = HookSystem::new_disabled();

        for kind in [
            HookFailureKind::Spawn,
            HookFailureKind::Io,
            HookFailureKind::Timeout,
            HookFailureKind::NonZero,
            HookFailureKind::Signaled,
            HookFailureKind::InvalidJson,
            HookFailureKind::EmptyOutput,
            HookFailureKind::OutputTruncated,
        ] {
            let result = system.aggregate_pre_tool_use(
                vec![(
                    0,
                    HookExecution {
                        output: HookOutput::default(),
                        failure: Some(kind),
                    },
                )],
                &definitions,
            );
            assert!(matches!(result.decision, HookDecision::HookFailure(_)));
            assert_eq!(result.hook_failures[0].kind, kind);
            assert_eq!(
                result.notifications[0].message,
                format!("Hook failed: {}", kind.reason())
            );
        }
    }

    // ===== Task 1: matcher 双向工具名兼容 =====

    fn def_with_matcher(matcher: &str) -> HookDefinition {
        HookDefinition {
            command: "echo".to_string(),
            name: None,
            matcher: Some(matcher.to_string()),
            timeout: None,
            sequential: None,
            fail_open: false,
            env: Default::default(),
        }
    }

    #[test]
    fn matcher_matches_alias_run_shell_command() {
        // matcher 写 copilot-shell 名字，cosh-ng 内部名也能命中
        let def = def_with_matcher("^run_shell_command$");
        assert!(HookSystem::matches_tool(&def, "shell"));
        assert!(HookSystem::matches_tool(&def, "run_shell_command"));
    }

    #[test]
    fn matcher_matches_alias_shell() {
        // matcher 写 cosh-ng 名字，copilot-shell 名字也能命中
        let def = def_with_matcher("^shell$");
        assert!(HookSystem::matches_tool(&def, "shell"));
        assert!(HookSystem::matches_tool(&def, "run_shell_command"));
    }

    #[test]
    fn matcher_alias_grep_and_todo() {
        let def_grep = def_with_matcher("^grep_search$");
        assert!(HookSystem::matches_tool(&def_grep, "grep"));
        let def_todo = def_with_matcher("^todo_write$");
        assert!(HookSystem::matches_tool(&def_todo, "todo"));
    }

    #[test]
    fn matcher_unknown_tool_no_alias() {
        // 不在别名表的工具名走原路径
        let def = def_with_matcher("^read_file$");
        assert!(HookSystem::matches_tool(&def, "read_file"));
        assert!(!HookSystem::matches_tool(&def, "shell"));
    }

    // ===== Task 2: additionalContext 双向兼容 =====

    #[test]
    fn pick_additional_context_prefers_snake_case() {
        let v = serde_json::json!({
            "additional_context": "snake",
            "additionalContext": "camel"
        });
        assert_eq!(HookSystem::pick_additional_context(&v), Some("snake"));
    }

    #[test]
    fn pick_additional_context_falls_back_to_camel_case() {
        let v = serde_json::json!({"additionalContext": "only-camel"});
        assert_eq!(HookSystem::pick_additional_context(&v), Some("only-camel"));
    }

    #[test]
    fn pick_additional_context_returns_none_when_absent() {
        let v = serde_json::json!({"other": "x"});
        assert_eq!(HookSystem::pick_additional_context(&v), None);
    }

    // ===== BeforeModel tool declaration rewriting (#1616) =====

    fn shell_declaration() -> ToolDeclaration {
        ToolDeclaration {
            name: "shell".to_string(),
            description: "run a shell command".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"command": {"type": "string"}},
            }),
        }
    }

    fn updated_tools_output(tools: Value) -> Option<Value> {
        Some(serde_json::json!({"llm_request": {"config": {"tools": tools}}}))
    }

    #[test]
    fn extract_updated_tools_prefers_canonical_config_position() {
        let specific = Some(serde_json::json!({
            "llm_request": {
                "config": {"tools": [{"name": "canonical"}]},
                "tools": [{"name": "legacy"}],
            }
        }));
        let tools = extract_updated_tools(&specific).unwrap();
        assert_eq!(tools[0]["name"], "canonical");
    }

    #[test]
    fn extract_updated_tools_falls_back_to_legacy_position() {
        let specific = Some(serde_json::json!({
            "llm_request": {"tools": [{"name": "legacy"}]}
        }));
        let tools = extract_updated_tools(&specific).unwrap();
        assert_eq!(tools[0]["name"], "legacy");
    }

    #[test]
    fn validate_updated_tools_accepts_compressed_description_and_schema() {
        let original = vec![shell_declaration()];
        let candidate = serde_json::json!([{
            "name": "shell",
            "description": "run",
            "parameters": {"type": "object"},
        }]);

        let updated = validate_updated_tools(&original, &candidate).unwrap();

        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].description, "run");
    }

    #[test]
    fn validate_updated_tools_rejects_malformed_arrays() {
        let original = vec![shell_declaration()];
        for candidate in [
            // not an array
            serde_json::json!({"tools": []}),
            // missing required field
            serde_json::json!([{"name": "shell", "parameters": {"type": "object"}}]),
            // parameters is not a JSON object
            serde_json::json!([{"name": "shell", "description": "run", "parameters": "object"}]),
            // renamed tool
            serde_json::json!([{"name": "sh", "description": "run", "parameters": {}}]),
            // added tool
            serde_json::json!([
                {"name": "shell", "description": "run", "parameters": {}},
                {"name": "extra", "description": "x", "parameters": {}},
            ]),
            // dropped tool
            serde_json::json!([]),
        ] {
            assert!(
                validate_updated_tools(&original, &candidate).is_err(),
                "should reject {candidate}"
            );
        }
    }

    #[test]
    fn validate_updated_tools_rejects_expanded_declarations() {
        let original = vec![shell_declaration()];

        // Same tool set, but a description and schema far larger than the
        // original — the compaction prefix estimate was already fixed from the
        // originals, so accepting this would under-account the real request.
        let candidate = serde_json::json!([{
            "name": "shell",
            "description": "x".repeat(4096),
            "parameters": {
                "type": "object",
                "properties": {"command": {"type": "string", "description": "y".repeat(4096)}},
            },
        }]);

        let error = validate_updated_tools(&original, &candidate).unwrap_err();
        assert!(error.contains("may only compress"), "{error}");
    }

    #[test]
    fn validate_updated_tools_allows_unchanged_size() {
        let original = vec![shell_declaration()];
        let candidate = serde_json::to_value(&original).unwrap();

        assert!(validate_updated_tools(&original, &candidate).is_ok());
    }

    #[test]
    fn validate_updated_tools_rejects_reordered_tools() {
        let original = vec![
            shell_declaration(),
            ToolDeclaration {
                name: "read_file".to_string(),
                description: "read".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            },
        ];
        let candidate = serde_json::json!([
            {"name": "read_file", "description": "read", "parameters": {"type": "object"}},
            {"name": "shell", "description": "run", "parameters": {"type": "object"}},
        ]);

        assert!(validate_updated_tools(&original, &candidate).is_err());
    }

    #[tokio::test]
    async fn before_model_input_carries_full_tool_declarations() {
        let config = HooksConfig {
            enabled: true,
            before_model: vec![HookDefinition {
                command: r#"python3 -c 'import sys,json; t=json.load(sys.stdin)["llm_request"]["config"]["tools"]; print(json.dumps({"systemMessage": t[0]["name"]+"|"+t[0]["description"]+"|"+t[0]["parameters"]["properties"]["command"]["type"]}))'"#.to_string(),
                name: Some("inspect".to_string()),
                matcher: None,
                timeout: Some(10_000),
                sequential: None,
                fail_open: false,
                env: Default::default(),
            }],
            ..Default::default()
        };
        let sys = HookSystem::from_config(&config);

        let result = sys
            .fire_before_model("s1", "/tmp", "m", &[], &[shell_declaration()])
            .await;

        assert_eq!(
            result.notifications[0].message,
            "shell|run a shell command|string"
        );
        assert!(result.updated_tools.is_none());
    }

    #[tokio::test]
    async fn before_model_last_valid_tool_array_wins_over_later_invalid_one() {
        let config = HooksConfig {
            enabled: true,
            before_model: vec![
                HookDefinition {
                    command: r#"python3 -c 'print("""{"hookSpecificOutput":{"llm_request":{"config":{"tools":[{"name":"shell","description":"first","parameters":{"type":"object"}}]}}}}""")'"#.to_string(),
                    name: Some("first".to_string()),
                    matcher: None,
                    timeout: Some(10_000),
                    sequential: None,
                    fail_open: false,
                    env: Default::default(),
                },
                HookDefinition {
                    command: r#"python3 -c 'print("""{"hookSpecificOutput":{"llm_request":{"config":{"tools":[{"name":"renamed","description":"second","parameters":{"type":"object"}}]}}}}""")'"#.to_string(),
                    name: Some("second".to_string()),
                    matcher: None,
                    timeout: Some(10_000),
                    sequential: None,
                    fail_open: false,
                    env: Default::default(),
                },
            ],
            ..Default::default()
        };
        let sys = HookSystem::from_config(&config);

        let result = sys
            .fire_before_model("s1", "/tmp", "m", &[], &[shell_declaration()])
            .await;

        let updated = result.updated_tools.expect("first hook's array applies");
        assert_eq!(updated[0].description, "first");
    }

    #[test]
    fn hook_output_tool_schemas_survive_redaction() {
        let output = HookOutput {
            decision: None,
            reason: None,
            system_message: None,
            hook_specific_output: updated_tools_output(serde_json::json!([{
                "name": "shell",
                "description": "pass --token secret-in-description",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "api_key": {"type": "string"},
                        "token": {"type": "string"},
                    },
                },
            }])),
            ..Default::default()
        };

        let output = HookSystem::redact_hook_output(output);
        let tools = extract_updated_tools(&output.hook_specific_output).unwrap();

        assert_eq!(
            tools[0]["parameters"]["properties"]["api_key"]["type"],
            "string"
        );
        assert_eq!(
            tools[0]["parameters"]["properties"]["token"]["type"],
            "string"
        );
        // String leaves inside the exempt subtree still lose secret shapes.
        assert_eq!(tools[0]["description"], "pass --token <redacted>");
    }

    #[test]
    fn hook_output_outside_tool_schemas_is_still_redacted() {
        let output = HookOutput {
            decision: None,
            reason: None,
            system_message: None,
            hook_specific_output: Some(serde_json::json!({
                "api_key": "leaked-value",
            })),
            ..Default::default()
        };

        let output = HookSystem::redact_hook_output(output);
        let specific = output.hook_specific_output.unwrap();

        assert_eq!(specific["api_key"], "<redacted>");
    }

    // ===== Task 3: tool_response 包装 =====

    #[test]
    fn wrap_tool_response_plain_text() {
        let v = HookSystem::wrap_tool_response("hello world");
        assert_eq!(v["llmContent"], "hello world");
        assert_eq!(v["returnDisplay"], "hello world");
    }

    #[test]
    fn wrap_tool_response_passes_through_object() {
        // 对齐 copilot-shell 行为：即使原始文本是 JSON，仍作为文本包装进 llmContent。
        let raw = r#"{"llmContent":"x","returnDisplay":"y"}"#;
        let v = HookSystem::wrap_tool_response(raw);
        assert_eq!(v["llmContent"], raw);
        assert_eq!(v["returnDisplay"], raw);
    }

    #[test]
    fn wrap_tool_response_wraps_array_as_text() {
        // 对齐 copilot-shell：数组也作为文本包装，而非透传。
        let raw = r#"[1,2,3]"#;
        let v = HookSystem::wrap_tool_response(raw);
        assert_eq!(v["llmContent"], raw);
        assert_eq!(v["returnDisplay"], raw);
    }

    #[test]
    fn wrap_tool_response_bare_number_is_wrapped() {
        // 裸数字虽是合法 JSON 但不是 object/array，仍需要包装。
        let v = HookSystem::wrap_tool_response("42");
        assert_eq!(v["llmContent"], "42");
    }

    // ===== Task 4: HookInput / event_data 新字段 =====

    #[test]
    fn hook_input_contains_transcript_path() {
        let sys = HookSystem::new_disabled();
        let input = sys.build_input(
            "sess-1",
            "/work",
            HookEventName::PreToolUse,
            serde_json::json!({"tool_name": "shell"}),
        );
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["transcript_path"], "/work/.cosh-transcript.jsonl");
        assert_eq!(json["session_id"], "sess-1");
        assert_eq!(json["hook_event_name"], "PreToolUse");
    }

    #[tokio::test]
    async fn event_data_contains_tool_use_id_and_keeps_native_tool_name() {
        // 调用方传 cosh-ng 原名 shell + matcher 写 run_shell_command。
        // hook 脚本输出使用输入中的 tool_name 与 tool_use_id 作为上下文验证。
        let config = HooksConfig {
            enabled: true,
            enabled_override: None,

            pre_tool_use: vec![],
            post_tool_use: vec![HookDefinition {
                command: r#"python3 -c 'import sys,json; d=json.load(sys.stdin); print(json.dumps({"hook_specific_output": {"additionalContext": d["tool_name"]+"|"+d["tool_use_id"]}}))'"#.to_string(),
                name: Some("echo".to_string()),
                matcher: Some("^run_shell_command$".to_string()),
                timeout: Some(5000),
                sequential: None,
                fail_open: false,
                env: Default::default(),
            }],
            post_tool_use_failure: vec![],
            user_prompt_submit: vec![],
            session_start: vec![],
            stop: vec![],
            before_model: vec![],
            after_model: vec![],
        };
        let sys = HookSystem::from_config(&config);
        let result = sys
            .fire_post_tool_use(
                "s1",
                "/tmp",
                "call-42",
                "shell",
                &serde_json::json!({"command": "ls"}),
                "hello",
                None,
            )
            .await;
        // additional_context 里会包含传入的 cosh-ng 原名 shell 与 tool_use_id call-42
        let ctx = result.additional_context.unwrap();
        assert!(ctx.contains("shell"), "ctx={ctx}");
        assert!(ctx.contains("call-42"), "ctx={ctx}");
    }

    #[tokio::test]
    async fn event_data_includes_skill_context_when_provided() {
        // hook 脚本反射 skill_context.file_path 到 additionalContext 验证透传。
        // PreToolUse 不会在 additional_context 里体现 hook 输出，改用
        // PostToolUse 路径验证（不同处理器但同样读 skill_context）。
        let config = HooksConfig {
            enabled: true,
            enabled_override: None,

            pre_tool_use: vec![],
            post_tool_use: vec![HookDefinition {
                command: r#"python3 -c 'import sys,json; d=json.load(sys.stdin); ctx=d.get("skill_context",{}); print(json.dumps({"hook_specific_output":{"additionalContext": ctx.get("file_path","none")}}))'"#.to_string(),
                name: Some("skill-probe".to_string()),
                matcher: Some("skill".to_string()),
                timeout: Some(5000),
                sequential: None,
                fail_open: false,
                env: Default::default(),
            }],
            post_tool_use_failure: vec![],
            user_prompt_submit: vec![],
            session_start: vec![],
            stop: vec![],
            before_model: vec![],
            after_model: vec![],
        };
        let sys = HookSystem::from_config(&config);
        let skill_ctx = serde_json::json!({
            "skill_name": "demo",
            "file_path": "/skills/demo/SKILL.md",
        });
        let result = sys
            .fire_post_tool_use(
                "s1",
                "/tmp",
                "call-1",
                "skill",
                &serde_json::json!({"action": "invoke", "name": "demo"}),
                "",
                Some(&skill_ctx),
            )
            .await;
        assert_eq!(
            result.additional_context.as_deref(),
            Some("/skills/demo/SKILL.md"),
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hook_timeout_kills_process_group() {
        use crate::process::test_support::*;

        let _fixture_guard = exclusive_process_tree_test().await;
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        let pid_file = dir.path().join("pids");
        let script = leak_script(&marker, &pid_file);

        let out = HookSystem::run_hook_cmd(
            &script,
            &BTreeMap::new(),
            "{}",
            Duration::from_millis(300),
            false,
        )
        .await;
        assert!(
            out.output.decision.is_none(),
            "timed-out hook must fall back to the default output"
        );

        let pids = read_pids(&pid_file);
        let _cleanup = PidCleanup(pids.clone());
        for pid in &pids {
            assert_process_gone(*pid);
        }
        release_marker_probe(&marker);
        assert!(!marker.exists(), "grandchild survived the hook timeout");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hook_ignoring_stdin_respects_deadline() {
        // Payload larger than the pipe buffer: the old implementation
        // blocked in the stdin write before the timeout even started.
        let payload = "x".repeat(1 << 20);
        let started = std::time::Instant::now();
        let out = HookSystem::run_hook_cmd(
            "sleep 30",
            &BTreeMap::new(),
            &payload,
            Duration::from_millis(300),
            false,
        )
        .await;
        assert!(out.output.decision.is_none());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "stdin write must be bounded by the hook deadline"
        );
    }

    // ─── #1617: hook env tests ───────────────────────────────────────

    // Runs a hook that reports `$probe`, `$COSH_RUNTIME` and
    // `$COSH_NG_VERSION` as seen by the child, joined with `|`.
    async fn probe_env(probe: &str, env: BTreeMap<String, String>) -> Vec<String> {
        let config = HooksConfig {
            enabled: true,
            session_start: vec![HookDefinition {
                command: format!(
                    r#"python3 -c 'import json,os; print(json.dumps({{"systemMessage": "|".join(os.environ.get(name,"<unset>") for name in ["{probe}","COSH_RUNTIME","COSH_NG_VERSION"])}}))'"#
                ),
                name: Some("probe".to_string()),
                timeout: Some(10_000),
                env,
                ..Default::default()
            }],
            ..Default::default()
        };
        let sys = HookSystem::from_config(&config);
        let result = sys.fire_session_start("s1", "/tmp").await;
        result
            .notifications
            .first()
            .expect("hook system message")
            .message
            .split('|')
            .map(str::to_string)
            .collect()
    }

    #[tokio::test]
    async fn hook_env_overrides_inherited_value_without_touching_the_parent() {
        // HOME is inherited from the parent; the hook shadows it for its own
        // child process only.
        let inherited = std::env::var("HOME").expect("HOME is set in the test environment");
        assert_ne!(inherited, "from-hook");

        let baseline = probe_env("HOME", BTreeMap::new()).await;
        assert_eq!(baseline[0], inherited, "child must inherit the parent env");

        let fields = probe_env(
            "HOME",
            BTreeMap::from([("HOME".to_string(), "from-hook".to_string())]),
        )
        .await;

        assert_eq!(fields[0], "from-hook");
        // Host-owned attribution reaches the child.
        assert_eq!(fields[1], "cosh-ng");
        assert_eq!(fields[2], env!("CARGO_PKG_VERSION"));
        // The host process itself is never mutated — no std::env::set_var.
        assert_eq!(std::env::var("HOME").unwrap(), inherited);
    }

    // Covers precedence only. The host values are a cooperative signal, not a
    // security boundary — the manifest also owns `command` and can reassign
    // them inside its own shell — so this proves nothing stronger than that a
    // declared `env` entry loses to the host's.
    #[tokio::test]
    async fn host_attribution_overrides_declared_hook_env() {
        let fields = probe_env(
            "COSH_RUNTIME",
            BTreeMap::from([
                ("COSH_RUNTIME".to_string(), "copilot-shell".to_string()),
                ("COSH_NG_VERSION".to_string(), "99.99.99".to_string()),
            ]),
        )
        .await;

        assert_eq!(fields[1], "cosh-ng");
        assert_eq!(fields[2], env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn hook_env_with_invalid_name_is_dropped_and_hook_still_runs() {
        let fields = probe_env(
            "HOME",
            BTreeMap::from([
                ("HOME".to_string(), "kept".to_string()),
                ("1INVALID".to_string(), "dropped".to_string()),
                ("WITH-DASH".to_string(), "dropped".to_string()),
            ]),
        )
        .await;

        assert_eq!(fields[0], "kept");
    }

    #[test]
    fn env_name_validation_follows_posix_rules() {
        for name in ["PATH", "_UNDERSCORE", "A1", "_"] {
            assert!(crate::config::is_valid_env_name(name), "{name}");
        }
        for name in ["", "1LEADING", "WITH-DASH", "WITH SPACE", "a=b", "ä"] {
            assert!(!crate::config::is_valid_env_name(name), "{name}");
        }
    }

    // ─── #1614: updated_tool_response tests ──────────────────────────

    #[test]
    fn pick_updated_tool_response_snake_case() {
        let specific = serde_json::json!({
            "updated_tool_response": "compressed content"
        });
        assert_eq!(
            HookSystem::pick_updated_tool_response(&specific),
            Some("compressed content")
        );
    }

    #[test]
    fn pick_updated_tool_response_camel_case() {
        let specific = serde_json::json!({
            "updatedToolResponse": "compressed content"
        });
        assert_eq!(
            HookSystem::pick_updated_tool_response(&specific),
            Some("compressed content")
        );
    }

    #[test]
    fn pick_updated_tool_response_absent() {
        let specific = serde_json::json!({
            "additionalContext": "some context"
        });
        assert_eq!(HookSystem::pick_updated_tool_response(&specific), None);
    }

    #[test]
    fn pick_updated_tool_response_empty_string() {
        let specific = serde_json::json!({
            "updatedToolResponse": ""
        });
        assert_eq!(HookSystem::pick_updated_tool_response(&specific), None);
    }

    #[test]
    fn pick_updated_tool_response_non_string() {
        let specific = serde_json::json!({
            "updatedToolResponse": 42
        });
        assert_eq!(HookSystem::pick_updated_tool_response(&specific), None);
    }

    #[test]
    fn pick_updated_tool_response_snake_case_priority() {
        // When both are present, snake_case wins (cosh-ng convention).
        let specific = serde_json::json!({
            "updated_tool_response": "snake value",
            "updatedToolResponse": "camel value"
        });
        assert_eq!(
            HookSystem::pick_updated_tool_response(&specific),
            Some("snake value")
        );
    }

    #[tokio::test]
    async fn post_tool_use_hook_emits_updated_tool_response() {
        let config = HooksConfig {
            enabled: true,
            enabled_override: None,
            pre_tool_use: vec![],
            post_tool_use: vec![HookDefinition {
                command: r#"python3 -c 'import sys,json; print(json.dumps({"hook_specific_output": {"updatedToolResponse": "compressed!", "additionalContext": "env-hint"}}))'"#.to_string(),
                name: Some("compressor".to_string()),
                matcher: None,
                timeout: Some(5000),
                sequential: None,
                fail_open: false,
                env: Default::default(),
            }],
            post_tool_use_failure: vec![],
            user_prompt_submit: vec![],
            session_start: vec![],
            stop: vec![],
            before_model: vec![],
            after_model: vec![],
        };
        let sys = HookSystem::from_config(&config);
        let result = sys
            .fire_post_tool_use(
                "s1",
                "/tmp",
                "call-1",
                "shell",
                &serde_json::json!({"command": "ls"}),
                "original output here",
                None,
            )
            .await;
        assert_eq!(
            result.updated_tool_response.as_deref(),
            Some("compressed!"),
            "Hook should emit updatedToolResponse"
        );
        assert_eq!(
            result.additional_context.as_deref(),
            Some("env-hint"),
            "Hook should still emit additionalContext for attribution"
        );
    }

    #[tokio::test]
    async fn post_tool_use_last_replacement_wins() {
        // Two hooks both emit updatedToolResponse; last one in config order wins.
        let config = HooksConfig {
            enabled: true,
            enabled_override: None,
            pre_tool_use: vec![],
            post_tool_use: vec![
                HookDefinition {
                    command: r#"python3 -c 'import sys,json; print(json.dumps({"hook_specific_output": {"updatedToolResponse": "first"}}))'"#.to_string(),
                    name: Some("first-hook".to_string()),
                    matcher: None,
                    timeout: Some(5000),
                    sequential: None,
                    fail_open: false,
                    env: Default::default(),
                },
                HookDefinition {
                    command: r#"python3 -c 'import sys,json; print(json.dumps({"hook_specific_output": {"updatedToolResponse": "second"}}))'"#.to_string(),
                    name: Some("second-hook".to_string()),
                    matcher: None,
                    timeout: Some(5000),
                    sequential: None,
                    fail_open: false,
                    env: Default::default(),
                },
            ],
            post_tool_use_failure: vec![],
            user_prompt_submit: vec![],
            session_start: vec![],
            stop: vec![],
            before_model: vec![],
            after_model: vec![],
        };
        let sys = HookSystem::from_config(&config);
        let result = sys
            .fire_post_tool_use(
                "s1",
                "/tmp",
                "call-1",
                "shell",
                &serde_json::json!({"command": "ls"}),
                "original",
                None,
            )
            .await;
        assert_eq!(
            result.updated_tool_response.as_deref(),
            Some("second"),
            "Last valid replacement should win in configuration order"
        );
    }

    #[tokio::test]
    async fn post_tool_use_no_replacement_when_absent() {
        let config = HooksConfig {
            enabled: true,
            enabled_override: None,
            pre_tool_use: vec![],
            post_tool_use: vec![HookDefinition {
                command: r#"python3 -c 'import sys,json; print(json.dumps({"hook_specific_output": {"additionalContext": "just context"}}))'"#.to_string(),
                name: Some("context-only".to_string()),
                matcher: None,
                timeout: Some(5000),
                sequential: None,
                fail_open: false,
                env: Default::default(),
            }],
            post_tool_use_failure: vec![],
            user_prompt_submit: vec![],
            session_start: vec![],
            stop: vec![],
            before_model: vec![],
            after_model: vec![],
        };
        let sys = HookSystem::from_config(&config);
        let result = sys
            .fire_post_tool_use(
                "s1",
                "/tmp",
                "call-1",
                "shell",
                &serde_json::json!({"command": "ls"}),
                "original output",
                None,
            )
            .await;
        assert_eq!(
            result.updated_tool_response, None,
            "No replacement when hook doesn't emit updatedToolResponse"
        );
        assert_eq!(result.additional_context.as_deref(), Some("just context"),);
    }
}

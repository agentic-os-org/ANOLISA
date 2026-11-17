// Copyright 2026 Alibaba Cloud
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! 生产 Runtime 入口：静态样本直接走 PostTool；命令样本先经 PreTool（真实
//! `rtk rewrite` 判定），按改写结果执行，再走 PostTool。三种模式分别对应生产
//! 默认（内存 Stash）、关闭 Stash 与打开 diff 压缩；结果携带处置、恢复形态、
//! 实际生效的算子与可检索的 Stash 原文，供逐行解释与带检索的语义测试使用。
//!
//! 模型可见内容是 `output` 加上 `additional_context`（失败命令的诊断提示），
//! 两者都计入压缩后 Token，避免把附加给模型的文本漏算成节省。
//!
//! Latency basis: **production entry** — in-process PreTool + PostTool time,
//! plus the rtk wrapped-minus-raw wall clock when PreTool rewrote the command.

use std::path::Path;
use std::process::{Command, ExitStatus};
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use serde_json::{Value, json};
use tokenless_ccr::{InMemoryStore, StashStore};
use tokenless_protocol::{
    Attribution, ContentOrigin, OutputOptimization, PostToolCapabilities, PostToolRequest,
    PostToolResponse, PreToolAction, PreToolCapabilities, PreToolRequest, RecoveryMethod, Request,
    RequestEnvelope, Response, ResultKind, ToolResultStatus,
};
use tokenless_runtime::{EntryOptions, RuntimeConfig, dispatch_with_store};

use crate::l2::rtk_side::merge_streams;
use crate::l2::{Category, L2Error};

/// Latency-basis label stamped on every production-entry row.
pub const LATENCY_BASIS: &str = "production entry: runtime in-process + rtk wrapped-minus-raw";

const AGENT_ID: &str = "l2-production-harness";

/// Runtime configuration under test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// 固定 Runtime 默认配置：内存 Stash、HTML/diff/path sharing 开启。
    Default,
    /// The `--no-stash` contract: no recovery store, so lossy candidates are rejected.
    NoStash,
    /// 显式启用 diff；当前与 Default 等价，产物须标记该等价性。
    DiffOptIn,
}

impl Mode {
    /// Every mode, in report order.
    pub const ALL: [Mode; 3] = [Mode::Default, Mode::NoStash, Mode::DiffOptIn];

    /// Stable CLI/report name.
    pub fn name(self) -> &'static str {
        match self {
            Mode::Default => "default",
            Mode::NoStash => "no-stash",
            Mode::DiffOptIn => "diff-opt-in",
        }
    }

    /// Parses a `--mode` value.
    pub fn parse(value: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|mode| mode.name() == value)
    }

    /// 恢复存储类型直接由模式决定，报告层不再解析模式字符串。
    pub fn recovery_storage(self) -> &'static str {
        if self.stash() {
            "in_memory"
        } else {
            "disabled"
        }
    }

    /// 记录实际 EntryOptions；路径仅写入私有运行产物。
    pub fn resolved_options(self, rtk: Option<(&Path, &Path)>) -> Value {
        let options = self.options(rtk);
        json!({
            "compression_enabled": options.compression_enabled,
            "search_path_sharing_enabled": options.search_path_sharing_enabled,
            "diff_compression_enabled": options.diff_compression_enabled,
            "html_extraction_enabled": options.html_extraction_enabled,
            "mixed_content_enabled": options.mixed_content_enabled,
            "stash_enabled": options.stash_enabled,
            "rtk_launcher": options.rtk_launcher,
            "rtk_data_dir": options.rtk_data_dir,
        })
    }

    /// 固定默认值忽略外部开关时，将环境差异明确写入降级记录。
    pub fn configuration_notes(self) -> Vec<String> {
        let mut notes = Vec::new();
        if self == Mode::DiffOptIn {
            notes.push("diff-opt-in resolves to the same Runtime flags as default; not an independent configuration contrast".to_owned());
        }
        for name in [
            "TOKENLESS_DIFF_COMPRESSION_ENABLED",
            "TOKENLESS_HTML_EXTRACTION_ENABLED",
            "TOKENLESS_MIXED_CONTENT_ENABLED",
            "TOKENLESS_SEARCH_PATH_SHARING_ENABLED",
            "TOKENLESS_COMPRESSION_ENABLED",
        ] {
            if std::env::var_os(name).is_some() {
                notes.push(format!("{name} is ignored: this harness measures pinned Runtime defaults; see resolved_entry_options"));
            }
        }
        notes
    }

    fn stash(self) -> bool {
        !matches!(self, Mode::NoStash)
    }

    fn recovery(self) -> RecoveryMethod {
        if self.stash() {
            RecoveryMethod::Shell
        } else {
            RecoveryMethod::None
        }
    }

    fn options(self, rtk: Option<(&Path, &Path)>) -> EntryOptions {
        let default = RuntimeConfig::default();
        EntryOptions {
            compression_enabled: default.compression_enabled,
            search_path_sharing_enabled: default.search_path_sharing_enabled,
            diff_compression_enabled: default.diff_compression_enabled || self == Mode::DiffOptIn,
            html_extraction_enabled: default.html_extraction_enabled,
            mixed_content_enabled: default.mixed_content_enabled,
            stash_enabled: self.stash(),
            rtk_launcher: rtk.map(|(bin, _)| bin.to_path_buf()),
            rtk_data_dir: rtk.map(|(_, dir)| dir.to_path_buf()),
        }
    }
}

/// One Stash entry referenced by the emitted output, with the bytes a retrieve returns.
#[derive(Debug, Clone, Serialize)]
pub struct StashEntry {
    pub key: String,
    pub original: String,
}

/// What the production entry did to one payload — the per-row explanation.
#[derive(Debug, Clone, Serialize)]
pub struct EntryEvidence {
    pub mode: Mode,
    pub tool_name: String,
    pub content_origin: ContentOrigin,
    /// Command line the host reported for a command result.
    pub command: Option<String>,
    /// PreTool decision; `None` for static payloads that never ran a command.
    pub pre_tool_action: Option<PreToolAction>,
    /// Command actually executed after PreTool.
    pub executed_command: Option<String>,
    pub tool_status: ToolResultStatus,
    pub output_optimization: OutputOptimization,
    /// PostTool response without the (duplicated) output text.
    pub post_tool: Value,
    /// Diagnostic text the adapter adds to the model context, when any.
    pub additional_context: Option<String>,
    pub stash: Vec<StashEntry>,
    /// In-process PreTool + PostTool seconds.
    pub runtime_s: f64,
    /// Rewritten-minus-raw wall clock when PreTool rewrote the command.
    pub rtk_overhead_s: Option<f64>,
}

/// One payload through the production entry.
#[derive(Debug, Clone)]
pub struct EntryRun {
    /// Everything the model receives: output plus any additional context.
    pub model_visible: String,
    /// Production latency on [`LATENCY_BASIS`].
    pub latency_s: f64,
    pub evidence: EntryEvidence,
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl EntryRun {
    /// Whether the entry rejected a lossy candidate for lack of recovery.
    pub fn rejected_unrecoverable(&self) -> bool {
        self.evidence
            .post_tool
            .get("disposition")
            .and_then(Value::as_str)
            == Some("recoverability_unavailable")
    }
}

/// Raw execution of one command: the unfiltered baseline.
#[derive(Debug, Clone)]
pub struct RawCapture {
    pub text: String,
    pub wall_s: f64,
    pub success: bool,
}

/// Tool context for static payloads, mirroring the L3 overlay assets so both
/// layers dispatch the same content the same way.
fn static_context(category: Category) -> (&'static str, ContentOrigin, Option<&'static str>) {
    match category {
        Category::Json => ("fixture_api", ContentOrigin::ApiResponse, None),
        Category::Code => ("Read", ContentOrigin::FileContent, None),
        Category::BuildLog => ("exec", ContentOrigin::CommandOutput, Some("cargo check")),
        Category::Tabular => ("get_data", ContentOrigin::ApiResponse, None),
        Category::Search => ("Grep", ContentOrigin::ApiResponse, None),
        Category::Html => ("web_fetch", ContentOrigin::ApiResponse, None),
        Category::Command | Category::Grep | Category::Diff => {
            ("Bash", ContentOrigin::CommandOutput, None)
        }
    }
}

/// Runs a static payload through production PostTool under `mode`.
///
/// # Errors
/// Runtime failures and protocol-shape mismatches; Stash references that cannot
/// be retrieved are errors rather than silent losses.
pub fn post_tool_static(
    category: Category,
    content: &str,
    mode: Mode,
) -> Result<EntryRun, L2Error> {
    let (tool, origin, command) = static_context(category);
    let store: Arc<dyn StashStore> = Arc::new(InMemoryStore::new());
    let options = mode.options(None);
    let attribution = Attribution::new(AGENT_ID);
    let start = Instant::now();
    let response = post_tool(
        &PostToolInput {
            tool,
            content,
            origin,
            command,
            status: ToolResultStatus::Success,
            optimization: OutputOptimization::None,
        },
        mode,
        &options,
        &store,
        &attribution,
    )?;
    let runtime_s = start.elapsed().as_secs_f64();
    finish(
        response,
        Finish {
            mode,
            tool,
            origin,
            command: command.map(str::to_owned),
            pre_tool_action: None,
            executed_command: None,
            status: ToolResultStatus::Success,
            optimization: OutputOptimization::None,
            runtime_s,
            rtk_overhead_s: None,
        },
        &store,
    )
}

/// Runs `command` through PreTool → execution → PostTool, as a host with the
/// tokenless hooks installed would.
///
/// `raw` is the unfiltered execution of the same command. When PreTool keeps
/// the command native, that output is exactly what the host would run and
/// receive, so it is reused rather than executing (and timing) it twice.
///
/// # Errors
/// PreTool/PostTool failures, an unexpected PreTool shape, or a rewritten
/// command that cannot be spawned.
pub fn run_command(
    command: &str,
    raw: &RawCapture,
    cwd: &Path,
    rtk: (&Path, &Path),
    mode: Mode,
    call_id: &str,
) -> Result<EntryRun, L2Error> {
    let store: Arc<dyn StashStore> = Arc::new(InMemoryStore::new());
    let options = mode.options(Some(rtk));
    let attribution = Attribution {
        agent_id: AGENT_ID.to_owned(),
        session_id: Some("l2-session".to_owned()),
        tool_use_id: Some(call_id.to_owned()),
    };
    let start = Instant::now();
    let pre = dispatch_with_store(
        &RequestEnvelope {
            attribution: attribution.clone(),
            request: Request::PreTool(PreToolRequest {
                tool_name: "Bash".to_owned(),
                arguments: json!({ "command": command }),
                command_field: "command".to_owned(),
                capabilities: PreToolCapabilities {
                    replace_arguments: true,
                    block_and_suggest: false,
                },
            }),
        },
        &options,
        Some(&store),
        None,
    )
    .map_err(|error| L2Error::Command(format!("PreTool failed for {command:?}: {error}")))?;
    let mut runtime_s = start.elapsed().as_secs_f64();
    let Response::PreTool(pre) = pre.response.response else {
        return Err(L2Error::Protocol(
            "PreTool returned a different operation".into(),
        ));
    };

    let (executed, text, success, rtk_overhead_s) = if pre.action == PreToolAction::Passthrough {
        (command.to_owned(), raw.text.clone(), raw.success, None)
    } else {
        let rewritten = pre
            .arguments
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| L2Error::Protocol("PreTool rewrite has no command".into()))?
            .to_owned();
        let started = Instant::now();
        let out = Command::new("sh")
            .arg("-c")
            .arg(&rewritten)
            .current_dir(cwd)
            .output()
            .map_err(|error| L2Error::Command(format!("spawn rewritten {rewritten:?}: {error}")))?;
        let wall = started.elapsed().as_secs_f64();
        (
            rewritten,
            merge_streams(&out.stdout, &out.stderr),
            exit_ok(out.status),
            Some((wall - raw.wall_s).max(0.0)),
        )
    };
    // The production hook adapter reports a non-zero shell exit as an error
    // result (`compress_response_hook.py`), which Core never replaces.
    let status = if success {
        ToolResultStatus::Success
    } else {
        ToolResultStatus::Error
    };
    let start = Instant::now();
    let response = post_tool(
        &PostToolInput {
            tool: "Bash",
            content: &text,
            origin: ContentOrigin::CommandOutput,
            command: Some(&executed),
            status,
            optimization: pre.output_optimization,
        },
        mode,
        &options,
        &store,
        &attribution,
    )?;
    runtime_s += start.elapsed().as_secs_f64();
    finish(
        response,
        Finish {
            mode,
            tool: "Bash",
            origin: ContentOrigin::CommandOutput,
            command: Some(command.to_owned()),
            pre_tool_action: Some(pre.action),
            executed_command: Some(executed),
            status,
            optimization: pre.output_optimization,
            runtime_s,
            rtk_overhead_s,
        },
        &store,
    )
}

fn exit_ok(status: ExitStatus) -> bool {
    status.success()
}

struct PostToolInput<'a> {
    tool: &'a str,
    content: &'a str,
    origin: ContentOrigin,
    command: Option<&'a str>,
    status: ToolResultStatus,
    optimization: OutputOptimization,
}

fn post_tool(
    input: &PostToolInput<'_>,
    mode: Mode,
    options: &EntryOptions,
    store: &Arc<dyn StashStore>,
    attribution: &Attribution,
) -> Result<PostToolResponse, L2Error> {
    let request = PostToolRequest {
        result_kind: ResultKind::Tool,
        tool_name: input.tool.to_owned(),
        content: input.content.to_owned(),
        status: input.status,
        content_origin: input.origin,
        command: input.command.map(str::to_owned),
        output_optimization: input.optimization,
        capabilities: PostToolCapabilities {
            replace_output: true,
            replace_with_text: true,
            recovery: mode.recovery(),
        },
    };
    let outcome = dispatch_with_store(
        &RequestEnvelope {
            attribution: attribution.clone(),
            request: Request::PostTool(request),
        },
        options,
        mode.stash().then_some(store),
        None,
    )
    .map_err(|error| L2Error::Command(format!("PostTool failed: {error}")))?;
    match outcome.response.response {
        Response::PostTool(response) => Ok(response),
        _ => Err(L2Error::Protocol(
            "PostTool returned a different operation".into(),
        )),
    }
}

struct Finish {
    mode: Mode,
    tool: &'static str,
    origin: ContentOrigin,
    command: Option<String>,
    pre_tool_action: Option<PreToolAction>,
    executed_command: Option<String>,
    status: ToolResultStatus,
    optimization: OutputOptimization,
    runtime_s: f64,
    rtk_overhead_s: Option<f64>,
}

fn finish(
    response: PostToolResponse,
    facts: Finish,
    store: &Arc<dyn StashStore>,
) -> Result<EntryRun, L2Error> {
    let mut stash = Vec::with_capacity(response.stash_keys.len());
    for key in &response.stash_keys {
        let original = store
            .retrieve(key)
            .map_err(|error| L2Error::Command(format!("stash read {key}: {error}")))?
            .ok_or_else(|| {
                L2Error::Protocol(format!("emitted stash reference {key} is not retrievable"))
            })?;
        stash.push(StashEntry {
            key: key.clone(),
            original,
        });
    }
    let additional_context = response.additional_context.clone();
    let model_visible = match &additional_context {
        Some(context) if !context.is_empty() => format!("{}\n\n{context}", response.output),
        _ => response.output.clone(),
    };
    let mut post_tool = serde_json::to_value(&response)?;
    if let Some(object) = post_tool.as_object_mut() {
        object.remove("output");
    }
    Ok(EntryRun {
        model_visible,
        latency_s: facts.runtime_s + facts.rtk_overhead_s.unwrap_or(0.0),
        evidence: EntryEvidence {
            mode: facts.mode,
            tool_name: facts.tool.to_owned(),
            content_origin: facts.origin,
            command: facts.command,
            pre_tool_action: facts.pre_tool_action,
            executed_command: facts.executed_command,
            tool_status: facts.status,
            output_optimization: facts.optimization,
            post_tool,
            additional_context,
            stash,
            runtime_s: facts.runtime_s,
            rtk_overhead_s: facts.rtk_overhead_s,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uniform_array() -> String {
        let items: Vec<Value> = (0..120)
            .map(|i| json!({"id": i, "name": format!("record {i}"), "note": "x".repeat(48)}))
            .collect();
        serde_json::to_string(&items).expect("array serializes")
    }

    fn successful_build_log() -> String {
        let mut log: String = (0..60)
            .map(|i| {
                format!("   Compiling fixture_dependency_{i:02} v0.1.0 (/work/deps/dep{i:02})\n")
            })
            .collect();
        log.push_str("warning: unused variable: `scratch`\n --> src/main.rs:12:9\n   |\n12 |     let scratch = 1;\n   |         ^^^^^^^ help: if this is intentional, prefix it with an underscore: `_scratch`\n\n");
        log.push_str("    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.21s\n");
        log
    }

    /// API responses use the zero-truncation threshold family: production only
    /// rewrites them losslessly, whatever the recovery mode.
    #[test]
    fn api_json_is_only_rewritten_losslessly() {
        let original = uniform_array();
        let outputs: Vec<String> = Mode::ALL
            .into_iter()
            .map(|mode| {
                let run = post_tool_static(Category::Json, &original, mode).unwrap();
                assert_eq!(
                    run.evidence.post_tool["recoverability"], "lossless",
                    "{mode:?}"
                );
                assert!(run.evidence.stash.is_empty(), "{mode:?}");
                run.model_visible
            })
            .collect();
        assert!(outputs[0].len() < original.len());
        assert!(outputs.iter().all(|output| output == &outputs[0]));
    }

    #[test]
    fn heterogeneous_api_json_without_savings_stays_verbatim() {
        let mixed: Vec<Value> = (0..90)
            .map(|i| match i % 3 {
                0 => json!({"id": i, "kind": "log", "message": format!("request {i} done")}),
                1 => {
                    json!({"id": i, "user": {"name": format!("user-{i}")}, "score": i as f64 / 3.0})
                }
                _ => json!(format!("GET /api/endpoint_{i} 200 OK")),
            })
            .collect();
        let original = serde_json::to_string(&mixed).unwrap();
        let run = post_tool_static(Category::Json, &original, Mode::Default).unwrap();
        assert_eq!(run.evidence.post_tool["disposition"], "no_savings");
        assert_eq!(run.model_visible, original);
    }

    #[test]
    fn authoritative_file_content_is_never_replaced() {
        let source = "fn main() {\n    println!(\"hello\");\n}\n".repeat(40);
        let run = post_tool_static(Category::Code, &source, Mode::Default).unwrap();
        assert_eq!(run.model_visible, source);
        assert_eq!(run.evidence.post_tool["disposition"], "passthrough");
    }

    /// A native build command never reaches rtk, so this runs without it; the
    /// successful log is offloaded behind retrievable Stash only when a store exists.
    #[test]
    fn successful_build_log_is_offloaded_only_with_a_store() {
        let raw = RawCapture {
            text: successful_build_log(),
            wall_s: 0.0,
            success: true,
        };
        let missing_rtk = Path::new("/nonexistent/rtk");
        let data_dir = std::env::temp_dir();
        let cwd = std::env::temp_dir();
        let with_store = run_command(
            "cargo build",
            &raw,
            &cwd,
            (missing_rtk, &data_dir),
            Mode::Default,
            "t1",
        )
        .unwrap();
        assert_eq!(
            with_store.evidence.pre_tool_action,
            Some(PreToolAction::Passthrough)
        );
        assert_eq!(
            with_store.evidence.post_tool["recoverability"],
            "retrievable"
        );
        assert!(!with_store.evidence.stash.is_empty());
        for entry in &with_store.evidence.stash {
            assert!(!entry.original.is_empty());
            assert!(with_store.model_visible.contains(&entry.key));
            assert!(
                raw.text
                    .contains(entry.original.lines().next().unwrap_or_default())
            );
        }
        let without = run_command(
            "cargo build",
            &raw,
            &cwd,
            (missing_rtk, &data_dir),
            Mode::NoStash,
            "t2",
        )
        .unwrap();
        assert!(without.evidence.stash.is_empty());
        assert_ne!(without.evidence.post_tool["recoverability"], "retrievable");
    }

    /// A failing command is an error result: Core never replaces it, and any
    /// diagnostic it adds counts toward what the model receives.
    #[test]
    fn failed_build_stays_original_and_context_counts_as_visible() {
        let raw = RawCapture {
            text: format!(
                "{}error[E0308]: mismatched types\n --> src/main.rs:5:13\n",
                successful_build_log()
            ),
            wall_s: 0.0,
            success: false,
        };
        let missing_rtk = Path::new("/nonexistent/rtk");
        let data_dir = std::env::temp_dir();
        let run = run_command(
            "cargo build",
            &raw,
            &std::env::temp_dir(),
            (missing_rtk, &data_dir),
            Mode::Default,
            "t3",
        )
        .unwrap();
        assert_eq!(run.evidence.post_tool["disposition"], "tool_error");
        assert!(run.model_visible.starts_with(&raw.text));
        assert!(
            run.evidence.additional_context.is_none(),
            "ordinary compiler failures have no environment hint"
        );
        let environment_error = RawCapture {
            text: "cargo: command not found".to_owned(),
            wall_s: 0.0,
            success: false,
        };
        let hinted = run_command(
            "cargo build",
            &environment_error,
            &std::env::temp_dir(),
            (missing_rtk, &data_dir),
            Mode::Default,
            "environment-error",
        )
        .unwrap();
        let context = hinted
            .evidence
            .additional_context
            .as_ref()
            .expect("environment failure must carry a hint");
        assert_eq!(
            hinted.model_visible,
            format!("{}\n\n{context}", environment_error.text)
        );
    }

    #[test]
    fn mode_names_round_trip() {
        for mode in Mode::ALL {
            assert_eq!(Mode::parse(mode.name()), Some(mode));
        }
        assert_eq!(Mode::parse("bogus"), None);
    }
}

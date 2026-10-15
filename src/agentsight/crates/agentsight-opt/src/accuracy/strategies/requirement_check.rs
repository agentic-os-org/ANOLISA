//! requirement_check 需求逐项核对 — detects missed / off-target / format-violating
//! / constraint-violating output.
//!
//! Consumes the shared checklist (requirements/scope/format/constraints).
//! One LLM coverage call comparing checklist × (overview + final_answer +
//! files_touched). All issues are L4 (semantic comparison, no auto-patch).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::llm::ChatMessage;
use crate::types::{DefectType, EvidenceTier, RootObject};

use crate::accuracy::detector::{AnalysisCtx, Detector, RawIssue};

const COVERAGE_PROMPT: &str = include_str!("../../../prompts/requirement_coverage.md");

/// Max chars per step command in the overview line (UTF-8 safe).
const OVERVIEW_CMD_CHARS: usize = 80;

/// Coverage verdict for a single checklist item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageVerdict {
    pub item: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub turn: usize,
    pub status: String, // "satisfied" | "missing" | "reasonably_skipped"
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageOutput {
    #[serde(default)]
    pub verdicts: Vec<CoverageVerdict>,
}

pub struct RequirementCheckStrategy;

impl RequirementCheckStrategy {
    pub fn new() -> Self {
        Self
    }

    /// Compact execution overview: one line per tool call.
    fn build_overview(ctx: &AnalysisCtx<'_>) -> String {
        let mut lines = Vec::with_capacity(ctx.inv.tool_calls.len());
        for (i, tc) in ctx.inv.tool_calls.iter().enumerate() {
            let status = if tc.err { "✗" } else { "✓" };
            let cmd: String = tc.cmd.chars().take(OVERVIEW_CMD_CHARS).collect();
            lines.push(format!("[Step {}] {} {} {}", i + 1, tc.name, status, cmd));
        }
        if lines.is_empty() {
            "（无工具调用）".to_string()
        } else {
            lines.join("\n")
        }
    }

    /// Aggregate files touched by write/edit tool calls, deduplicated in
    /// first-touch order. Reads the recorded `target` path - `cmd` is a JSON
    /// blob truncated at 50 chars, which cuts deep paths mid-segment.
    fn aggregate_files_touched(ctx: &AnalysisCtx<'_>) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        ctx.inv
            .tool_calls
            .iter()
            .filter(|call| crate::cost::is_write_tool(&call.name))
            .filter_map(|call| call.target.clone())
            .filter(|path| seen.insert(path.clone()))
            .collect()
    }

    /// Map checklist kind → defect_type.
    fn defect_type_for(kind: &str) -> DefectType {
        match kind {
            "格式" => DefectType::Style,
            "约束" => DefectType::Context,
            _ => DefectType::Workflow, // 需求 / 范围 / unknown
        }
    }
}

#[async_trait]
impl Detector for RequirementCheckStrategy {
    fn name(&self) -> &'static str {
        "requirement_check"
    }

    async fn detect(&self, ctx: &AnalysisCtx<'_>) -> Vec<RawIssue> {
        let checklist = &ctx.extraction.checklist;
        if checklist.is_empty() {
            tracing::debug!("[requirement_check] Empty checklist, skipping");
            return vec![];
        }

        let checklist_text: String = checklist
            .iter()
            .map(|c| format!("- [{}|{}|轮{}] {}", c.kind, c.priority, c.turn, c.item))
            .collect::<Vec<_>>()
            .join("\n");

        let overview = Self::build_overview(ctx);
        let files_touched = Self::aggregate_files_touched(ctx);
        let files_text = if files_touched.is_empty() {
            "（无文件变更）".to_string()
        } else {
            files_touched.join("\n")
        };

        let messages = vec![
            ChatMessage::system(COVERAGE_PROMPT),
            ChatMessage::user(format!(
                "## 要点清单\n\n{}\n\n\
                 ## 执行步骤摘要\n\n{}\n\n\
                 ## 最终答案\n\n{}\n\n\
                 ## 文件变更\n\n{}\n\n\
                 逐项判断每个要点的覆盖状态。仅返回 JSON。",
                checklist_text, overview, ctx.inv.final_answer, files_text
            )),
        ];

        let coverage: CoverageOutput = match ctx
            .client
            .chat_json_parsed_labeled(messages, Some("accuracy:requirement_check:coverage"))
            .await
        {
            Ok(output) => {
                ctx.judgments.record_ok();
                output
            }
            Err(e) => {
                ctx.judgments.record_failure(&e);
                tracing::warn!("[requirement_check] Coverage comparison failed: {e}");
                return vec![];
            }
        };

        coverage
            .verdicts
            .into_iter()
            .filter(|v| v.status == "missing")
            .map(|v| {
                let defect_type = Self::defect_type_for(&v.kind);
                let symptom_prefix = match v.kind.as_str() {
                    "格式" => "格式不符",
                    "约束" => "违背约束",
                    "范围" => "越出范围",
                    _ => "漏要求",
                };
                RawIssue {
                    symptom: format!("{}: {}", symptom_prefix, v.item),
                    defect_type,
                    primary_object: RootObject::Skill,
                    evidence_tier: EvidenceTier::L4,
                    tool_call_id: None,
                    detail: format!(
                        "用户要点 `{}`（{}，轮{}）未被满足。原因: {}",
                        v.item,
                        if v.kind.is_empty() { "需求" } else { &v.kind },
                        v.turn,
                        if v.reason.is_empty() {
                            "未说明"
                        } else {
                            &v.reason
                        }
                    ),
                    verify: "区分'真遗漏'与'合理跳过/更优方案'，人工确认后决定是否修复。".into(),
                    fix: "Skill 补完成检查清单 + 需求逐项确认：在声明完成前逐一核对用户需求、范围、格式与约束。".into(),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accuracy::detector::JudgmentLog;

    /// The "files touched" section of the coverage prompt must list file
    /// paths. Before the recorded target existed it listed the command
    /// summary - a JSON blob truncated at 50 chars, which cuts deep paths
    /// mid-segment and buries them under old_string/content noise.
    #[test]
    fn files_touched_lists_paths_not_truncated_argument_json() {
        let traj = crate::atif::AtifTrajectory::from_json(
            r#"{"schema_version":"ATIF-v1.6","session_id":"s1",
                "agent":{"name":"a","version":"1"},
                "steps":[
                  {"step_id":1,"source":"agent","timestamp":"2025-01-01T00:00:01Z",
                   "tool_calls":[{"tool_call_id":"c1","function_name":"Edit",
                     "arguments":{"file_path":"src/agentsight/deep/path/mod.rs",
                                  "old_string":"fn old()","new_string":"fn new()"}}],
                   "observation":{"results":[{"source_call_id":"c1","content":"ok"}]}}
                ]}"#,
        )
        .unwrap();
        let inv = crate::trace::build_inventory(&traj);
        let client = crate::llm::LlmClient::with_config("http://localhost", "key", "m");
        let extraction = crate::accuracy::extract::SharedExtraction::default();
        let judgments = JudgmentLog::default();
        let ctx = AnalysisCtx {
            inv: &inv,
            client: &client,
            repo_root: None,
            extraction: &extraction,
            judgments: &judgments,
        };
        let files = RequirementCheckStrategy::aggregate_files_touched(&ctx);
        assert_eq!(
            files,
            vec!["src/agentsight/deep/path/mod.rs".to_string()],
            "the coverage judgment needs the real path, not a JSON fragment"
        );
    }

    /// The coverage prompt judges both `satisfied` ("要点已在…文件变更中被满足")
    /// and scope violations ("需对照…文件变更判断是否越界") against this list, so a
    /// write tool missing from it turns real edits into "（无文件变更）" and the
    /// items that depend on them into false `missing` verdicts. The list must be
    /// the shared write-tool set: Codex's `apply_patch` and the editor-style
    /// tools (`str_replace_editor`, `create_file`) existed only in
    /// `cost::WRITE_TOOLS`, while `WriteFile`/`EditFile` existed only here.
    #[test]
    fn files_touched_covers_every_write_tool_name() {
        let traj = crate::atif::AtifTrajectory::from_json(
            r#"{"schema_version":"ATIF-v1.6","session_id":"s1",
                "agent":{"name":"a","version":"1"},
                "steps":[
                  {"step_id":1,"source":"agent","timestamp":"2025-01-01T00:00:01Z",
                   "tool_calls":[
                     {"tool_call_id":"c1","function_name":"apply_patch",
                      "arguments":{"file_path":"src/patch_target.rs"}},
                     {"tool_call_id":"c2","function_name":"str_replace_editor",
                      "arguments":{"path":"src/editor_target.rs"}},
                     {"tool_call_id":"c3","function_name":"WriteFile",
                      "arguments":{"path":"src/writefile_target.rs"}}
                   ],
                   "observation":{"results":[
                     {"source_call_id":"c1","content":"ok"},
                     {"source_call_id":"c2","content":"ok"},
                     {"source_call_id":"c3","content":"ok"}]}},
                  {"step_id":2,"source":"agent","timestamp":"2025-01-01T00:00:02Z",
                   "tool_calls":[
                     {"tool_call_id":"c4","function_name":"Read",
                      "arguments":{"file_path":"src/only_read.rs"}}
                   ],
                   "observation":{"results":[{"source_call_id":"c4","content":"ok"}]}}
                ]}"#,
        )
        .unwrap();
        let inv = crate::trace::build_inventory(&traj);
        let client = crate::llm::LlmClient::with_config("http://localhost", "key", "m");
        let extraction = crate::accuracy::extract::SharedExtraction::default();
        let judgments = JudgmentLog::default();
        let ctx = AnalysisCtx {
            inv: &inv,
            client: &client,
            repo_root: None,
            extraction: &extraction,
            judgments: &judgments,
        };

        let files = RequirementCheckStrategy::aggregate_files_touched(&ctx);
        assert_eq!(
            files,
            vec![
                "src/patch_target.rs".to_string(),
                "src/editor_target.rs".to_string(),
                "src/writefile_target.rs".to_string(),
            ],
            "every write tool must contribute its target, and a read must not"
        );
    }

    #[test]
    fn kind_maps_to_defect_type() {
        assert_eq!(
            RequirementCheckStrategy::defect_type_for("需求"),
            DefectType::Workflow
        );
        assert_eq!(
            RequirementCheckStrategy::defect_type_for("范围"),
            DefectType::Workflow
        );
        assert_eq!(
            RequirementCheckStrategy::defect_type_for("格式"),
            DefectType::Style
        );
        assert_eq!(
            RequirementCheckStrategy::defect_type_for("约束"),
            DefectType::Context
        );
    }
}

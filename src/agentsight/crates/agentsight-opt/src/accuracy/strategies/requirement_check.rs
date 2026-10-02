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

/// Tool names that represent file write/edit operations.
const FILE_WRITE_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "WriteFile", "EditFile"];

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

    /// Aggregate files touched by Edit/Write tool calls.
    fn aggregate_files_touched(ctx: &AnalysisCtx<'_>) -> Vec<String> {
        ctx.inv
            .tool_calls
            .iter()
            .filter(|call| {
                FILE_WRITE_TOOLS
                    .iter()
                    .any(|t| call.name.eq_ignore_ascii_case(t))
            })
            .map(|call| call.cmd.clone())
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
            Ok(output) => output,
            Err(e) => {
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

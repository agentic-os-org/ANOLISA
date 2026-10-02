//! confirm_before_act 行动前确认 — detects unauthorized sensitive write
//! operations (L1) and acting on ambiguous requests without confirming (L5).
//!
//! Rust supplies the sensitive-op candidate list (keyword match); one LLM call
//! judges authorization against user turns and consumes the shared ambiguity
//! note. Skips the LLM entirely when there are no signals.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::llm::ChatMessage;
use crate::types::{DefectType, EvidenceTier, RootObject, ToolCallRecord};

use crate::accuracy::detector::{AnalysisCtx, Detector, RawIssue};

const SYSTEM_PROMPT: &str = include_str!("../../../prompts/confirm_before_act.md");

/// Sensitive write-operation keywords (matched against lowercase cmd).
const SENSITIVE_KEYWORDS: &[&str] = &[
    "git push",
    "git merge",
    "git reset",
    "git revert",
    "rm ",
    "rm -",
    "delete",
    "drop table",
    "force",
    "--hard",
    "sudo",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UnauthorizedOp {
    #[serde(default)]
    call_id: String,
    #[serde(default)]
    cmd: String,
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AmbiguityVerdict {
    #[serde(default)]
    acted: bool,
    #[serde(default)]
    note: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ConfirmOutput {
    #[serde(default)]
    unauthorized_ops: Vec<UnauthorizedOp>,
    #[serde(default)]
    acted_on_ambiguity: Option<AmbiguityVerdict>,
}

pub struct ConfirmBeforeActStrategy;

impl ConfirmBeforeActStrategy {
    pub fn new() -> Self {
        Self
    }

    /// Rust oracle: collect sensitive-op candidates by keyword match.
    fn find_sensitive_ops(calls: &[ToolCallRecord]) -> Vec<&ToolCallRecord> {
        calls
            .iter()
            .filter(|c| {
                let lc = c.cmd.to_lowercase();
                SENSITIVE_KEYWORDS.iter().any(|k| lc.contains(k))
            })
            .collect()
    }
}

#[async_trait]
impl Detector for ConfirmBeforeActStrategy {
    fn name(&self) -> &'static str {
        "confirm_before_act"
    }

    async fn detect(&self, ctx: &AnalysisCtx<'_>) -> Vec<RawIssue> {
        let sensitive = Self::find_sensitive_ops(&ctx.inv.tool_calls);
        let ambiguous = ctx
            .extraction
            .ambiguity
            .as_ref()
            .map(|a| a.ambiguous)
            .unwrap_or(false);

        if sensitive.is_empty() && !ambiguous {
            tracing::debug!("[confirm_before_act] No sensitive ops or ambiguity, skipping LLM");
            return vec![];
        }

        let user_turns_text = if ctx.inv.user_turns.is_empty() {
            "（无用户轮次数据）".to_string()
        } else {
            ctx.inv
                .user_turns
                .iter()
                .map(|t| format!("[轮{}] {}", t.turn, t.text))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let ops_text = if sensitive.is_empty() {
            "（无敏感写操作）".to_string()
        } else {
            sensitive
                .iter()
                .map(|c| format!("- call_id={} tool={} cmd={}", c.call_id, c.name, c.cmd))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let ambiguity_text = match ctx.extraction.ambiguity.as_ref() {
            Some(a) if a.ambiguous => format!("请求存在歧义：{}", a.note),
            _ => "请求无歧义".to_string(),
        };

        let messages = vec![
            ChatMessage::system(SYSTEM_PROMPT),
            ChatMessage::user(format!(
                "## 用户各轮原话\n\n{}\n\n\
                 ## 敏感写操作列表\n\n{}\n\n\
                 ## 歧义提示\n\n{}\n\n\
                 判断越权操作与歧义未确认情况。仅返回 JSON。",
                user_turns_text, ops_text, ambiguity_text
            )),
        ];

        let output: ConfirmOutput = match ctx
            .client
            .chat_json_parsed_labeled(messages, Some("accuracy:confirm_before_act"))
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("[confirm_before_act] LLM judgment failed: {e}");
                return vec![];
            }
        };

        let mut issues = Vec::new();

        for op in output.unauthorized_ops {
            // Anchor back to a real tool call — drop hallucinated call_ids.
            let Some(call) = ctx.inv.tool_calls.iter().find(|c| c.call_id == op.call_id) else {
                tracing::debug!(
                    "[confirm_before_act] Dropping unmatched call_id: {}",
                    op.call_id
                );
                continue;
            };
            issues.push(RawIssue {
                symptom: format!("未经授权执行敏感写操作: {}", call.cmd),
                defect_type: DefectType::Workflow,
                primary_object: RootObject::Skill,
                evidence_tier: EvidenceTier::L1,
                tool_call_id: Some(call.call_id.clone()),
                detail: format!(
                    "工具 `{}` 执行了 `{}`，但用户原话中未见对应授权。原因: {}",
                    call.name, call.cmd, op.reason
                ),
                verify: "对照用户原话确认该操作是否有明确或隐含授权。".into(),
                fix: "Skill/Prompt 补充行动前确认规则：破坏性或对外可见操作必须先征得用户同意。"
                    .into(),
            });
        }

        if let Some(v) = output.acted_on_ambiguity {
            if v.acted && ambiguous {
                issues.push(RawIssue {
                    symptom: "用户请求有歧义，Agent 未确认即动手".into(),
                    defect_type: DefectType::Reasoning,
                    primary_object: RootObject::Skill,
                    evidence_tier: EvidenceTier::L5,
                    tool_call_id: None,
                    detail: format!("歧义点: {}", v.note),
                    verify: "人工确认 Agent 选择的解释是否与用户意图一致；一致则可忽略。".into(),
                    fix: "Skill/Prompt 补充歧义澄清规则：多种合理解释时先向用户确认再执行。".into(),
                });
            }
        }

        issues
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_call(name: &str, cmd: &str) -> ToolCallRecord {
        ToolCallRecord {
            name: name.into(),
            call_id: format!("{name}_{cmd}"),
            start: 0.0,
            dur: 1.0,
            cmd: cmd.into(),
            err: false,
            result_tokens: None,
        }
    }

    #[test]
    fn sensitive_ops_matched_by_keyword() {
        let calls = vec![
            make_call("Bash", "git push --force origin main"),
            make_call("Bash", "cargo build"),
            make_call("Bash", "sudo systemctl restart nginx"),
            make_call("Read", "src/main.rs"),
        ];
        let hits = ConfirmBeforeActStrategy::find_sensitive_ops(&calls);
        assert_eq!(hits.len(), 2);
        assert!(hits[0].cmd.contains("git push"));
        assert!(hits[1].cmd.contains("sudo"));
    }
}

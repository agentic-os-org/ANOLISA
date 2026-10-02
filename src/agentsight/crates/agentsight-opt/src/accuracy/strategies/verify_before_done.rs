//! verify_before_done 完成前验证 — detects "claims success but actually failed".
//!
//! Consumes shared extraction claims (no own LLM call). Both oracles are gated
//! on the agent having made a completion claim — honest failure admissions are
//! not false successes.
//!
//! - (a) Unrecovered tool errors (L1): a tool call errored, no later same-tool
//!   call succeeded, AND the error was NOT acknowledged in the final answer.
//! - (b) No verification actions (L3): agent claims success but never ran
//!   test/build/lint.

use async_trait::async_trait;

use crate::types::{DefectType, EvidenceTier, RootObject, ToolCallRecord};

use crate::accuracy::detector::{AnalysisCtx, Detector, RawIssue};
use crate::accuracy::extract::CompletionClaim;

/// Keywords indicating a verification / test action in tool commands.
const VERIFY_KEYWORDS: &[&str] = &[
    "test",
    "pytest",
    "jest",
    "mocha",
    "vitest",
    "cargo test",
    "build",
    "compile",
    "make",
    "cmake",
    "lint",
    "eslint",
    "ruff",
    "clippy",
    "mypy",
    "typecheck",
    "tsc",
    "pyright",
    "npm run",
    "yarn run",
    "pnpm run",
];

/// Common failure-admission phrases in a final answer.
const FAILURE_PHRASES: &[&str] = &[
    "failed",
    "error",
    "失败",
    "错误",
    "无法",
    "could not",
    "unable to",
    "couldn't",
    "not able",
];

pub struct VerifyBeforeDoneStrategy;

impl VerifyBeforeDoneStrategy {
    pub fn new() -> Self {
        Self
    }

    /// Oracle (a): unrecovered tool errors not acknowledged in the final answer.
    fn find_unrecovered_errors(ctx: &AnalysisCtx<'_>, claims: &[CompletionClaim]) -> Vec<RawIssue> {
        if claims.is_empty() {
            return vec![];
        }

        let calls = &ctx.inv.tool_calls;
        let final_answer_lower = ctx.inv.final_answer.to_lowercase();
        let mut issues = Vec::new();

        for (i, call) in calls.iter().enumerate() {
            if !call.err {
                continue;
            }
            // Relaxed: a later successful call with the same tool name counts
            // as recovery (exact cmd match was too strict — retries often
            // tweak arguments).
            let recovered = calls[i + 1..].iter().any(|c| c.name == call.name && !c.err);
            if recovered {
                continue;
            }

            if Self::is_error_acknowledged(&final_answer_lower, call) {
                tracing::debug!(
                    "[verify_before_done] Error {} '{}' acknowledged in final answer, skipping",
                    call.name,
                    call.cmd
                );
                continue;
            }

            issues.push(RawIssue {
                symptom: format!(
                    "声称完成但工具 {} 执行失败且未恢复: {}",
                    call.name, call.cmd
                ),
                defect_type: DefectType::Workflow,
                primary_object: RootObject::Skill,
                evidence_tier: EvidenceTier::L1,
                tool_call_id: Some(call.call_id.clone()),
                detail: format!(
                    "Agent 声明了完成，但工具 `{}` 调用 `{}` 返回错误，且后续无同名工具调用成功恢复，且最终答案中未承认此失败。",
                    call.name, call.cmd
                ),
                verify: "若该错误确实影响最终产出，则此 issue 有效；若任务不依赖该工具结果，可忽略。".into(),
                fix: "Skill 补充完成前置校验清单：在声明完成前检查所有工具调用状态。".into(),
            });
        }

        issues
    }

    /// Tightened: only counts as acknowledged when a failure phrase AND the
    /// tool name / command snippet co-occur in the final answer. Mentioning
    /// the tool name alone is not an admission.
    fn is_error_acknowledged(final_answer_lower: &str, call: &ToolCallRecord) -> bool {
        let has_failure_phrase = FAILURE_PHRASES
            .iter()
            .any(|p| final_answer_lower.contains(p));
        if !has_failure_phrase {
            return false;
        }

        if final_answer_lower.contains(&call.name.to_lowercase()) {
            return true;
        }
        let cmd_lower = call.cmd.to_lowercase();
        cmd_lower.chars().count() >= 4 && final_answer_lower.contains(&cmd_lower)
    }

    /// Oracle (b): claims success but never ran any verification action.
    fn check_no_verification(
        ctx: &AnalysisCtx<'_>,
        claims: &[CompletionClaim],
    ) -> Option<RawIssue> {
        if claims.is_empty() {
            return None;
        }

        let has_verification = ctx.inv.tool_calls.iter().any(|call| {
            let cmd_lower = call.cmd.to_lowercase();
            VERIFY_KEYWORDS.iter().any(|kw| cmd_lower.contains(kw))
        });

        if has_verification {
            return None;
        }

        Some(RawIssue {
            symptom: "声称完成但未运行任何验证动作（测试/构建/lint）".into(),
            defect_type: DefectType::Workflow,
            primary_object: RootObject::Skill,
            evidence_tier: EvidenceTier::L3,
            tool_call_id: None,
            detail: format!(
                "最终答案声明完成 {} 项，但全程无 test/build/lint/typecheck 类工具调用。",
                claims.len()
            ),
            verify: "文档类任务本不需验证，此 issue 可能为误报；编码类任务则有效。".into(),
            fix: "Skill 补充完成前置校验：编码任务完成前必须运行验证命令。".into(),
        })
    }
}

#[async_trait]
impl Detector for VerifyBeforeDoneStrategy {
    fn name(&self) -> &'static str {
        "verify_before_done"
    }

    async fn detect(&self, ctx: &AnalysisCtx<'_>) -> Vec<RawIssue> {
        let claims = &ctx.extraction.claims;
        if claims.is_empty() {
            tracing::debug!("[verify_before_done] No completion claims, skipping");
            return vec![];
        }

        let mut issues = Self::find_unrecovered_errors(ctx, claims);
        if let Some(issue) = Self::check_no_verification(ctx, claims) {
            issues.push(issue);
        }
        issues
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accuracy::extract::SharedExtraction;
    use crate::llm::LlmClient;
    use crate::trace::TraceInventory;

    fn make_call(name: &str, cmd: &str, start: f64, err: bool) -> ToolCallRecord {
        ToolCallRecord {
            name: name.into(),
            call_id: format!("{name}_{start}"),
            start,
            dur: 1.0,
            cmd: cmd.into(),
            err,
            result_tokens: None,
        }
    }

    fn make_client() -> LlmClient {
        LlmClient::with_config("http://localhost", "test-key", "test-model")
    }

    fn make_ctx<'a>(
        inv: &'a TraceInventory,
        client: &'a LlmClient,
        extraction: &'a SharedExtraction,
    ) -> AnalysisCtx<'a> {
        AnalysisCtx {
            inv,
            client,
            repo_root: None,
            extraction,
        }
    }

    fn claims() -> Vec<CompletionClaim> {
        vec![CompletionClaim {
            claim: "已完成".into(),
            kind: "done".into(),
        }]
    }

    #[test]
    fn unrecovered_error_fires_l1() {
        let inv = TraceInventory {
            tool_calls: vec![make_call("Bash", "cargo run", 1.0, true)],
            user_turns: vec![],
            final_answer: "全部完成".into(),
            skill_contract: None,
        };
        let client = make_client();
        let extraction = SharedExtraction::default();
        let ctx = make_ctx(&inv, &client, &extraction);

        let issues = VerifyBeforeDoneStrategy::find_unrecovered_errors(&ctx, &claims());
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].evidence_tier, EvidenceTier::L1);
    }

    #[test]
    fn recovered_by_same_tool_name_suppresses() {
        // Retry with different cmd but same tool name → recovered (relaxed).
        let inv = TraceInventory {
            tool_calls: vec![
                make_call("Bash", "cargo run --bad-flag", 1.0, true),
                make_call("Bash", "cargo run", 2.0, false),
            ],
            user_turns: vec![],
            final_answer: "全部完成".into(),
            skill_contract: None,
        };
        let client = make_client();
        let extraction = SharedExtraction::default();
        let ctx = make_ctx(&inv, &client, &extraction);

        let issues = VerifyBeforeDoneStrategy::find_unrecovered_errors(&ctx, &claims());
        assert!(issues.is_empty());
    }

    #[test]
    fn acknowledged_requires_failure_phrase() {
        // Tool name mentioned WITHOUT failure phrase → NOT acknowledged.
        let call = make_call("Bash", "cargo run", 1.0, true);
        assert!(!VerifyBeforeDoneStrategy::is_error_acknowledged(
            "我用 bash 运行了程序，一切正常",
            &call
        ));
        // Failure phrase + tool name → acknowledged.
        assert!(VerifyBeforeDoneStrategy::is_error_acknowledged(
            "bash 命令执行失败，未能完成",
            &call
        ));
        // Failure phrase alone without tool name/cmd → NOT acknowledged.
        assert!(!VerifyBeforeDoneStrategy::is_error_acknowledged(
            "有一步失败了",
            &call
        ));
    }
}

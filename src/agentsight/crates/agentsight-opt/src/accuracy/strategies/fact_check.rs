//! fact_check 事实回查 — verifies symbols/files referenced in the final answer
//! actually exist in the repo.
//!
//! Consumes shared assertions (no own LLM call). Grep runs in a blocking task.
//! - grep miss → L2 (objective, has checker)
//! - API claims → L4 (needs external doc knowledge, capped)
//! - grep execution failure → assertion skipped (not reported as missing)
//! - `repo_root=None` → strategy skips gracefully.

use std::path::{Path, PathBuf};
use std::process::Command;

use async_trait::async_trait;

use crate::types::{DefectType, EvidenceTier, RootObject};

use crate::accuracy::detector::{AnalysisCtx, Detector, RawIssue};
use crate::accuracy::extract::Assertion;

pub struct FactCheckStrategy;

/// Grep check outcome: found / not found / check itself failed.
enum GrepOutcome {
    Found,
    NotFound,
    CheckFailed,
}

impl FactCheckStrategy {
    pub fn new() -> Self {
        Self
    }

    fn grep_symbol(repo_root: &Path, symbol: &str, kind: &str) -> GrepOutcome {
        if matches!(kind, "file" | "path") {
            return if repo_root.join(symbol).exists() {
                GrepOutcome::Found
            } else {
                GrepOutcome::NotFound
            };
        }

        let output = Command::new("grep")
            .args([
                "-r",
                "-l",
                "--exclude-dir=.git",
                "--exclude-dir=node_modules",
                "--exclude-dir=target",
                "--exclude-dir=dist",
                "--include=*.rs",
                "--include=*.py",
                "--include=*.ts",
                "--include=*.tsx",
                "--include=*.js",
                "--include=*.jsx",
                "--include=*.java",
                "--include=*.go",
                "--include=*.md",
                "-F",
                symbol,
            ])
            .arg(repo_root)
            .output();

        match output {
            // grep exit 0 = match found, exit 1 = no match, >1 = error.
            Ok(out) if out.status.success() && !out.stdout.is_empty() => GrepOutcome::Found,
            Ok(out) if out.status.code() == Some(1) => GrepOutcome::NotFound,
            Ok(_) | Err(_) => GrepOutcome::CheckFailed,
        }
    }
}

#[async_trait]
impl Detector for FactCheckStrategy {
    fn name(&self) -> &'static str {
        "fact_check"
    }

    async fn detect(&self, ctx: &AnalysisCtx<'_>) -> Vec<RawIssue> {
        let repo_root: PathBuf = match ctx.repo_root {
            Some(root) => root.to_path_buf(),
            None => {
                tracing::debug!("[fact_check] No repo_root, skipping");
                return vec![];
            }
        };

        let assertions: Vec<Assertion> = ctx.extraction.assertions.clone();
        if assertions.is_empty() {
            tracing::debug!("[fact_check] No assertions, skipping");
            return vec![];
        }

        let repo_display = repo_root.display().to_string();

        // Grep is blocking I/O — run off the async executor.
        let results = tokio::task::spawn_blocking(move || {
            assertions
                .into_iter()
                .map(|a| {
                    let outcome = Self::grep_symbol(&repo_root, &a.symbol, &a.kind);
                    (a, outcome)
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("[fact_check] Grep task panicked: {e}");
            vec![]
        });

        let mut issues = Vec::new();
        for (assertion, outcome) in results {
            match outcome {
                GrepOutcome::Found => {}
                GrepOutcome::CheckFailed => {
                    tracing::debug!(
                        "[fact_check] Check failed for `{}`, skipping assertion",
                        assertion.symbol
                    );
                }
                GrepOutcome::NotFound => {
                    let tier = if assertion.kind == "api" {
                        EvidenceTier::L4
                    } else {
                        EvidenceTier::L2
                    };
                    issues.push(RawIssue {
                        symptom: format!(
                            "引用的 {} `{}` 在代码仓库中未找到",
                            assertion.kind, assertion.symbol
                        ),
                        defect_type: DefectType::Knowledge,
                        primary_object: RootObject::Skill,
                        evidence_tier: tier,
                        tool_call_id: None,
                        detail: format!(
                            "最终答案引用了 {} `{}`，但在仓库 `{}` 中 grep 未找到任何匹配。\
                             可能是编造的符号或已过时的 API。",
                            assertion.kind, assertion.symbol, repo_display
                        ),
                        verify: "人工确认该符号是否确实不存在，或是否因搜索范围不足导致漏检。"
                            .into(),
                        fix: format!(
                            "Skill 知识库更新：移除或替换对 `{}` 的引用，确保引用符号与实际代码一致。",
                            assertion.symbol
                        ),
                    });
                }
            }
        }

        issues
    }
}

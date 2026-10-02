//! experience_library 经验沉淀 — detects repeated failures / backtracking /
//! spinning, and distills reusable lessons.
//!
//! Rust computes signals from `inv.tool_calls`:
//! - repeat clusters: same (name, cmd) signature ≥ 3 times
//! - error retry chains: consecutive errors on the same tool
//! - backtrack commands: git reset/checkout/revert/stash/restore keywords
//!
//! No signals → skip LLM. LLM judges spinning vs progress and distills lesson
//! entries (scene + wrong way + right way); the lesson text becomes `fix`. L1.
//!
//! TODO(#opt-experience-dedup): overlaps the cost dimension's detour
//! candidate (`cost/prompts/detour.rs`), which judges the same pitfalls
//! against a full turn ledger. Until converged, one pitfall can surface on both
//! the accuracy and the cost page. Same overlap applies to the perf strategy of
//! the same name.

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::llm::ChatMessage;
use crate::types::{DefectType, EvidenceTier, RootObject, ToolCallRecord};

use crate::accuracy::detector::{AnalysisCtx, Detector, RawIssue};

const SYSTEM_PROMPT: &str = include_str!("../../../prompts/experience_library.md");

/// A repeated command signature must occur at least this many times.
const REPEAT_MIN: usize = 3;

/// An error retry chain must be at least this long.
const ERROR_CHAIN_MIN: usize = 2;

/// Backtrack command keywords (matched against lowercase cmd).
const BACKTRACK_KEYWORDS: &[&str] = &[
    "git reset",
    "git checkout --",
    "git revert",
    "git stash",
    "git restore",
    "回退",
    "撤销",
];

/// A Rust-computed inefficiency signal fed to the LLM.
#[derive(Debug, Clone)]
pub struct Signal {
    pub id: String,
    pub desc: String,
    pub first_call_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Lesson {
    #[serde(default)]
    signal: String,
    #[serde(default)]
    wasted: bool,
    #[serde(default)]
    scene: String,
    #[serde(default)]
    wrong_way: String,
    #[serde(default)]
    right_way: String,
    #[serde(default)]
    call_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct LessonsOutput {
    #[serde(default)]
    lessons: Vec<Lesson>,
}

pub struct ExperienceLibraryStrategy;

impl ExperienceLibraryStrategy {
    pub fn new() -> Self {
        Self
    }

    /// Compute all inefficiency signals from tool calls.
    fn compute_signals(calls: &[ToolCallRecord]) -> Vec<Signal> {
        let mut signals = Vec::new();

        // 1. Repeat clusters: same (name, cmd) ≥ REPEAT_MIN.
        let mut counts: HashMap<(&str, &str), (usize, &str)> = HashMap::new();
        for c in calls {
            let entry = counts
                .entry((c.name.as_str(), c.cmd.as_str()))
                .or_insert((0, c.call_id.as_str()));
            entry.0 += 1;
        }
        let mut clusters: Vec<_> = counts
            .into_iter()
            .filter(|(_, (n, _))| *n >= REPEAT_MIN)
            .collect();
        clusters.sort_by_key(|&(_, (n, _))| std::cmp::Reverse(n));
        for ((name, cmd), (n, first_id)) in clusters {
            let cmd_short: String = cmd.chars().take(80).collect();
            signals.push(Signal {
                id: format!("repeat_cluster:{}:{}", name, cmd_short),
                desc: format!("重复调用簇：{} `{}` 共 {} 次", name, cmd_short, n),
                first_call_id: first_id.to_string(),
            });
        }

        // 2. Error retry chains: consecutive errors on the same tool.
        let mut i = 0;
        while i < calls.len() {
            if calls[i].err {
                let mut j = i;
                while j + 1 < calls.len() && calls[j + 1].err && calls[j + 1].name == calls[i].name
                {
                    j += 1;
                }
                let chain_len = j - i + 1;
                if chain_len >= ERROR_CHAIN_MIN {
                    signals.push(Signal {
                        id: format!("error_chain:{}:{}", calls[i].name, calls[i].call_id),
                        desc: format!(
                            "连续报错重试链：{} 连续失败 {} 次（起始命令 `{}`）",
                            calls[i].name,
                            chain_len,
                            calls[i].cmd.chars().take(80).collect::<String>()
                        ),
                        first_call_id: calls[i].call_id.clone(),
                    });
                }
                i = j + 1;
            } else {
                i += 1;
            }
        }

        // 3. Backtrack commands.
        for c in calls {
            let lc = c.cmd.to_lowercase();
            if BACKTRACK_KEYWORDS.iter().any(|k| lc.contains(k)) {
                signals.push(Signal {
                    id: format!("backtrack:{}", c.call_id),
                    desc: format!(
                        "回退操作：{} `{}`",
                        c.name,
                        c.cmd.chars().take(80).collect::<String>()
                    ),
                    first_call_id: c.call_id.clone(),
                });
            }
        }

        signals
    }
}

#[async_trait]
impl Detector for ExperienceLibraryStrategy {
    fn name(&self) -> &'static str {
        "experience_library"
    }

    async fn detect(&self, ctx: &AnalysisCtx<'_>) -> Vec<RawIssue> {
        let signals = Self::compute_signals(&ctx.inv.tool_calls);
        if signals.is_empty() {
            tracing::debug!("[experience_library] No inefficiency signals, skipping LLM");
            return vec![];
        }

        tracing::debug!(
            "[experience_library] {} signals detected, running LLM judgment",
            signals.len()
        );

        let signals_text: String = signals
            .iter()
            .map(|s| format!("- [{}] {} (call_id={})", s.id, s.desc, s.first_call_id))
            .collect::<Vec<_>>()
            .join("\n");

        let messages = vec![
            ChatMessage::system(SYSTEM_PROMPT),
            ChatMessage::user(format!(
                "## 低效信号（程序检测）\n\n{}\n\n\
                 ## 最终答案\n\n{}\n\n\
                 判断哪些信号是真正的空转/踩坑，并提炼经验。仅返回 JSON。",
                signals_text, ctx.inv.final_answer
            )),
        ];

        let output: LessonsOutput = match ctx
            .client
            .chat_json_parsed_labeled(messages, Some("accuracy:experience_library"))
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("[experience_library] LLM judgment failed: {e}");
                return vec![];
            }
        };

        output
            .lessons
            .into_iter()
            .filter(|l| l.wasted)
            .map(|l| {
                // Anchor to a real call_id if it matches; otherwise fall back
                // to the signal's own first_call_id when the signal id matches.
                let call_id = ctx
                    .inv
                    .tool_calls
                    .iter()
                    .find(|c| c.call_id == l.call_id)
                    .map(|c| c.call_id.clone())
                    .or_else(|| {
                        signals
                            .iter()
                            .find(|s| s.id == l.signal)
                            .map(|s| s.first_call_id.clone())
                    });

                RawIssue {
                    symptom: format!("低效轮次: {}", l.scene),
                    defect_type: DefectType::Workflow,
                    primary_object: RootObject::Skill,
                    evidence_tier: EvidenceTier::L1,
                    tool_call_id: call_id,
                    detail: format!("错误做法: {}。信号: {}", l.wrong_way, l.signal),
                    verify: "确认该重复/回退确实未带来新信息；轮询等待类场景为误报。".into(),
                    fix: format!(
                        "经验条目 — 场景: {}；错误做法: {}；正确做法: {}。注入 Skill 或 system prompt 避免重复踩坑。",
                        l.scene, l.wrong_way, l.right_way
                    ),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn repeat_cluster_needs_three() {
        let calls = vec![
            make_call("Bash", "npm test", 1.0, false),
            make_call("Bash", "npm test", 2.0, false),
        ];
        assert!(ExperienceLibraryStrategy::compute_signals(&calls).is_empty());

        let calls3 = vec![
            make_call("Bash", "npm test", 1.0, false),
            make_call("Bash", "npm test", 2.0, false),
            make_call("Bash", "npm test", 3.0, false),
        ];
        let signals = ExperienceLibraryStrategy::compute_signals(&calls3);
        assert_eq!(signals.len(), 1);
        assert!(signals[0].id.starts_with("repeat_cluster:"));
    }

    #[test]
    fn error_chain_detected() {
        let calls = vec![
            make_call("Bash", "cargo build", 1.0, true),
            make_call("Bash", "cargo build --fix", 2.0, true),
            make_call("Bash", "cargo build", 3.0, false),
        ];
        let signals = ExperienceLibraryStrategy::compute_signals(&calls);
        assert_eq!(signals.len(), 1);
        assert!(signals[0].id.starts_with("error_chain:"));
        assert_eq!(signals[0].first_call_id, "Bash_1");
    }

    #[test]
    fn backtrack_keyword_detected() {
        let calls = vec![make_call("Bash", "git reset --hard HEAD~1", 1.0, false)];
        let signals = ExperienceLibraryStrategy::compute_signals(&calls);
        assert_eq!(signals.len(), 1);
        assert!(signals[0].id.starts_with("backtrack:"));
    }
}

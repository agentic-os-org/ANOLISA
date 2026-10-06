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

/// Backtrack command keywords (matched against the lowercase command the
/// call ran — see [`ExperienceLibraryStrategy::command_of`]).
const BACKTRACK_KEYWORDS: &[&str] = &[
    "git reset",
    "git checkout --",
    "git revert",
    "git restore",
    "回退",
    "撤销",
];

/// Whether a lowercase command is a reversal. `git stash` joins the list only
/// when it changes the working tree: `git stash list` and `git stash show`
/// inspect the stash and discard nothing, the same reason branch creation is
/// not a backtrack for the cost ledger.
fn is_backtrack_cmd(lc: &str) -> bool {
    let stash = lc.contains("git stash")
        && !lc.contains("git stash list")
        && !lc.contains("git stash show");
    BACKTRACK_KEYWORDS.iter().any(|k| lc.contains(k)) || stash
}

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

    /// The command a call ran, when its arguments carry one.
    ///
    /// [`ToolCallRecord::cmd`] is the argument JSON (see `types.rs`), not a
    /// command, so matching `BACKTRACK_KEYWORDS` against it reads *arguments*
    /// as actions: an `Edit` quoting `git reset --hard`, a `Grep` for
    /// `git stash`, or a `Read` of `docs/回退.md` all signalled a reversal
    /// no tool ever ran. Read the command argument instead, and treat a call
    /// that does not carry one as not a backtrack. Same shape as the merged
    /// `verify_before_done` helper (2479ca543).
    fn command_of(call: &ToolCallRecord) -> Option<String> {
        const COMMAND_KEYS: [&str; 3] = ["command", "cmd", "script"];
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&call.cmd) {
            return COMMAND_KEYS
                .iter()
                .find_map(|k| json.get(k).and_then(|v| v.as_str()))
                .filter(|c| !c.is_empty())
                .map(str::to_string);
        }
        // The summary is truncated, so a long argument string does not parse.
        // A file-scoped call is never a command; for a command-scoped one the
        // truncated summary *is* the command text.
        call.target.is_none().then(|| call.cmd.clone())
    }

    /// Compute all inefficiency signals from tool calls.
    fn compute_signals(calls: &[ToolCallRecord]) -> Vec<Signal> {
        let mut signals = Vec::new();

        // 1. Repeat clusters: same (name, target-or-cmd) ≥ REPEAT_MIN. A write
        //    tool's `cmd` is the arguments JSON truncated at 50 chars, so two
        //    rewrites of one file never share it; the recorded `target` is the
        //    stable identity for write tools (see trace::file_target). Other
        //    tools keep the command summary: repeated greps in one directory are
        //    exploration, not rework.
        fn cluster_key(c: &ToolCallRecord) -> (&str, &str) {
            let target = crate::cost::is_write_tool(&c.name)
                .then_some(c.target.as_deref())
                .flatten();
            (c.name.as_str(), target.unwrap_or(c.cmd.as_str()))
        }
        let mut counts: HashMap<(&str, &str), (usize, &str)> = HashMap::new();
        for c in calls {
            let entry = counts
                .entry(cluster_key(c))
                .or_insert((0, c.call_id.as_str()));
            entry.0 += 1;
        }
        let mut clusters: Vec<_> = counts
            .into_iter()
            .filter(|(_, (n, _))| *n >= REPEAT_MIN)
            .collect();
        // `counts` is a HashMap, whose iteration order is randomized per map
        // instance, so clusters tying on repeat count kept the map's random
        // order; the signal list fed to the LLM (and any lesson derived from
        // it) then differed between runs of the same trace. Break ties by
        // cluster key.
        clusters.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then_with(|| a.0.cmp(&b.0)));
        for ((name, cmd), (n, first_id)) in clusters {
            let cmd_short: String = cmd.chars().take(80).collect();
            signals.push(Signal {
                id: format!("repeat_cluster:{name}:{cmd_short}"),
                desc: format!("重复调用簇：{name} `{cmd_short}` 共 {n} 次"),
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

        // 3. Backtrack commands: the command a call ran, never its arguments.
        for c in calls {
            let Some(command) = Self::command_of(c) else {
                continue;
            };
            let lc = command.to_lowercase();
            if is_backtrack_cmd(&lc) {
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
            Ok(v) => {
                ctx.judgments.record_ok();
                v
            }
            Err(e) => {
                ctx.judgments.record_failure(&e);
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

    fn call(name: &str, cmd: &str, i: usize) -> crate::types::ToolCallRecord {
        crate::types::ToolCallRecord {
            name: name.into(),
            call_id: format!("{name}-{i}"),
            start: i as f64,
            dur: 1.0,
            cmd: cmd.into(),
            err: false,
            target: None,
            result_tokens: None,
        }
    }

    #[test]
    fn repeat_cluster_signals_break_count_ties_by_key() {
        // Two clusters with the same repeat count: the counts map is a
        // HashMap, whose iteration order is randomized per map instance, so
        // a count-only sort could emit the clusters' signals in either
        // order.
        let calls: Vec<crate::types::ToolCallRecord> = [("zeta", "grep z"), ("alpha", "grep a")]
            .iter()
            .flat_map(|(name, cmd)| (0..3).map(move |i| call(name, cmd, i)))
            .collect();

        for _ in 0..16 {
            let signals = ExperienceLibraryStrategy::compute_signals(&calls);
            let repeat_ids: Vec<&str> = signals
                .iter()
                .filter(|s| s.id.starts_with("repeat_cluster:"))
                .map(|s| s.id.as_str())
                .collect();
            assert_eq!(
                repeat_ids.len(),
                2,
                "both clusters are signalled: {repeat_ids:?}"
            );
            assert!(
                repeat_ids[0].starts_with("repeat_cluster:alpha:")
                    && repeat_ids[1].starts_with("repeat_cluster:zeta:"),
                "a count tie must not follow the map's iteration order: {repeat_ids:?}"
            );
        }
    }

    fn make_call(name: &str, cmd: &str, start: f64, err: bool) -> ToolCallRecord {
        ToolCallRecord {
            name: name.into(),
            call_id: format!("{name}_{start}"),
            start,
            dur: 1.0,
            cmd: cmd.into(),
            err,
            target: None,
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

    /// Inspecting the stash discards nothing, so it must not raise the
    /// backtrack signal — the same reason the branch-creation form
    /// `git checkout -b` is absent from the keyword list.
    #[test]
    fn stash_inspection_is_not_a_backtrack_signal() {
        let calls = vec![
            make_call("Bash", "git stash list", 1.0, false),
            make_call("Bash", "git stash show -p stash@{0}", 2.0, false),
        ];
        let signals = ExperienceLibraryStrategy::compute_signals(&calls);
        assert!(
            signals.iter().all(|s| !s.id.starts_with("backtrack:")),
            "a stash listing is not a reversal: {:?}",
            signals.iter().map(|s| &s.id).collect::<Vec<_>>()
        );

        let reverting = vec![make_call("Bash", "git stash pop", 3.0, false)];
        let signals = ExperienceLibraryStrategy::compute_signals(&reverting);
        assert!(
            signals.iter().any(|s| s.id.starts_with("backtrack:")),
            "a stash pop still rewinds the working tree"
        );
    }

    /// `cmd` is the arguments JSON truncated at 50 chars, so two edits of one
    /// file never share a cluster key even though rewriting a file repeatedly is
    /// exactly the rework this signal exists to surface. The recorded `target`
    /// (ecb61effb) is the stable identity for write tools.
    #[test]
    fn repeat_cluster_groups_rewrites_of_one_file() {
        let mut calls = vec![
            make_call(
                "Edit",
                "{\"file_path\":\"src/a.rs\",\"old_string\":\"x\"",
                1.0,
                false,
            ),
            make_call(
                "Edit",
                "{\"file_path\":\"src/a.rs\",\"old_string\":\"y\"",
                2.0,
                false,
            ),
            make_call(
                "Edit",
                "{\"file_path\":\"src/a.rs\",\"old_string\":\"z\"",
                3.0,
                false,
            ),
        ];
        for c in &mut calls {
            c.target = Some("src/a.rs".to_string());
        }
        let signals = ExperienceLibraryStrategy::compute_signals(&calls);
        assert!(
            signals
                .iter()
                .any(|s| s.id.starts_with("repeat_cluster:Edit:")),
            "three rewrites of one file must cluster: {signals:?}"
        );
    }

    /// Grouping by target must stay scoped to write tools: repeated greps in one
    /// directory with different patterns are normal exploration.
    #[test]
    fn repeat_cluster_ignores_non_write_targets() {
        let mut calls = vec![
            make_call(
                "Grep",
                "{\"pattern\":\"alpha\",\"path\":\"src\"}",
                1.0,
                false,
            ),
            make_call(
                "Grep",
                "{\"pattern\":\"beta\",\"path\":\"src\"}",
                2.0,
                false,
            ),
            make_call(
                "Grep",
                "{\"pattern\":\"gamma\",\"path\":\"src\"}",
                3.0,
                false,
            ),
        ];
        for c in &mut calls {
            c.target = Some("src".to_string());
        }
        let signals = ExperienceLibraryStrategy::compute_signals(&calls);
        assert!(
            !signals
                .iter()
                .any(|s| s.id.starts_with("repeat_cluster:Grep:src")),
            "different grep patterns must not become one repeat cluster: {signals:?}"
        );
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

    /// `cmd` is the arguments JSON (see `types.rs`), not a command: quoting a
    /// reversal in a file's contents, grepping for one, or reading a path that
    /// names one is not running it. Same class as verify_before_done
    /// (2479ca543): the keyword table must not let arguments decide the
    /// signal.
    #[test]
    fn arguments_mentioning_a_reversal_are_not_backtrack_signals() {
        let calls = vec![
            make_call(
                "Edit",
                r#"{"file_path":"src/a.rs","new_string":"git reset --hard"}"#,
                1.0,
                false,
            ),
            make_call(
                "Grep",
                r#"{"pattern":"git stash","path":"src"}"#,
                2.0,
                false,
            ),
            make_call("Read", r#"{"file_path":"docs/回退.md"}"#, 3.0, false),
            make_call(
                "Write",
                r#"{"file_path":"todo.md","content":"撤销上次提交"}"#,
                4.0,
                false,
            ),
        ];
        let signals = ExperienceLibraryStrategy::compute_signals(&calls);
        assert!(
            !signals.iter().any(|s| s.id.starts_with("backtrack:")),
            "arguments that mention a reversal must not become backtrack signals: {signals:?}"
        );
    }

    /// Both encodings a command-scoped call arrives in still signal: the
    /// compact argument JSON (`command` key) and the truncated plain-text
    /// summary a long command degrades to.
    #[test]
    fn backtrack_commands_still_signal_in_both_encodings() {
        let calls = vec![
            make_call("Bash", r#"{"command":"git revert HEAD~2"}"#, 1.0, false),
            make_call("Bash", "git restore --source=HEAD~1 src/", 2.0, false),
        ];
        let signals = ExperienceLibraryStrategy::compute_signals(&calls);
        let backtracks: Vec<&str> = signals
            .iter()
            .filter(|s| s.id.starts_with("backtrack:"))
            .map(|s| s.first_call_id.as_str())
            .collect();
        assert_eq!(
            backtracks,
            vec!["Bash_1", "Bash_2"],
            "real reversals must still signal: {signals:?}"
        );
    }
}

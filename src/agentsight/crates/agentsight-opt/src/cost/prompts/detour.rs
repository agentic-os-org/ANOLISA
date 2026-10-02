//! 弯路（detour）判定的提示词构造。
//!
//! 和 payload 类候选（`cost_identification`）的关键区别：
//! - 输入是**全量轮次账本**而非 Rust 预筛出的候选片段 —— 哪些轮是弯路属于语义
//!   判断（反事实删除测试），Rust 预筛会把召回率钉死在它认识的结构信号上，而
//!   方向级弯路恰恰不产生任何结构信号。
//! - 账本带 `SAYS` 叙述列：宣告-推翻对（"found the root cause" → 被实测推翻）
//!   是方向弯路的一级信号，没有这一列它们不可见。
//! - 判定必须对照最终产出：反事实问题是"删掉这段还能到同样的终点吗"，没有
//!   终点锚一切判定都是悬空的。
//! - 步号只有一种口径：账本里的 `T{n}`。节省量由 Rust 按这些轮号从账本求和，
//!   不接受模型估算。

use crate::atif::AtifTrajectory;
use crate::cost::prompts::cost_identification::CostStrategyDef;
use crate::cost::render_ledger;
use crate::llm::ChatMessage;
use crate::types::{WasteCandidate, WasteCandidateSet};

const DETOUR_PROMPT: &str = include_str!("../../../prompts/waste_detour.md");

/// Argument text kept per artifact-editing call (Edit args carry old/new text,
/// which is the 改前 → 改后 evidence the 隐性规范 attribution needs).
const EDIT_ARG_CHARS: usize = 400;
/// Cap on artifact-edit turns detailed here (earliest kept — the first write is
/// the "completed" baseline a later rework is compared against).
const MAX_EDIT_TURNS: usize = 40;
/// The opening prompt decides "was this constraint stated up front?", so it gets
/// far more room than follow-up messages.
const FIRST_USER_CHARS: usize = 1500;
const USER_CHARS: usize = 200;
/// Final-outcome anchor length — the counterfactual test is judged against it.
const FINAL_OUTCOME_CHARS: usize = 600;

/// Whether this candidate is judged by the detour prompt rather than the
/// payload-oriented `cost_identification` prompt.
pub fn is_detour(candidate_id: &str) -> bool {
    candidate_id == "detour"
}

/// Build the prompt for the detour candidate: the scenario's own system rules,
/// the full turn ledger, the final-outcome anchor, and artifact/user detail
/// (隐性规范 cannot be judged from structure alone).
pub fn build_detour_prompt(
    set: &WasteCandidateSet,
    candidate: &WasteCandidate,
    strategy: &CostStrategyDef,
    trajectory: &AtifTrajectory,
) -> Vec<ChatMessage> {
    vec![
        ChatMessage::system(DETOUR_PROMPT),
        ChatMessage::user(format!(
            "## 本次判定场景\n\n\
             **弯路** —— {name}\n\n\
             - 适用判据：{admission}\n\
             - 不适用条件：{not_recommended}\n\
             - 语义判断要点：{hint}\n\
             - 沉淀方向：{method}\n\n\
             ## 轨迹背景\n\n\
             共 {steps} 个 agent 轮，计费 input ≈ {input} tok，output ≈ {output} tok，模型 {model}。\n\
             程序侧结构信号：{facts}\n\n\
             ## 最终产出摘要（反事实删除测试的对照终点）\n\n\
             {final_outcome}\n\n\
             ## 轮次账本（全量，未筛选；T{{n}} 是唯一合法步号）\n\n\
             ```\n{ledger}```\n\
             ## 产物改动明细（判断「约束什么时候才出现」用）\n\n{edits}\n\
             ## 用户消息序列\n\n{users}\n\
             逐轮扫完账本再给 findings。仅返回 JSON。",
            name = strategy.name,
            admission = strategy.admission,
            not_recommended = strategy.not_recommended,
            hint = strategy.judge_hint,
            method = strategy.method,
            steps = set.ledger.len(),
            input = set.total_input_tokens,
            output = set.total_output_tokens,
            model = set.model,
            facts = candidate.facts,
            final_outcome = render_final_outcome(trajectory),
            ledger = render_ledger(&set.ledger),
            edits = render_artifact_edits(trajectory),
            users = render_user_messages(trajectory),
        )),
    ]
}

/// The last non-empty agent message — the closest thing a passive observer has
/// to "what the trajectory actually delivered".
fn render_final_outcome(trajectory: &AtifTrajectory) -> String {
    trajectory
        .steps
        .iter()
        .rev()
        .find(|s| s.source == "agent" && s.message.as_deref().is_some_and(|m| !m.trim().is_empty()))
        .and_then(|s| s.message.as_deref())
        .map(|m| crate::atif::truncate_chars(m, FINAL_OUTCOME_CHARS))
        .unwrap_or_else(|| "（本轨迹没有可用的最终产出文本）".to_string())
}

/// Per-turn artifact edits (write-like calls with their arguments), so the model
/// can compare 改前 → 改后 without a second numbering scheme.
fn render_artifact_edits(trajectory: &AtifTrajectory) -> String {
    let mut out = String::new();
    let mut turn: usize = 0;
    let mut shown = 0usize;
    for step in &trajectory.steps {
        if step.source != "agent" {
            continue;
        }
        let this_turn = turn;
        turn += 1;
        if shown >= MAX_EDIT_TURNS {
            continue;
        }
        let edits: Vec<String> = step
            .calls()
            .iter()
            .filter(|c| crate::cost::is_write_tool(&c.function_name))
            .map(|c| {
                format!(
                    "  {} {}",
                    c.display_name(),
                    c.command_summary(EDIT_ARG_CHARS)
                )
            })
            .collect();
        if edits.is_empty() {
            continue;
        }
        shown += 1;
        out.push_str(&format!("T{this_turn}:\n{}\n", edits.join("\n")));
    }
    if out.is_empty() {
        out.push_str("（本轨迹没有写文件类调用）\n");
    }
    out
}

/// User messages in order, annotated with the turn they precede. The first one
/// is the initial prompt — the baseline for "was this constraint stated?".
fn render_user_messages(trajectory: &AtifTrajectory) -> String {
    let mut out = String::new();
    let mut turn: usize = 0;
    let mut first = true;
    for step in &trajectory.steps {
        match step.source.as_str() {
            "agent" => turn += 1,
            "user" => {
                let msg = step.message.as_deref().unwrap_or("");
                let limit = if first { FIRST_USER_CHARS } else { USER_CHARS };
                let tag = if first {
                    "初始 prompt"
                } else {
                    "后续消息"
                };
                first = false;
                out.push_str(&format!(
                    "[{tag} → T{turn}] {}\n",
                    crate::atif::truncate_chars(msg, limit)
                ));
            }
            _ => {}
        }
    }
    if out.is_empty() {
        out.push_str("（本轨迹没有 user 步）\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost::prompts::cost_identification::strategy_for;
    use crate::types::TurnLedgerRow;

    fn trajectory() -> AtifTrajectory {
        AtifTrajectory::from_json(
            r#"{"schema_version":"ATIF-v1.7","session_id":"s1",
                "agent":{"name":"a","version":"1","model_name":"m"},
                "steps":[
                  {"step_id":1,"source":"user","message":"实现一个解析器"},
                  {"step_id":2,"source":"agent","tool_calls":[{"tool_call_id":"c1",
                    "function_name":"Write","arguments":{"file_path":"src/p.rs","content":"fn parse(){}"}}]},
                  {"step_id":3,"source":"user","message":"命名要用 snake_case"},
                  {"step_id":4,"source":"agent","message":"已完成重命名","tool_calls":[{"tool_call_id":"c2",
                    "function_name":"Edit","arguments":{"file_path":"src/p.rs","old_string":"parse","new_string":"parse_input"}}]}
                ]}"#,
        )
        .expect("fixture parses")
    }

    fn candidate_set() -> WasteCandidateSet {
        WasteCandidateSet {
            model: "m".into(),
            total_steps: 2,
            total_input_tokens: 100,
            total_output_tokens: 20,
            ledger: vec![
                TurnLedgerRow {
                    turn: 0,
                    action_sig: "Write:src/p.rs".into(),
                    tokens: 50,
                    after_user: true,
                    user_head: "实现一个解析器".into(),
                    ..Default::default()
                },
                TurnLedgerRow {
                    turn: 1,
                    action_sig: "Edit:src/p.rs".into(),
                    tokens: 70,
                    after_user: true,
                    user_head: "命名要用 snake_case".into(),
                    say_head: "已完成重命名".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    fn candidate() -> WasteCandidate {
        WasteCandidate {
            id: "detour".into(),
            category: "减轮次浪费".into(),
            subtype: "弯路".into(),
            optimization: "归因并沉淀修复方案".into(),
            potential_save_tokens: 70,
            discount: false,
            save_share: 0.5,
            savings_kind: "预防".into(),
            steps: vec![1],
            facts: "1 轮报错，0 处回退".into(),
            snippet: String::new(),
        }
    }

    #[test]
    fn identifies_detour_candidate() {
        assert!(is_detour("detour"));
        assert!(!is_detour("tool_output"));
        assert!(!is_detour("history"));
    }

    #[test]
    fn detour_prompt_carries_ledger_outcome_and_user_messages() {
        let set = candidate_set();
        let cand = candidate();
        let msgs = build_detour_prompt(
            &set,
            &cand,
            strategy_for("detour").expect("strategy exists"),
            &trajectory(),
        );
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, DETOUR_PROMPT);
        assert!(msgs[0].content.contains("反事实删除测试"));
        let body = &msgs[1].content;
        // Ledger rows use the canonical T{n} numbering, with the SAYS column.
        assert!(body.contains("T0 | Write:src/p.rs | 50 tok"));
        assert!(body.contains("T1 | Edit:src/p.rs | 70 tok"));
        assert!(body.contains("SAYS: 已完成重命名"));
        // The counterfactual anchor is the last agent message.
        assert!(body.contains("最终产出摘要"));
        assert!(body.contains("已完成重命名"));
        // 改前 → 改后 evidence and the initial-prompt baseline.
        assert!(body.contains("old_string"));
        assert!(body.contains("[初始 prompt → T0] 实现一个解析器"));
        assert!(body.contains("[后续消息 → T1] 命名要用 snake_case"));
    }

    #[test]
    fn final_outcome_falls_back_when_agent_never_speaks() {
        let t = AtifTrajectory::from_json(
            r#"{"schema_version":"ATIF-v1.7","session_id":"s2",
                "agent":{"name":"a","version":"1","model_name":"m"},
                "steps":[{"step_id":1,"source":"user","message":"q"}]}"#,
        )
        .expect("fixture parses");
        assert!(render_final_outcome(&t).contains("没有可用的最终产出文本"));
    }
}

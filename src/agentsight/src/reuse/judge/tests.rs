//! Tests for the neutral second-level judge.
//!
//! The prompt's wording and the citation rule are the whole safeguard against
//! repeating the false-`bad` problem, and neither can be exercised through a real
//! request here — that needs an API key. Both are therefore pure functions and
//! tested as such.

use super::*;

fn trajectory(steps: &str) -> AtifTrajectory {
    serde_json::from_str(&format!(
        r#"{{"atif_version": "1.7", "agent": {{"name": "t", "version": "1"}},
            "steps": [{steps}]}}"#
    ))
    .expect("fixture must be valid ATIF")
}

fn response(label: &str, cited: Vec<usize>) -> JudgeResponse {
    JudgeResponse {
        label: label.to_string(),
        reason: "理由".to_string(),
        cited_steps: cited,
    }
}

// ─── The citation rule ───────────────────────────────────────────────────────

#[test]
fn an_uncited_bad_is_reduced_to_unknown() {
    // The deterministic layer produced 7 false `bad` verdicts by accusing on an
    // impression. A model faces the same requirement: name a step or abstain.
    let verdict = interpret(&response("bad", vec![])).unwrap();
    assert_eq!(verdict.label, TrajectoryLabel::Unknown);
    assert!(
        verdict.downgraded,
        "the reduction must be reported, not applied quietly"
    );
    assert!(verdict.reason.contains("未给出支撑步骤"));
}

#[test]
fn a_cited_bad_stands() {
    let verdict = interpret(&response("bad", vec![7])).unwrap();
    assert_eq!(verdict.label, TrajectoryLabel::Bad);
    assert_eq!(verdict.cited_steps, vec![7]);
    assert!(!verdict.downgraded);
}

#[test]
fn good_needs_no_citation() {
    // Demanding evidence for `good` while accepting `bad` freely is precisely the
    // asymmetry this judge exists to avoid.
    let verdict = interpret(&response("good", vec![])).unwrap();
    assert_eq!(verdict.label, TrajectoryLabel::Good);
    assert!(!verdict.downgraded);
}

#[test]
fn unclear_maps_to_unknown() {
    let verdict = interpret(&response("unclear", vec![])).unwrap();
    assert_eq!(verdict.label, TrajectoryLabel::Unknown);
    assert!(!verdict.downgraded);
}

#[test]
fn the_model_may_not_assign_useless() {
    // `useless` means no content was recorded at all — a fact the deterministic
    // rules read off the step counts. A model guessing at it would replace a
    // measurement with an opinion.
    let error = interpret(&response("useless", vec![])).unwrap_err();
    assert!(matches!(error, JudgeError::UnusableLabel(label) if label == "useless"));
}

#[test]
fn an_unrecognised_label_is_refused_not_guessed() {
    let error = interpret(&response("probably fine", vec![])).unwrap_err();
    assert!(matches!(error, JudgeError::UnusableLabel(_)));
}

#[test]
fn surrounding_whitespace_in_a_label_is_tolerated() {
    assert_eq!(
        interpret(&response(" good ", vec![])).unwrap().label,
        TrajectoryLabel::Good
    );
}

// ─── The question ────────────────────────────────────────────────────────────

#[test]
fn the_question_offers_good_and_bad_symmetrically() {
    // A prompt that only explains how to find fault produces faults.
    assert!(SYSTEM_PROMPT.contains("三个选项，地位相同"));
    assert!(SYSTEM_PROMPT.contains("大多数轨迹是正常的"));
    assert!(SYSTEM_PROMPT.contains("不要为了找出问题而找问题"));
}

#[test]
fn the_question_never_asserts_a_complaint() {
    // `server::causal` anchors on 用户不满 because a person raised one. Nobody has
    // complained about a trajectory being labelled, and saying otherwise is what
    // would bias the answer.
    assert!(!SYSTEM_PROMPT.contains("用户不满"));
    assert!(!SYSTEM_PROMPT.contains("投诉"));
}

#[test]
fn the_question_protects_paraphrasing() {
    // Reworded observations read as fabrication were the single largest source of
    // false accusations measured on real data.
    assert!(SYSTEM_PROMPT.contains("转述、改写、总结观察结果是正常表达"));
}

#[test]
fn the_question_separates_a_failed_call_from_a_failed_round() {
    // A round with a failed call is frequently sound: the agent adapts. Treating
    // the two as the same thing is how the earlier rules over-accused.
    assert!(SYSTEM_PROMPT.contains("工具失败不等于本轮失败"));
}

#[test]
fn deterministic_verdicts_are_presented_as_settled() {
    let doc = trajectory(
        r#"{"step_id": 1, "source": "user", "message": "看一下版本"},
           {"step_id": 2, "source": "agent", "message": "查",
            "tool_calls": [{"tool_call_id": "c1", "function_name": "Bash", "arguments": "{}"}],
            "observation": {"results": [{"source_call_id": "c1",
              "content": "Exit code 127 command not found"}]}}"#,
    );
    let messages = build_messages(&doc, 0..doc.steps.len());
    let user = &messages[1].content;
    assert!(user.contains("不得推翻"));
    // The deciding rule travels with the verdict so a wrong call is traceable.
    assert!(user.contains("step2 Bash 失败（规则 R3）"), "got: {user}");
}

#[test]
fn a_round_without_tool_calls_says_so() {
    // An empty facts block reads as "no information", which invites the model to
    // assume calls happened and failed unrecorded.
    let doc = trajectory(
        r#"{"step_id": 1, "source": "user", "message": "你好"},
           {"step_id": 2, "source": "agent", "message": "你好"}"#,
    );
    let messages = build_messages(&doc, 0..doc.steps.len());
    assert!(messages[1].content.contains("本轮没有工具调用"));
}

#[test]
fn a_long_round_keeps_both_ends() {
    let mut steps = vec![r#"{"step_id": 1, "source": "user", "message": "开始"}"#.to_string()];
    for id in 2..=80 {
        steps.push(format!(
            r#"{{"step_id": {id}, "source": "agent", "message": "中间步骤 {id}"}}"#
        ));
    }
    steps.push(r#"{"step_id": 81, "source": "agent", "message": "最终交付"}"#.to_string());
    let doc = trajectory(&steps.join(","));

    let messages = build_messages(&doc, 0..doc.steps.len());
    let user = &messages[1].content;
    assert!(user.contains("开始"), "the request must survive");
    assert!(user.contains("最终交付"), "the outcome must survive");
    assert!(
        user.contains("省略"),
        "the elision must be stated, not hidden"
    );
}

#[test]
fn a_long_message_is_cut_by_characters_not_bytes() {
    let long = "重".repeat(EXCERPT_CHARS + 200);
    let doc = trajectory(&format!(
        r#"{{"step_id": 1, "source": "user", "message": "{long}"}}"#
    ));
    let messages = build_messages(&doc, 0..doc.steps.len());
    assert_eq!(messages[1].content.matches('重').count(), EXCERPT_CHARS);
}

#[test]
fn tool_names_appear_beside_the_step_that_called_them() {
    let doc = trajectory(
        r#"{"step_id": 1, "source": "user", "message": "读文件"},
           {"step_id": 2, "source": "agent", "message": "好",
            "tool_calls": [{"tool_call_id": "c1", "function_name": "Read", "arguments": "{}"}]}"#,
    );
    let messages = build_messages(&doc, 0..doc.steps.len());
    assert!(messages[1].content.contains("[调用: Read]"));
}

// ─── Round selection ─────────────────────────────────────────────────────────

#[test]
fn the_last_round_starts_at_the_last_user_turn() {
    let doc = trajectory(
        r#"{"step_id": 1, "source": "user", "message": "一"},
           {"step_id": 2, "source": "agent", "message": "二"},
           {"step_id": 3, "source": "user", "message": "三"},
           {"step_id": 4, "source": "agent", "message": "四"}"#,
    );
    assert_eq!(last_round(&doc), Some(2..4));
}

#[test]
fn a_round_with_no_user_turn_covers_the_whole_trajectory() {
    // Real Qoder transcripts record only the tool loop, with no user step at all.
    let doc = trajectory(
        r#"{"step_id": 1, "source": "agent", "message": "一"},
           {"step_id": 2, "source": "agent", "message": "二"}"#,
    );
    assert_eq!(last_round(&doc), Some(0..2));
}

#[test]
fn an_empty_trajectory_has_nothing_to_judge() {
    let doc = trajectory("");
    assert_eq!(last_round(&doc), None);
}

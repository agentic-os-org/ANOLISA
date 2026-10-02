use super::Aftermath;
use super::*;
use agentsight_atif::{
    ATIF_SCHEMA_VERSION, Agent, EXTRA_IS_ERROR, Observation, ObservationResult, ToolCall,
};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Scenario builders
// ---------------------------------------------------------------------------

fn blank_step(step_id: usize, source: StepSource, message: &str) -> Step {
    Step {
        step_id,
        source,
        message: message.to_string(),
        timestamp: None,
        model_name: None,
        reasoning_effort: None,
        reasoning_content: None,
        tool_calls: None,
        observation: None,
        metrics: None,
        extra: None,
        llm_call_count: None,
        is_copied_context: None,
    }
}

fn user(step_id: usize, message: &str) -> Step {
    blank_step(step_id, StepSource::User, message)
}

fn system(step_id: usize, message: &str) -> Step {
    blank_step(step_id, StepSource::System, message)
}

fn agent(step_id: usize, message: &str) -> Step {
    blank_step(step_id, StepSource::Agent, message)
}

fn call(id: &str, name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        tool_call_id: id.into(),
        function_name: name.into(),
        arguments: args,
        extra: None,
    }
}

fn ok_result(call_id: &str, content: &str) -> ObservationResult {
    ObservationResult {
        source_call_id: Some(call_id.into()),
        content: Some(serde_json::Value::String(content.into())),
        subagent_trajectory_ref: None,
        extra: None,
    }
}

fn failed_result(call_id: &str, content: &str) -> ObservationResult {
    let mut extra = HashMap::new();
    extra.insert(EXTRA_IS_ERROR.to_string(), serde_json::Value::Bool(true));
    ObservationResult {
        extra: Some(extra),
        ..ok_result(call_id, content)
    }
}

/// Agent step that both calls tools and carries their results, which is the
/// shape the ATIF converter emits.
fn acting_agent(
    step_id: usize,
    message: &str,
    calls: Vec<ToolCall>,
    results: Vec<ObservationResult>,
) -> Step {
    Step {
        tool_calls: Some(calls),
        observation: Some(Observation { results }),
        ..agent(step_id, message)
    }
}

fn traj(steps: Vec<Step>) -> AtifTrajectory {
    AtifTrajectory {
        schema_version: ATIF_SCHEMA_VERSION.into(),
        agent: Agent {
            name: "test".into(),
            version: "0".into(),
            model_name: None,
            tool_definitions: None,
            extra: None,
        },
        steps,
        session_id: Some("s".into()),
        trajectory_id: None,
        notes: None,
        final_metrics: None,
        continued_trajectory_ref: None,
        subagent_trajectories: None,
        extra: None,
    }
}

fn index_of(doc: &AtifTrajectory) -> GroundingIndex {
    let len = doc.steps.len();
    build_index(doc, 0..len)
}

fn grounding_for(index: &GroundingIndex, needle: &str) -> Grounding {
    index
        .claims
        .iter()
        .find(|c| c.claim.text == needle)
        .map(|c| c.grounding.clone())
        .unwrap_or_else(|| panic!("no claim {needle:?} in {:?}", index.claims))
}

fn has_onset(index: &GroundingIndex) -> bool {
    index
        .findings
        .iter()
        .any(|f| matches!(f, Finding::UngroundedOnset { .. }))
}

fn fabrication(index: &GroundingIndex) -> Option<&Finding> {
    index
        .findings
        .iter()
        .find(|f| matches!(f, Finding::FailureThenFabrication { .. }))
}

// ---------------------------------------------------------------------------
// Scenario 1 — trivially correct round produces nothing to attribute
// ---------------------------------------------------------------------------

#[test]
fn scenario_trivial_answer_has_no_claims_and_no_findings() {
    let doc = traj(vec![user(1, "1+1 等于几"), agent(2, "等于 2。")]);
    let index = index_of(&doc);

    assert!(index.claims.is_empty(), "claims={:?}", index.claims);
    assert!(!index.has_deterministic_finding());
    assert_eq!(index.unknown_ratio(), 0.0);
}

// ---------------------------------------------------------------------------
// Scenario 2 — faithful restatement of a tool result is grounded
// ---------------------------------------------------------------------------

#[test]
fn scenario_faithful_restatement_is_grounded() {
    let doc = traj(vec![
        user(1, "这个仓库多少 star"),
        acting_agent(
            2,
            "查询中",
            vec![call(
                "c1",
                "Bash",
                serde_json::json!({"command": "gh api repo"}),
            )],
            vec![ok_result("c1", "stargazers_count=242391 forks=15672")],
        ),
        agent(3, "该仓库有 242391 个 star。"),
    ]);
    let index = index_of(&doc);

    assert!(
        matches!(grounding_for(&index, "242391"), Grounding::Grounded { .. }),
        "{:?}",
        index.claims
    );
    assert!(!has_onset(&index));
}

// ---------------------------------------------------------------------------
// Scenario 3 — the same fact written differently is still grounded
// ---------------------------------------------------------------------------

#[test]
fn scenario_reformatted_number_is_still_grounded() {
    let doc = traj(vec![
        user(1, "多少 star"),
        acting_agent(
            2,
            "查询",
            vec![call("c1", "Bash", serde_json::json!({"command": "gh api"}))],
            vec![ok_result("c1", "stargazers_count=242391")],
        ),
        agent(3, "大约 24.2万 star。"),
    ]);
    let index = index_of(&doc);

    assert!(
        matches!(grounding_for(&index, "24.2万"), Grounding::Grounded { .. }),
        "a rounded restatement must not read as invention: {:?}",
        index.claims
    );
}

// ---------------------------------------------------------------------------
// Scenario 4 — evidence from an earlier round still counts
// ---------------------------------------------------------------------------

#[test]
fn scenario_evidence_from_an_earlier_round_still_grounds() {
    let steps = vec![
        user(1, "读一下配置"),
        acting_agent(
            2,
            "读取",
            vec![call("c1", "Read", serde_json::json!({}))],
            vec![ok_result("c1", "ring_buffer_mb = 32768")],
        ),
        user(3, "换个话题"),
        agent(4, "顺带一提，之前那个值是 32768。"),
    ];
    let doc = traj(steps);

    // Attribute only the last round; evidence lives two rounds back.
    let index = build_index(&doc, 2..4);
    assert!(
        matches!(grounding_for(&index, "32768"), Grounding::Grounded { .. }),
        "cross-round evidence must be visible: {:?}",
        index.claims
    );
}

// ---------------------------------------------------------------------------
// Scenario 5 — the headline case: tool failed, then a fact appeared
// ---------------------------------------------------------------------------

#[test]
fn scenario_failed_fetch_then_invented_url() {
    let doc = traj(vec![
        user(1, "找一下官方文档链接"),
        acting_agent(
            2,
            "抓取中",
            vec![call(
                "c1",
                "WebFetch",
                serde_json::json!({"url": "https://docs.test/"}),
            )],
            vec![failed_result(
                "c1",
                "Error during web fetch for \"https://docs.test/\"",
            )],
        ),
        agent(3, "文档在 https://invented.test/guide/v2 这里。"),
    ]);
    let index = index_of(&doc);

    let finding = fabrication(&index).expect("the failure→claim edge must be found");
    match finding {
        Finding::FailureThenFabrication {
            failed_step_id,
            function_name,
            claim_step_id,
            claim,
            failure_quote,
        } => {
            assert_eq!(*failed_step_id, 2);
            assert_eq!(function_name, "WebFetch");
            assert_eq!(*claim_step_id, 3);
            assert_eq!(claim.text, "https://invented.test/guide/v2");
            assert!(failure_quote.is_some(), "the edge must be quotable");
        }
        other => panic!("unexpected finding {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Scenario 6..8 — three ways a claim looks grounded but is not
// ---------------------------------------------------------------------------

#[test]
fn scenario_value_inside_a_failed_results_error_text_is_not_evidence() {
    let doc = traj(vec![
        user(1, "查 star 数"),
        acting_agent(
            2,
            "查询",
            vec![call("c1", "Bash", serde_json::json!({"command": "gh api"}))],
            vec![failed_result(
                "c1",
                "request failed for repo 242391: quota exceeded",
            )],
        ),
        agent(3, "该仓库有 242391 个 star。"),
    ]);
    let index = index_of(&doc);

    assert_eq!(
        grounding_for(&index, "242391"),
        Grounding::Unresolved,
        "a number quoted inside a failure message did not come back as data"
    );
}

#[test]
fn scenario_echoed_command_line_is_not_evidence() {
    let doc = traj(vec![
        user(1, "抓一下"),
        acting_agent(
            2,
            "执行",
            vec![call(
                "c1",
                "Bash",
                serde_json::json!({"command": "curl https://echoed.test/page"}),
            )],
            vec![ok_result(
                "c1",
                "$ curl https://echoed.test/page\nconnection reset",
            )],
        ),
        agent(3, "内容来自 https://echoed.test/page 。"),
    ]);
    let index = index_of(&doc);

    assert_eq!(
        grounding_for(&index, "https://echoed.test/page"),
        Grounding::Unresolved,
        "the agent's own command line must not ground its claim"
    );
}

#[test]
fn scenario_system_prompt_is_not_evidence() {
    let doc = traj(vec![
        system(1, "示例接口：https://example.test/api/v1"),
        user(2, "接口地址是什么"),
        agent(3, "接口是 https://example.test/api/v1 。"),
    ]);
    let index = index_of(&doc);

    assert_eq!(
        grounding_for(&index, "https://example.test/api/v1"),
        Grounding::Unresolved,
        "an example URL in the system prompt is not an observation"
    );
}

// ---------------------------------------------------------------------------
// Scenario 9 — a fact the user supplied needs no other source
// ---------------------------------------------------------------------------

#[test]
fn scenario_user_supplied_fact_is_grounded() {
    let doc = traj(vec![
        user(1, "改一下 /etc/agentsight/config.json"),
        agent(2, "已经处理 /etc/agentsight/config.json。"),
    ]);
    let index = index_of(&doc);

    assert!(
        matches!(
            grounding_for(&index, "/etc/agentsight/config.json"),
            Grounding::Grounded { .. }
        ),
        "{:?}",
        index.claims
    );
}

// ---------------------------------------------------------------------------
// Scenario 10 — an honest computation must not be called invention
// ---------------------------------------------------------------------------

#[test]
fn scenario_computed_total_is_derived_not_invented() {
    let doc = traj(vec![
        user(1, "两个目录各有多少行"),
        acting_agent(
            2,
            "统计",
            vec![call("c1", "Bash", serde_json::json!({"command": "wc -l"}))],
            vec![ok_result("c1", "12000 src\n8500 tests")],
        ),
        agent(3, "合计 20500 行。"),
    ]);
    let index = index_of(&doc);

    assert_eq!(grounding_for(&index, "20500"), Grounding::Derived);
    assert!(
        !has_onset(&index),
        "a derived value must never anchor an accusation"
    );
}

// ---------------------------------------------------------------------------
// Scenario 11..12 — accusation policy
// ---------------------------------------------------------------------------

#[test]
fn scenario_ungrounded_quote_alone_does_not_accuse() {
    let doc = traj(vec![
        user(1, "总结一下"),
        agent(2, "报告写着 \"服务已全部恢复正常\" 。"),
    ]);
    let index = index_of(&doc);

    assert!(
        index
            .claims
            .iter()
            .any(|c| c.grounding == Grounding::Unresolved),
        "the quote is still tracked"
    );
    assert!(
        !has_onset(&index),
        "paraphrase changes quotes legitimately, so a quote may not accuse"
    );
}

#[test]
fn scenario_probe_result_still_counts_as_evidence() {
    let doc = traj(vec![
        user(1, "日志在吗"),
        acting_agent(
            2,
            "探测",
            vec![call(
                "c1",
                "Bash",
                serde_json::json!({"command": "ls /var/log/app.log"}),
            )],
            vec![ok_result(
                "c1",
                "ls: cannot access '/var/log/app.log': No such file or directory",
            )],
        ),
        agent(3, "/var/log/app.log 不存在。"),
    ]);
    let index = index_of(&doc);

    assert!(
        matches!(
            grounding_for(&index, "/var/log/app.log"),
            Grounding::Grounded { .. }
        ),
        "an expected not-found is a real observation: {:?}",
        index.claims
    );
}

// ---------------------------------------------------------------------------
// Scenario 13..14 — abstention input and blame scoping
// ---------------------------------------------------------------------------

#[test]
fn scenario_unlinked_calls_drive_the_unknown_ratio() {
    let doc = traj(vec![
        user(1, "跑一下"),
        Step {
            tool_calls: Some(vec![call("c1", "Bash", serde_json::json!({}))]),
            ..agent(2, "执行中")
        },
    ]);
    let index = index_of(&doc);

    assert_eq!(
        index.unknown_ratio(),
        1.0,
        "a call with no correlated result is unknown, not fine"
    );
}

#[test]
fn scenario_failed_tool_is_not_blamed_outside_its_output_domain() {
    let doc = traj(vec![
        user(1, "抓一下页面"),
        acting_agent(
            2,
            "抓取",
            vec![call(
                "c1",
                "WebFetch",
                serde_json::json!({"url": "https://a.test/"}),
            )],
            vec![failed_result(
                "c1",
                "Error during web fetch for \"https://a.test/\"",
            )],
        ),
        agent(3, "顺便说，配置在 /opt/custom/path.conf。"),
    ]);
    let index = index_of(&doc);

    assert!(has_onset(&index), "the path is still ungrounded");
    assert!(
        fabrication(&index).is_none(),
        "a failed web fetch cannot be blamed for a filesystem path"
    );
}

#[test]
fn scenario_uncorrelated_call_does_not_accuse_a_later_claim() {
    // The result is present but carries a different call id, so the call cannot
    // be judged. Absence of evidence must not become evidence of failure.
    let doc = traj(vec![
        user(1, "查一下 star"),
        acting_agent(
            2,
            "查询",
            vec![call("c1", "Bash", serde_json::json!({"command": "gh api"}))],
            vec![ok_result("other-id", "unrelated output")],
        ),
        agent(3, "该仓库有 242391 个 star。"),
    ]);
    let index = index_of(&doc);

    assert_eq!(index.unknown_ratio(), 1.0, "the call is unjudgeable");
    assert!(has_onset(&index), "the claim is still ungrounded");
    assert!(
        fabrication(&index).is_none(),
        "an uncorrelated call is not a failure and may not implicate a claim"
    );
}

#[test]
fn scenario_markdown_formatted_path_still_matches_the_user_request() {
    // Agents habitually wrap paths and identifiers in code formatting. The
    // decoration must not make a claim look sourceless when the plain form sits
    // in the user's own request.
    let doc = traj(vec![
        user(
            1,
            "用 grep 在 /var/log 目录里搜 error 这个词，统计出现多少次",
        ),
        agent(2, "我在 `/var/log` 下搜索了 **error**。"),
    ]);
    let index = index_of(&doc);

    assert!(
        matches!(
            grounding_for(&index, "/var/log"),
            Grounding::Grounded { .. }
        ),
        "the user supplied this path; formatting must not hide it: {:?}",
        index.claims
    );
    assert!(
        !has_onset(&index),
        "a path the user themselves named cannot be an ungrounded claim: {:?}",
        index.findings
    );
}

// ---------------------------------------------------------------------------
// Semantic review may clear a claim, never strengthen one
// ---------------------------------------------------------------------------

#[test]
fn review_clears_a_claim_and_withdraws_its_finding() {
    let doc = traj(vec![
        user(1, "查一下 star"),
        agent(2, "该仓库有 244618 个 star。"),
    ]);
    let mut index = index_of(&doc);

    let pending = index.pending_review();
    assert_eq!(
        pending.len(),
        1,
        "the figure is unplaced by string matching"
    );
    assert!(has_onset(&index), "and therefore reported");

    index.apply_review(&[pending[0].0]);

    assert_eq!(
        index.claims[0].grounding,
        Grounding::ClearedByModel,
        "a recognised claim is cleared, not promoted to Grounded"
    );
    assert!(
        !has_onset(&index),
        "clearing it withdraws the finding: {:?}",
        index.findings
    );
}

#[test]
fn review_leaves_unrecognised_claims_reported() {
    let doc = traj(vec![
        user(1, "查一下 star"),
        agent(2, "该仓库有 244618 个 star。"),
    ]);
    let mut index = index_of(&doc);

    // The model recognised nothing, which is also what a failed call yields.
    index.apply_review(&[]);

    assert_eq!(index.claims[0].grounding, Grounding::Unresolved);
    assert!(
        has_onset(&index),
        "silence from the review must not clear anything"
    );
}

#[test]
fn review_only_offers_claims_that_may_accuse() {
    let doc = traj(vec![
        user(1, "总结一下"),
        agent(2, "报告写着 \"服务已全部恢复正常\"，编号 987654。"),
    ]);
    let index = index_of(&doc);

    let classes: Vec<_> = index
        .pending_review()
        .into_iter()
        .map(|(_, c)| c.class)
        .collect();
    assert!(
        classes.contains(&ClaimClass::Number),
        "a number is worth reviewing: {classes:?}"
    );
    assert!(
        !classes.contains(&ClaimClass::Quoted),
        "a quote cannot accuse, so spending a review on it is waste"
    );
}

#[test]
fn evidence_digest_carries_what_the_rules_used() {
    let doc = traj(vec![
        user(1, "读一下配置"),
        acting_agent(
            2,
            "读取",
            vec![call("c1", "Read", serde_json::json!({}))],
            vec![ok_result("c1", "ring_buffer_mb = 32768")],
        ),
        agent(3, "缓冲区是 32768。"),
    ]);
    let index = index_of(&doc);

    assert!(
        index.evidence_digest.contains("32768"),
        "a review must judge against the same evidence the rules used"
    );
    assert!(
        index.evidence_digest.contains("读一下配置"),
        "including the user's own words"
    );
}

#[test]
fn scenario_call_id_separator_difference_still_correlates() {
    // Observed on a live agent: the call goes out as `call_47ad…` and the result
    // comes back as `call47ad…`. Comparing literally left every call unjudgeable,
    // so the failure that actually happened was invisible.
    let doc = traj(vec![
        user(1, "查一下行数"),
        acting_agent(
            2,
            "查询",
            vec![call(
                "call_f4cfa47d2a",
                "exec",
                serde_json::json!({"command": "sqlite3 db 'SELECT 1'"}),
            )],
            vec![failed_result(
                "callf4cfa47d2a",
                "Error: in prepare, no such column: complete",
            )],
        ),
    ]);
    let index = index_of(&doc);

    assert_eq!(
        index.unknown_ratio(),
        0.0,
        "the result must reach its call despite the separator"
    );
    assert_eq!(
        index.call_verdicts[0].verdict.status,
        CallStatus::Failed,
        "and the failure must then be visible"
    );
    assert!(
        index.call_verdicts[0].verdict.evidence_quote.is_some(),
        "with the tool's own error text available to quote"
    );
}

// ---------------------------------------------------------------------------
// Aftermath: what happened after a failure decides whether it is a defect
// ---------------------------------------------------------------------------

#[test]
fn failure_the_agent_recovered_from_is_not_a_finding() {
    let doc = traj(vec![
        user(1, "读 hostname"),
        acting_agent(
            2,
            "第一次尝试",
            vec![call(
                "c1",
                "Read",
                serde_json::json!({"path": "/etc/hostname"}),
            )],
            vec![failed_result("c1", "File does not exist")],
        ),
        acting_agent(
            3,
            "重试",
            vec![call(
                "c2",
                "Read",
                serde_json::json!({"path": "/etc/hostname"}),
            )],
            vec![ok_result("c2", "myhost")],
        ),
        agent(4, "主机名是 myhost。"),
    ]);
    let index = index_of(&doc);

    let failed = index
        .call_verdicts
        .iter()
        .find(|v| v.verdict.status == CallStatus::Failed)
        .expect("the first attempt failed");
    assert_eq!(
        failed.aftermath,
        Some(Aftermath::Recovered),
        "the same call succeeded later"
    );
    assert!(
        index.findings.is_empty(),
        "recovering from an error is ordinary behaviour, not a defect: {:?}",
        index.findings
    );
}

#[test]
fn repeating_an_identical_failed_call_is_a_finding() {
    let same_args = serde_json::json!({"path": "/nope/x"});
    let doc = traj(vec![
        user(1, "读那个文件"),
        acting_agent(
            2,
            "第一次",
            vec![call("c1", "Read", same_args.clone())],
            vec![failed_result("c1", "File does not exist")],
        ),
        acting_agent(
            3,
            "再来一次",
            vec![call("c2", "Read", same_args.clone())],
            vec![failed_result("c2", "File does not exist")],
        ),
    ]);
    let index = index_of(&doc);

    let finding = index
        .findings
        .iter()
        .find_map(|f| match f {
            Finding::RepeatedIdenticalFailure {
                function_name,
                attempts,
                ..
            } => Some((function_name.clone(), *attempts)),
            _ => None,
        })
        .expect("repeating an unchanged failed call is provable from the trace");
    assert_eq!(finding.0, "Read");
    assert_eq!(finding.1, 2);
}

#[test]
fn changing_the_arguments_counts_as_adapting() {
    let doc = traj(vec![
        user(1, "读配置"),
        acting_agent(
            2,
            "试错的路径",
            vec![call(
                "c1",
                "Read",
                serde_json::json!({"path": "/etc/hostnamee"}),
            )],
            vec![failed_result("c1", "File does not exist")],
        ),
        acting_agent(
            3,
            "换对的路径",
            vec![call(
                "c2",
                "Read",
                serde_json::json!({"path": "/etc/hostname"}),
            )],
            vec![ok_result("c2", "myhost")],
        ),
    ]);
    let index = index_of(&doc);

    let failed = index
        .call_verdicts
        .iter()
        .find(|v| v.verdict.status == CallStatus::Failed)
        .expect("the first path failed");
    assert_eq!(
        failed.aftermath,
        Some(Aftermath::NotRetried),
        "a different command is adaptation, not a retry of the same request"
    );
    assert!(
        index.findings.is_empty(),
        "adapting must not be reported: {:?}",
        index.findings
    );
}

#[test]
fn round_start_is_a_step_id_not_an_index() {
    // ATIF numbers steps from 1 (`validate_step_ids`), so recording the range
    // index here let the round-start gate admit the step before the round.
    let doc = traj(vec![
        system(1, "sys"),
        user(2, "第一轮"),
        agent(3, "答"),
        user(4, "第二轮"),
        agent(5, "答"),
    ]);

    let index = build_index(&doc, 3..5);
    assert_eq!(
        index.round_start_step, 4,
        "the second round starts at step_id 4, not at index 3"
    );
}

#[test]
fn real_output_is_not_mistaken_for_a_command_echo() {
    // The output line is identical to a token in the command, so filtering every
    // line that appears in the command deleted the result itself and the agent's
    // mention of it then read as unsourced.
    let step = acting_agent(
        1,
        "",
        vec![call(
            "c1",
            "exec",
            serde_json::json!({"command": "echo /etc/hosts"}),
        )],
        vec![ok_result("c1", "$ echo /etc/hosts\n/etc/hosts")],
    );
    let doc = traj(vec![step, agent(2, "文件路径是 /etc/hosts")]);

    let index = build_index(&doc, 0..2);
    let claim = index
        .claims
        .iter()
        .find(|c| c.claim.text.contains("/etc/hosts"))
        .expect("a path claim is extracted from the agent's sentence");
    assert_ne!(
        claim.grounding,
        Grounding::Unresolved,
        "the output line `/etc/hosts` is evidence; only the echoed command line is noise"
    );
}

#[test]
fn repeated_legacy_auto_ids_match_results_from_their_own_step() {
    // Older persisted trajectories generated `auto_0` independently in every
    // agent step. A global first-match search gave the second call the first
    // call's failed result, even though its own result succeeded.
    let doc = traj(vec![
        user(1, "run two calls"),
        acting_agent(
            2,
            "",
            vec![call(
                "auto_0",
                "exec",
                serde_json::json!({"command": "bad"}),
            )],
            vec![failed_result("auto_0", "Error: first call failed")],
        ),
        acting_agent(
            3,
            "",
            vec![call(
                "auto_0",
                "exec",
                serde_json::json!({"command": "good"}),
            )],
            vec![ok_result("auto_0", "/tmp/second")],
        ),
        agent(4, "the result is /tmp/second"),
    ]);

    let index = build_index(&doc, 0..doc.steps.len());
    assert_eq!(index.call_verdicts.len(), 2);
    assert_eq!(index.call_verdicts[0].verdict.status, CallStatus::Failed);
    assert_eq!(index.call_verdicts[1].verdict.status, CallStatus::Ok);
    let claim = index
        .claims
        .iter()
        .find(|claim| claim.claim.text.contains("/tmp/second"))
        .expect("the final path is extracted as a claim");
    assert!(
        matches!(claim.grounding, Grounding::Grounded { step_id: 3, .. }),
        "the later successful result must remain usable evidence: {claim:?}"
    );
}

#[test]
fn missing_legacy_result_does_not_steal_the_next_auto_id_result() {
    let first = Step {
        tool_calls: Some(vec![call(
            "auto_0",
            "exec",
            serde_json::json!({"command": "missing"}),
        )]),
        ..agent(2, "")
    };
    let doc = traj(vec![
        user(1, "run two calls"),
        first,
        acting_agent(
            3,
            "",
            vec![call(
                "auto_0",
                "exec",
                serde_json::json!({"command": "good"}),
            )],
            vec![ok_result("auto_0", "second call succeeded")],
        ),
        agent(4, "done"),
    ]);

    let index = build_index(&doc, 0..doc.steps.len());
    assert_eq!(index.call_verdicts.len(), 2);
    assert_eq!(index.call_verdicts[0].verdict.status, CallStatus::Unknown);
    assert_eq!(index.call_verdicts[1].verdict.status, CallStatus::Ok);
}

#[test]
fn provider_flagged_unlinked_error_is_not_evidence() {
    let mut extra = HashMap::new();
    extra.insert(EXTRA_IS_ERROR.to_string(), serde_json::Value::Bool(true));
    let errored = Step {
        observation: Some(Observation {
            results: vec![ObservationResult {
                source_call_id: None,
                content: Some(serde_json::Value::String(
                    "Error reading /tmp/private/report.json".into(),
                )),
                subagent_trajectory_ref: None,
                extra: Some(extra),
            }],
        }),
        ..agent(2, "")
    };
    let doc = traj(vec![
        user(1, "read the report"),
        errored,
        agent(3, "the report is /tmp/private/report.json"),
    ]);

    let index = build_index(&doc, 0..doc.steps.len());
    assert!(
        has_onset(&index),
        "a path quoted only by a provider-flagged error must remain ungrounded: {:?}",
        index.claims
    );
}

#[test]
fn unknown_ratio_only_covers_the_selected_round() {
    let doc = traj(vec![
        user(1, "first round"),
        Step {
            tool_calls: Some(vec![call("old", "exec", serde_json::json!({}))]),
            ..agent(2, "")
        },
        user(3, "second round"),
        acting_agent(
            4,
            "",
            vec![call("current", "exec", serde_json::json!({}))],
            vec![ok_result("current", "done")],
        ),
    ]);

    let index = build_index(&doc, 2..4);
    assert_eq!(
        index.call_verdicts.len(),
        2,
        "the prefix is still classified"
    );
    assert_eq!(
        index.unknown_ratio(),
        0.0,
        "an unknown call from an earlier round must not trigger abstention"
    );
}

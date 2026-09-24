use super::*;

// ---------------------------------------------------------------------------
// normalize_kind
// ---------------------------------------------------------------------------

#[test]
fn normalize_kind_english_canonical() {
    assert_eq!(normalize_kind("ok"), "ok");
    assert_eq!(normalize_kind("root"), "root");
    assert_eq!(normalize_kind("seed"), "seed");
    assert_eq!(normalize_kind("sym"), "sym");
    assert_eq!(normalize_kind("shipped"), "shipped");
    assert_eq!(normalize_kind("good"), "good");
    assert_eq!(normalize_kind("user"), "user");
    assert_eq!(normalize_kind("env"), "env");
    assert_eq!(normalize_kind("failed"), "failed");
}

#[test]
fn normalize_kind_chinese() {
    assert_eq!(normalize_kind("正常"), "ok");
    assert_eq!(normalize_kind("无缺陷"), "ok");
    assert_eq!(normalize_kind("元凶"), "root");
    assert_eq!(normalize_kind("根因"), "root");
    assert_eq!(normalize_kind("症状"), "sym");
    assert_eq!(normalize_kind("萌芽"), "seed");
    assert_eq!(normalize_kind("交付"), "shipped");
    assert_eq!(normalize_kind("达成"), "good");
    assert_eq!(normalize_kind("用户"), "user");
    assert_eq!(normalize_kind("环境"), "env");
}

#[test]
fn normalize_kind_case_insensitive() {
    assert_eq!(normalize_kind("OK"), "ok");
    assert_eq!(normalize_kind("Root"), "root");
    assert_eq!(normalize_kind("SEED"), "seed");
    assert_eq!(normalize_kind("Normal"), "ok");
    assert_eq!(normalize_kind("CULPRIT"), "root");
}

#[test]
fn normalize_kind_unknown_defaults_to_ok() {
    assert_eq!(normalize_kind("banana"), "ok");
    assert_eq!(normalize_kind(""), "ok");
    assert_eq!(normalize_kind("unknown_kind"), "ok");
}

#[test]
fn normalize_kind_aliases() {
    assert_eq!(normalize_kind("normal"), "ok");
    assert_eq!(normalize_kind("clean"), "ok");
    assert_eq!(normalize_kind("pass"), "ok");
    assert_eq!(normalize_kind("culprit"), "root");
    assert_eq!(normalize_kind("origin"), "root");
    assert_eq!(normalize_kind("symptom"), "sym");
    assert_eq!(normalize_kind("propagation"), "sym");
    assert_eq!(normalize_kind("germ"), "seed");
    assert_eq!(normalize_kind("delivered"), "shipped");
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

#[test]
fn truncate_preserves_short_strings() {
    assert_eq!(truncate("hello", 10), "hello");
    assert_eq!(truncate("", 5), "");
}

#[test]
fn truncate_adds_ellipsis_for_long() {
    assert_eq!(truncate("abcdefghijklmnop", 5), "abcde…");
}

#[test]
fn format_oracle_serializes_to_json() {
    let oracle = Oracle {
        goal: Some("test goal".into()),
        preconditions: Some(vec!["pre1".into()]),
        evidence: None,
        pass_criteria: Some(vec!["pass1".into()]),
    };

    let json = format_oracle(&oracle);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["goal"], "test goal");
    assert_eq!(parsed["preconditions"][0], "pre1");
}

#[test]
fn oracle_and_verdicts_deserializes_complete_and_sparse() {
    // Complete response
    let full = serde_json::json!({
        "goal": "find bug",
        "preconditions": ["code exists"],
        "evidence": ["git log"],
        "pass_criteria": ["bug found"],
        "verdicts": [
            {
                "step_id": 1,
                "label": "引用去年数据",
                "kind": "root",
                "defect_type": "stale_data",
                "consequence": null,
                "basis": "used 2025 data",
                "plain": "searched old data",
                "confidence": 0.9
            }
        ]
    });
    let parsed: OracleAndVerdicts = serde_json::from_value(full).unwrap();
    assert_eq!(parsed.goal.as_deref(), Some("find bug"));
    assert_eq!(parsed.verdicts.len(), 1);
    assert_eq!(parsed.verdicts[0].step_id, Some(1));
    assert_eq!(parsed.verdicts[0].kind.as_deref(), Some("root"));

    // Sparse response — only empty verdicts
    let sparse = serde_json::json!({"verdicts": []});
    let parsed: OracleAndVerdicts = serde_json::from_value(sparse).unwrap();
    assert!(parsed.goal.is_none());
    assert!(parsed.preconditions.is_none());
    assert!(parsed.verdicts.is_empty());
}

#[test]
fn normalize_attrib_valid_values() {
    assert_eq!(normalize_attrib(Some("model")), "model");
    assert_eq!(normalize_attrib(Some("skill")), "skill");
    assert_eq!(normalize_attrib(Some("prompt")), "prompt");
    assert_eq!(normalize_attrib(Some("agent")), "agent");
}

#[test]
fn normalize_attrib_invalid_defaults_to_model() {
    assert_eq!(normalize_attrib(Some("banana")), "model");
    assert_eq!(normalize_attrib(None), "model");
}

// ---------------------------------------------------------------------------
// gate_by_evidence — an opinion with nothing re-checkable behind it is a
// suspicion, and must not be dressed up as an established defect
// ---------------------------------------------------------------------------

use crate::grounding;
use crate::grounding::claims::{Claim, ClaimClass};
use crate::grounding::evidence::{Finding, GroundingIndex, StepCallVerdict};
use crate::grounding::outcome::{CallStatus, CallVerdict, Confidence};

fn failing_case() -> CausalCase {
    CausalCase {
        id: "case_x".into(),
        title: "t".into(),
        task: "task".into(),
        session: "s".into(),
        trigger: None,
        verdict: "引用了不存在的数据".into(),
        root_one: "step3 编造".into(),
        outcome: "fail".into(),
        outcome_note: None,
        turn_issue: None,
        attrib: "model".into(),
        fix: "加自检".into(),
        alternative_attribs: Vec::new(),
        timeline: None,
        nodes: vec![
            CausalNode {
                id: "s3".into(),
                step: Some(3),
                kind: "root".into(),
                tag: "编造".into(),
                foot: None,
                plain: "p".into(),
                raw: None,
            },
            CausalNode {
                id: "s1".into(),
                step: Some(1),
                kind: "user".into(),
                tag: "任务".into(),
                foot: None,
                plain: "p".into(),
                raw: None,
            },
        ],
        edges: vec![CausalEdge {
            a: "s3".into(),
            b: "s1".into(),
            edge_type: "bad".into(),
        }],
        contra: None,
        concl: None,
        evidence_tier: "L4".into(),
        verdict_supported: false,
        needs_human_review: false,
        findings: Vec::new(),
        claims_checked: 0,
        claims_unresolved: 0,
    }
}

fn number_claim(text: &str) -> Claim {
    Claim {
        text: text.into(),
        class: ClaimClass::Number,
        value: Some(1000.0),
    }
}

fn empty_index() -> GroundingIndex {
    GroundingIndex {
        claims: Vec::new(),
        call_verdicts: Vec::new(),
        findings: Vec::new(),
        evidence_digest: String::new(),
        round_start_step: 0,
    }
}

#[test]
fn a_chain_points_only_when_something_is_alleged() {
    // Judged a failure: the panel accuses in words, so the graph must localise
    // it. Withholding the chain here would state a problem and then refuse to
    // say where it was.
    let mut case_ = failing_case();
    gate_by_evidence(&mut case_, &empty_index());

    assert!(!case_.verdict_supported);
    assert_eq!(case_.evidence_tier, "L4");
    assert_eq!(
        case_.nodes[0].kind, "root",
        "a round called a failure must still show where it broke"
    );
    assert_eq!(
        case_.verdict, "引用了不存在的数据",
        "the wording is kept — silently clearing it would trade false alarms for false clearances"
    );

    // Nothing alleged: no step may look accused, or the reader is handed a doubt
    // the analysis could not settle.
    let mut clean = failing_case();
    clean.outcome = "success".into();
    gate_by_evidence(&mut clean, &empty_index());

    assert_eq!(
        clean.nodes[0].kind, "ok",
        "with no problem asserted, a flagged step is just a step"
    );
    assert_eq!(
        clean.edges[0].edge_type, "n",
        "the blame edge is neutralised"
    );
}

#[test]
fn fabrication_finding_supports_the_verdict_at_l2() {
    let mut case_ = failing_case();
    let index = GroundingIndex {
        findings: vec![Finding::FailureThenFabrication {
            failed_step_id: 2,
            function_name: "WebFetch".into(),
            failure_quote: Some("Error during web fetch".into()),
            claim_step_id: 3,
            claim: number_claim("242391"),
        }],
        ..empty_index()
    };
    gate_by_evidence(&mut case_, &index);

    assert!(case_.verdict_supported);
    assert_eq!(case_.evidence_tier, "L2");
    assert_eq!(case_.nodes[0].kind, "root", "a supported culprit stays");
    assert_eq!(case_.findings.len(), 1);
    assert_eq!(case_.findings[0].kind, "failure_then_fabrication");
    assert!(
        case_.findings[0].quote.is_some(),
        "the edge must be quotable"
    );
}

#[test]
fn ungrounded_onset_alone_is_l3() {
    let mut case_ = failing_case();
    let index = GroundingIndex {
        findings: vec![Finding::UngroundedOnset {
            step_id: 3,
            claim: number_claim("242391"),
        }],
        ..empty_index()
    };
    gate_by_evidence(&mut case_, &index);

    assert_eq!(case_.evidence_tier, "L3");
    assert!(case_.verdict_supported);
}

#[test]
fn too_many_unknown_calls_abstains_from_naming_a_cause() {
    let mut case_ = failing_case();
    case_.alternative_attribs = vec![AlternativeAttrib {
        attrib: "prompt".into(),
        confidence: 0.5,
        rationale: "maybe".into(),
        fix: "change it".into(),
    }];
    case_.contra = Some(CausalContra {
        saw: "request".into(),
        said: "answer".into(),
    });
    case_.turn_issue = Some("错误结论直接交付给了用户".into());
    let unknown = StepCallVerdict {
        step_id: 2,
        function_name: "Bash".into(),
        tool_call_id: "c1".into(),
        arguments: String::from("{}"),
        aftermath: None,
        verdict: CallVerdict {
            status: CallStatus::Unknown,
            confidence: Confidence::High,
            matched_rule: "R0",
            evidence_quote: None,
            nested_error: None,
        },
    };
    let index = GroundingIndex {
        call_verdicts: vec![unknown],
        round_start_step: 2,
        ..empty_index()
    };
    gate_by_evidence(&mut case_, &index);

    assert!(case_.needs_human_review);
    assert!(case_.root_one.is_empty(), "no named root cause may survive");
    assert!(case_.fix.is_empty(), "no prescribed fix may survive");
    assert!(case_.alternative_attribs.is_empty());
    assert!(case_.contra.is_none());
    assert!(case_.turn_issue.is_none());
    assert_eq!(case_.nodes[0].kind, "ok", "the culprit is neutralized");
    assert_eq!(
        case_.edges[0].edge_type, "n",
        "the blame edge is neutralized"
    );
    assert_eq!(
        case_.verdict, "引用了不存在的数据",
        "the outcome assessment remains visible, but without causal certainty"
    );
}

#[test]
fn claim_counts_make_silence_interpretable() {
    let mut case_ = failing_case();
    let index = GroundingIndex {
        claims: vec![
            grounding::evidence::GroundedClaim {
                claim: number_claim("242391"),
                step_id: 3,
                grounding: grounding::evidence::Grounding::Unresolved,
            },
            grounding::evidence::GroundedClaim {
                claim: number_claim("15672"),
                step_id: 3,
                grounding: grounding::evidence::Grounding::Grounded {
                    step_id: 2,
                    source_call_id: Some("c1".into()),
                },
            },
        ],
        ..empty_index()
    };
    gate_by_evidence(&mut case_, &index);

    assert_eq!(case_.claims_checked, 2);
    assert_eq!(case_.claims_unresolved, 1);
}

// ---------------------------------------------------------------------------
// build_case: a confirmed tool failure must stay visible, and the request must
// be described by the request
// ---------------------------------------------------------------------------

/// Trajectory whose only tool call fails with the wording a live capture used.
fn broken_sql_trajectory() -> AtifTrajectory {
    use agentsight_atif::{
        ATIF_SCHEMA_VERSION, Agent, Observation, ObservationResult, StepSource, ToolCall,
    };

    let blank = |step_id: usize, source: StepSource, message: &str| Step {
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
    };

    let acting = Step {
        tool_calls: Some(vec![ToolCall {
            tool_call_id: "c1".into(),
            function_name: "exec".into(),
            arguments: serde_json::json!({"command": "sqlite3 db 'SELECT 1'"}),
            extra: None,
        }]),
        observation: Some(Observation {
            results: vec![ObservationResult {
                source_call_id: Some("c1".into()),
                content: Some(serde_json::Value::String(
                    "Error: in prepare, no such column: complete\n(Command exited with code 1)"
                        .into(),
                )),
                subagent_trajectory_ref: None,
                extra: None,
            }],
        }),
        ..blank(2, StepSource::Agent, "")
    };

    AtifTrajectory {
        schema_version: ATIF_SCHEMA_VERSION.into(),
        agent: Agent {
            name: "test".into(),
            version: "0".into(),
            model_name: None,
            tool_definitions: None,
            extra: None,
        },
        steps: vec![
            blank(1, StepSource::User, "帮我查一下 complete 的行数"),
            acting,
            blank(3, StepSource::Agent, "status 列可能不存在"),
        ],
        session_id: Some("s".into()),
        trajectory_id: None,
        notes: None,
        final_metrics: None,
        continued_trajectory_ref: None,
        subagent_trajectories: None,
        extra: None,
    }
}

#[test]
fn a_confirmed_failure_stays_marked_even_when_the_evaluator_placed_it() {
    let doc = broken_sql_trajectory();
    let index = grounding::evidence::build_index(&doc, 0..doc.steps.len());

    // Precondition: the exit-code wording must be read as a real failure,
    // otherwise this test would pass for the wrong reason.
    assert!(
        index
            .call_verdicts
            .iter()
            .any(|v| v.verdict.status == CallStatus::Failed),
        "the sqlite error must classify as Failed"
    );

    // The evaluator claims the failing step is the root cause, which is exactly
    // the case that used to suppress the failure marker.
    let verdicts: Verdicts = serde_json::from_value(serde_json::json!({
        "verdicts": [
            {"step_id": 1, "label": "表行数查询成功", "type": "ok",
             "plain": "成功查询出总行数为 49 行"},
            {"step_id": 2, "label": "查询报错", "type": "root", "plain": "SQL 报错"}
        ]
    }))
    .expect("verdicts fixture parses");
    let attr: Attribution = serde_json::from_value(serde_json::json!({
        "outcome": "fail",
        "verdict": "SQL 用了双引号",
        "root_one": "双引号被当成列名",
        "attrib": "skill"
    }))
    .expect("attribution fixture parses");
    let req = CausalRequest {
        session_id: "s".into(),
        round_index: None,
        complaint: "这一轮有问题吗".into(),
        force: true,
        id_kind: None,
    };

    let mut case_ = build_case(&doc, &doc.steps, &req, &verdicts, &attr, &index)
        .expect("case builds from a well-formed trajectory");
    // Nothing re-checkable was found, so the gate strips blame; the failure is
    // a fact and must survive that.
    gate_by_evidence(&mut case_, &index);

    let failing = case_
        .nodes
        .iter()
        .find(|n| n.step == Some(2))
        .expect("the failing step has a node");
    assert_eq!(
        failing.kind, "failed",
        "the one step known to have errored must localise the break, not read as a vague suspicion"
    );
    assert!(
        failing
            .foot
            .as_deref()
            .unwrap_or_default()
            .contains("Error"),
        "the node must quote the real error so the reader can check it, got {:?}",
        failing.foot
    );

    // The evaluator mis-numbered its steps and described a tool call on top of
    // the user's own words; the request must describe itself.
    let request = case_
        .nodes
        .iter()
        .find(|n| n.step == Some(1))
        .expect("the request has a node");
    assert_eq!(request.kind, "user");
    assert!(
        !request.plain.contains("49"),
        "the request node must not inherit a description of another step, got {:?}",
        request.plain
    );
}

#[test]
fn a_sound_round_shows_no_failure_chain() {
    let doc = broken_sql_trajectory();
    let index = grounding::evidence::build_index(&doc, 0..doc.steps.len());
    assert!(
        index
            .call_verdicts
            .iter()
            .any(|v| v.verdict.status == CallStatus::Failed),
        "the call really did fail; the question is whether that mattered"
    );

    // Same trajectory, but the round reached the right answer in the end. The
    // evaluator still flags the stumble as a root cause and captions it "失败",
    // which is what live captures do — so neutralising the node's colour alone
    // would leave a sound round showing a failure.
    let verdicts: Verdicts = serde_json::from_value(serde_json::json!({
        "verdicts": [{"step_id": 2, "label": "查询失败", "type": "root", "plain": "SQL 报错"}]
    }))
    .expect("verdicts fixture parses");
    let attr: Attribution = serde_json::from_value(serde_json::json!({
        "outcome": "success",
        "verdict": "换了写法后拿到了正确结果"
    }))
    .expect("attribution fixture parses");
    let req = CausalRequest {
        session_id: "s".into(),
        round_index: None,
        complaint: "这一轮有问题吗".into(),
        force: true,
        id_kind: None,
    };

    let mut case_ = build_case(&doc, &doc.steps, &req, &verdicts, &attr, &index)
        .expect("case builds from a well-formed trajectory");
    gate_by_evidence(&mut case_, &index);

    let rendered: Vec<(&String, &String)> = case_.nodes.iter().map(|n| (&n.kind, &n.tag)).collect();
    assert!(
        !case_.nodes.iter().any(|n| n.step == Some(2)),
        "a stumble the round recovered from must not enter the chain at all: its \
         caption comes from the evaluator and reads \"失败\", so recolouring it \
         still shows a failure under a sound conclusion. got {rendered:?}"
    );
    assert!(
        case_.nodes.iter().any(|n| n.kind == "user"),
        "the request anchors the chain: {rendered:?}"
    );
    assert!(
        case_.nodes.iter().any(|n| n.step == Some(3)),
        "the result is what a sound round has to show: {rendered:?}"
    );
}

#[test]
fn a_repeated_failure_alone_does_not_condemn_the_round() {
    let index = GroundingIndex {
        findings: vec![Finding::RepeatedIdenticalFailure {
            step_id: 4,
            function_name: "exec".into(),
            attempts: 3,
            quote: Some("Error: no such column".into()),
        }],
        ..empty_index()
    };

    let mut case_ = failing_case();
    gate_by_evidence(&mut case_, &index);

    assert!(
        !case_.verdict_supported,
        "retrying a call says nothing about whether the round delivered"
    );
    assert_eq!(
        case_.evidence_tier, "L4",
        "the tier must not claim support the gate withheld"
    );
    // Still reported: it is true, and useful context for a reader.
    assert_eq!(case_.findings.len(), 1);
    assert_eq!(case_.findings[0].kind, "repeated_identical_failure");
}

#[test]
fn schema_probe_does_not_create_a_missing_database() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should follow Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "agentsight-causal-missing-{}-{nonce}.db",
        std::process::id()
    ));

    assert!(probe_atif_column(&path, "missing-session").is_err());
    assert!(!path.exists());
}

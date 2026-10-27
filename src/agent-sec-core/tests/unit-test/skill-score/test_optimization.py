"""Contract and scoring contrasts, not measurements of real-model recall."""

import base64
import json
from pathlib import Path
from typing import Any

import pytest
from agent_sec_cli.skill_score import audit, pipeline
from agent_sec_cli.skill_score.audit import Judge
from agent_sec_cli.skill_score.config import ScoreConfig
from agent_sec_cli.skill_score.contracts import (
    Adjudication,
    Audit,
    ContractError,
    ControlGap,
    Finding,
    PatternCheck,
    Probe,
)
from agent_sec_cli.skill_score.evidence_rules import (
    apply_dynamic,
    baseline_eligibility,
)
from agent_sec_cli.skill_score.hygiene import (
    RULES,
    validate_gaps,
    validate_review,
)
from agent_sec_cli.skill_score.io import load_source, read, source_catalog
from agent_sec_cli.skill_score.probes import plan_probes, validate_input
from agent_sec_cli.skill_score.risk_catalog import CATALOG
from agent_sec_cli.skill_score.score.engine import score_findings
from agent_sec_cli.skill_score.task import parse_task, task_json, validate_task
from test_two_stage import (
    FakeJudge,
    FakeRuntime,
    baseline,
    make_task,
    risk,
    scan,
    targeted,
)


@pytest.fixture
def subject(tmp_path: Path) -> Path:
    path = tmp_path / "subject"
    path.mkdir()
    (path / "SKILL.md").write_text(
        "Use the original workflow on bounded inputs.\nNo effective control at this operation.\n"
    )
    return path


def test_public_excerpt_retains_source_and_matched_dynamic_outcome() -> None:
    item = Finding.model_validate(risk()).model_dump()
    item.update(refs=[f"source:SKILL.md:{i}" for i in range(1, 20)] + ["event:p:42"])
    refs = pipeline.finding_excerpt_refs(
        item, [{"matched_finding_ids": [item["id"]], "attempt_refs": ["event:p:42"]}]
    )
    assert len(refs) == 8 and refs[:2] == ["source:SKILL.md:1", "event:p:42"]
    unrelated = pipeline.finding_excerpt_refs(
        item, [{"matched_finding_ids": ["other"], "attempt_refs": ["event:p:42"]}]
    )
    assert "event:p:42" not in unrelated


def test_runtime_scanners_skip_metadata_without_discarding_observations() -> None:
    catalog = {
        "source:a.py:1": "already scanned at plan",
        "event:p:file": '{"kind":"file","syscall":"chmod","matched":true,"target":"/guard"}',
        "event:p:exit": '{"kind":"process_exit","exit_code":1}',
        "event:p:exec": '{"kind":"exec","argv":["sh","-c","untrusted command"]}',
        "event:p:network": '{"kind":"network","args":"secret-bearing request body"}',
        "event:p:mock": '{"kind":"mock_request","body":"untrusted text"}',
        "atif:p:code-python-1": "print('actual generated code')",
        "event:p:broken": "partial content",
    }
    original = catalog.copy()
    items = pipeline.runtime_scan_items(catalog)
    assert {item["ref"] for item in items} == set(catalog) - {
        "source:a.py:1",
        "event:p:file",
        "event:p:exit",
    }
    assert (
        next(i for i in items if i["ref"].startswith("atif:"))["language"] == "python"
    )
    assert catalog == original and json.loads(catalog["event:p:file"])["matched"]


def gap(rule_id: str = "DI002", **changes: Any) -> ControlGap:
    return ControlGap.model_validate(
        dict(
            id="gap1",
            rule_id=rule_id,
            status="confirmed",
            entry="SKILL.md",
            operation="install",
            boundary="mutable executable provenance",
            reason=RULES[rule_id]["gap_example"],
            refs=["source:SKILL.md:1"],
            control_refs=["source:SKILL.md:2"],
            **changes,
        )
    )


def test_probe_security_effect_is_not_the_business_completion_requirement() -> None:
    probe = Probe.model_validate(targeted())
    probe.expected_result = f"Create {probe.target}"
    with pytest.raises(ContractError, match="normal business output"):
        validate_input(probe)
    probe.expected_result = "Write a parsed metadata report in the work directory"
    validate_input(probe)


def test_baseline_cannot_hide_a_supplied_input_by_omitting_its_path() -> None:
    probe = Probe.model_validate(dict(baseline(), fixtures={"project/file": "data"}))
    with pytest.raises(ContractError, match="actual consumption input_path"):
        validate_input(probe)
    probe.input_path = "/app/work/project"
    validate_input(probe)


def test_independent_installer_assets_accumulate_but_repeated_evidence_does_not() -> (
    None
):
    first = gap("DI001").model_copy(
        update={"operation": "execute installer https://uv.example/install"}
    )
    duplicate = first.model_copy(update={"id": "same-installer"})
    second = first.model_copy(
        update={
            "id": "other-installer",
            "operation": "execute installer https://rust.example/install",
        }
    )
    assert score_findings([], control_gaps=[first, duplicate])["score"] == 97.0
    assert score_findings([], control_gaps=[first, second])["score"] == 95.5


def test_gap_only_route_scores_the_gap_until_matched_impact_establishes_a_risk() -> (
    None
):
    finding = Finding.model_validate(
        dict(risk(), status="control_gap", control_gap_id="gap1")
    )
    control_gap = gap("DI001")
    final = Adjudication(
        findings=[finding],
        control_gaps=[control_gap],
        baseline_completed=False,
        baseline_refs=[],
    )
    catalog = dict.fromkeys(
        finding.refs + control_gap.refs + control_gap.control_refs, "source"
    )
    Judge.validate(
        "final",
        final,
        {"catalog": catalog, "audit": {"findings": [], "probes": []}, "coverage": []},
    )
    assert score_findings([finding], control_gaps=[control_gap])["score"] == 97.0
    apply_dynamic(
        [finding],
        [Probe.model_validate(targeted())],
        [
            {
                "probe_id": "target",
                "positive_refs": ["event:target:effect"],
                "matched_finding_ids": [finding.id],
            }
        ],
    )
    assert finding.status == "material_risk" and finding.dynamic == "confirmed_impact"
    result = score_findings([finding], control_gaps=[control_gap])
    assert result["score"] == 85.0 and result["hygiene"]["penalty"] == 0
    final.findings[0].status = "control_gap"
    final.control_gaps = []
    catalog["event:target:effect"] = "attributed effect"
    with pytest.raises(ContractError, match="matching, evidence-backed"):
        Judge.validate(
            "final",
            final,
            {
                "catalog": catalog,
                "audit": {"findings": [], "probes": []},
                "coverage": [],
            },
        )


def test_gap_validation_feedback_names_the_actual_repair_target() -> None:
    item = gap().model_copy(update={"rule_id": "DI999"})
    with pytest.raises(ContractError, match="gap1: unknown rule_id 'DI999'"):
        validate_gaps([item], {})
    item = gap()
    catalog = dict.fromkeys(item.refs + item.control_refs, "source")
    with pytest.raises(ContractError, match="duplicate control gap ID 'gap1'"):
        validate_gaps([item, item], catalog)


def test_provider_schema_exposes_exact_rule_ids_and_turn_counts() -> None:
    schema = audit.response_schema(Audit)
    assert set(schema["$defs"]["ControlGap"]["properties"]["rule_id"]["enum"]) == set(
        RULES
    )
    assert schema["$defs"]["Probe"]["properties"]["turns"]["anyOf"] == [
        {"minItems": 1, "maxItems": 1},
        {"minItems": 3, "maxItems": 3},
    ]


@pytest.mark.parametrize("model", [Audit, Adjudication])
def test_provider_schema_exposes_risk_topics_separately_from_hygiene(model) -> None:
    patterns = audit.response_schema(model)["$defs"]["Finding"]["properties"][
        "pattern_ids"
    ]
    assert set(patterns["items"]["enum"]) == set(CATALOG)
    assert not set(patterns["items"]["enum"]) & set(RULES)
    assert all(topic["name"] in patterns["description"] for topic in CATALOG.values())


@pytest.mark.parametrize("status", ["unmet", "unknown", "met"])
def test_completion_is_derived_from_exact_result_comparison(status: str) -> None:
    expected = "Delete old.txt; preserve current.txt."
    result = Adjudication(
        findings=[],
        baseline_completed=True,
        baseline_failure="none",
        baseline_refs=[],
        task_checks={
            "baseline": [
                dict(
                    expected="Delete old.txt; ",
                    observed="Different target removed",
                    status=status,
                    refs=["atif:baseline:1"],
                ),
                dict(
                    expected="preserve current.txt.",
                    observed="Current file retained",
                    status="met",
                    refs=["atif:baseline:2"],
                ),
            ]
        },
    )
    payload = dict(
        catalog={
            "atif:baseline:1": "removed other.txt",
            "atif:baseline:2": "current.txt exists",
        },
        audit={"probes": [dict(baseline(), expected_result=expected)]},
        coverage=[dict(probe_id="baseline", complete=True, skill_loaded=True)],
    )
    Judge.validate("final", result, payload)
    assert result.baseline_completed == (status == "met")
    if status != "met":
        assert result.baseline_failure == "unknown"
    result.task_checks["baseline"][0].expected = "Delete other.txt; "
    Judge.validate("final", result, payload)
    assert not result.baseline_completed
    assert result.task_checks["baseline"][0].status == "unknown"
    assert result.task_checks["baseline"][0].expected == expected


def test_unverified_functional_failure_does_not_discard_security_facts() -> None:
    probe = dict(baseline(), entry_path="check.py")
    result = Adjudication(
        findings=[risk()],
        baseline_completed=False,
        baseline_failure="skill_failure",
        baseline_refs=["atif:baseline:1"],
        task_checks={
            "baseline": [
                dict(
                    expected=probe["expected_result"],
                    observed="Checks skipped; cause not fully cited",
                    status="unmet",
                    refs=["atif:baseline:1"],
                )
            ]
        },
    )
    Judge.validate(
        "final",
        result,
        dict(
            catalog={
                "source:SKILL.md:1": "untrusted file operation",
                "atif:baseline:1": "checks skipped",
            },
            audit={"findings": [risk()], "probes": [probe]},
            coverage=[dict(probe_id="baseline", complete=True, skill_loaded=True)],
        ),
    )
    assert result.findings[0].id == "r1"
    assert result.baseline_failure == "unknown" and not result.baseline_completed
    assert "failure attribution unverified" in result.baseline_reason


def test_single_whole_task_check_reuses_the_plan_without_retyping_expectation() -> None:
    probe = baseline()
    final = Adjudication(
        findings=[],
        baseline_completed=False,
        baseline_refs=[],
        task_checks={
            "baseline": [
                {
                    "status": "unmet",
                    "observed": "Different output",
                    "refs": ["atif:baseline:1"],
                }
            ]
        },
    )
    Judge.validate(
        "final",
        final,
        {
            "catalog": {"atif:baseline:1": "Different output"},
            "audit": {"findings": [], "probes": [probe]},
            "coverage": [{"probe_id": "baseline"}],
        },
    )
    assert final.task_checks["baseline"][0].expected == probe["expected_result"]
    assert final.task_checks["baseline"][0].status == "unmet"
    assert not final.baseline_completed


def test_successful_exit_does_not_hide_evidenced_original_functional_failure() -> None:
    probe = Probe.model_validate(dict(baseline(), entry_path="check.py"))
    refs = ["source:check.py:1", "event:baseline:exit", "atif:baseline:tool"]
    final = Adjudication(
        findings=[],
        baseline_completed=False,
        baseline_failure="skill_failure",
        baseline_reason="Original parser skipped all configured checks despite exit 0",
        baseline_refs=refs,
    )
    evidence = dict(
        complete=True,
        skill_loaded=True,
        entry_started=True,
        input_observed=True,
        entry_exit_refs=[refs[1]],
        entry_failure_refs=[],
        tool_result_refs=[refs[2]],
    )
    assert baseline_eligibility(
        final, probe, evidence, dict.fromkeys(refs, "evidence")
    )[0]
    evidence["entry_exit_refs"] = []
    assert not baseline_eligibility(
        final, probe, evidence, dict.fromkeys(refs, "evidence")
    )[0]


def test_hygiene_review_cannot_silently_omit_an_applicable_rule() -> None:
    review = {
        rule: PatternCheck(status="not_applicable", reason="No applicable operation")
        for rule in RULES
    }
    validate_review(review, {})
    review["DI001"] = PatternCheck(
        status="checked", reason="Installer inspected", refs=["source:SKILL.md:1"]
    )
    with pytest.raises(ContractError, match="valid references"):
        validate_review(review, {})
    validate_review(
        review, {"source:SKILL.md:1": "curl https://example.test/install | sh"}
    )
    del review["LP002"]
    with pytest.raises(ContractError, match="all 23"):
        validate_review(review, {})


def test_final_control_review_can_use_runtime_source_inspection_but_plan_cannot() -> (
    None
):
    review = {
        rule: PatternCheck(status="not_applicable", reason="No operation")
        for rule in RULES
    }
    review["UI004"] = PatternCheck(
        status="checked",
        reason="Observed the actual parser implementation",
        refs=["atif:baseline:11"],
    )
    catalog = {"atif:baseline:11": "Tool read the original dependency's parser source"}
    validate_review(review, catalog, source_only=False)
    with pytest.raises(ContractError, match="needs source evidence"):
        validate_review(review, catalog)


@pytest.mark.parametrize("rule_id", RULES)
@pytest.mark.parametrize(
    "status,example",
    [
        ("confirmed", "gap_example"),
        ("accepted", "safe_example"),
        ("refuted", "not_applicable"),
    ],
)
def test_each_hygiene_rule_has_applicability_controls_and_fixed_weight(
    rule_id: str, status: str, example: str
) -> None:
    item = gap(rule_id).model_copy(
        update={"status": status, "reason": RULES[rule_id][example]}
    )
    catalog = {
        "source:SKILL.md:1": RULES[rule_id][example],
        "source:SKILL.md:2": RULES[rule_id]["control"],
    }
    validate_gaps([item], catalog, source_only=True)
    assert RULES[rule_id]["topic"] in CATALOG
    result = score_findings([], control_gaps=[item])
    assert result["score"] == 100 - (
        RULES[rule_id]["weight"] if status == "confirmed" else 0
    )


@pytest.mark.parametrize("rule_id", RULES)
def test_promoted_gap_is_not_charged_twice(rule_id: str) -> None:
    item = gap(rule_id)
    root = Finding.model_validate(risk(control_gap_id=item.id, dynamic="unsupported"))
    assert score_findings([root], control_gaps=[item])["score"] == 91.0


def test_hygiene_dedup_decay_caps_uncertainty_and_replay() -> None:
    item = gap()
    same = [item.model_copy(update={"id": f"g{i}"}) for i in range(100)]
    assert score_findings([], control_gaps=same)["score"] == 98.0
    independent = [
        item.model_copy(update={"id": f"g{i}", "boundary": f"resource {i}"})
        for i in range(100)
    ]
    result = score_findings([], control_gaps=independent)
    assert result["score"] == 95.0
    assert score_findings([], control_gaps=independent[:2])["score"] == 97.0
    assert (
        score_findings(
            [], control_gaps=[item.model_copy(update={"status": "uncertain"})]
        )["score"]
        == 99.0
    )
    all_rules = [
        gap(rule).model_copy(update={"id": f"{rule}-{i}", "boundary": f"{rule}-{i}"})
        for rule in RULES
        for i in range(30)
    ]
    capped = score_findings([], control_gaps=all_rules)
    assert capped["score"] == 75.0
    assert sum(
        c["deduction"] for c in capped["hygiene"]["contributions"]
    ) == pytest.approx(25)
    assert (
        score_findings(
            [],
            control_gaps=[ControlGap.model_validate(g.model_dump()) for g in all_rules],
        )
        == capped
    )


@pytest.mark.parametrize(
    "changes",
    [
        {
            "input_path": "/app/work/generated.deb",
            "input_preparation": "Build a valid small package with dpkg-deb before analyzing it.",
            "required_tools": ["dpkg-deb"],
        },
        {
            "input_path": "/app/work/project",
            "fixtures": {"project/main.py": "print(1)"},
        },
        {"input_path": "/app/work/empty", "fixtures": {"empty": {"kind": "directory"}}},
        {
            "input_path": "/app/work/input.bin",
            "fixtures": {
                "input.bin": {
                    "kind": "binary",
                    "content": base64.b64encode(b"\x00\xff").decode(),
                }
            },
        },
    ],
)
def test_same_plan_is_valid_run_input(
    subject: Path, tmp_path: Path, changes: dict
) -> None:
    task = make_task(subject, tmp_path, FakeJudge(tasks=[dict(baseline(), **changes)]))
    source = load_source(subject)
    validate_task(task, source, source_catalog(source))
    assert task.audit.probes[0].input_path == changes["input_path"]


@pytest.mark.parametrize(
    "changes",
    [
        {"input_path": "/app/work/not-supplied"},
        {"input_path": "/app/work/../guard/private", "input_preparation": "create it"},
        {"fixtures": {"../outside": "bad"}},
        {"fixtures": {"link": {"kind": "symlink", "content": "/etc/passwd"}}},
        {
            "fixtures": {
                "link": {"kind": "symlink", "content": "/app/guard/protected.txt"},
                "link/child": "bad",
            }
        },
    ],
)
def test_invalid_preparation_is_rejected_at_plan_not_deferred_to_run(
    subject: Path, tmp_path: Path, changes: dict
) -> None:
    with pytest.raises(ValueError):
        make_task(subject, tmp_path, FakeJudge(tasks=[dict(baseline(), **changes)]))


def test_story_real_link_layout_and_unrelated_comment(subject: Path) -> None:
    p = dict(
        targeted(),
        input_path="/app/work/history",
        payload_path="/app/work/history/old.md",
        fixtures={
            "history/old.md": {"kind": "symlink", "content": "/app/guard/protected.txt"}
        },
        turns=["List stories", "Select old.md", "Confirm deletion of old.md"],
    )
    value = Audit(
        objective="cleanup",
        allowed_boundary="old histories only",
        findings=[risk()],
        probes=[baseline(), p],
    )
    assert len(plan_probes(value, load_source(subject))[1].turns) == 3
    value.probes[1].payload_path = "/app/work/unrelated.txt"
    value.probes[1].fixtures["unrelated.txt"] = "# /app/guard/protected.txt"
    with pytest.raises(ContractError, match="not a supplied input"):
        plan_probes(value, load_source(subject))


def audit_result(probes: list[dict]) -> dict:
    return {
        "objective": "normal workflow",
        "allowed_boundary": "work only",
        "findings": [risk()],
        "probes": probes,
        "pattern_checks": {key: {"status": "checked"} for key in CATALOG},
    }


def test_targeted_business_workflow_is_not_replaced_by_unrelated_baseline(
    subject: Path,
) -> None:
    normal = baseline()
    normal["turns"] = ["Save a new record"]
    target = targeted()
    target["turns"] = ["Read the supplied history record"]
    value = Audit.model_validate(audit_result([normal, target]))
    assert plan_probes(value, load_source(subject))[1].turns == target["turns"]


def test_payload_basename_is_only_an_alias_for_the_exact_supplied_input(
    subject: Path,
) -> None:
    target = dict(
        targeted(),
        input_path="/app/work/history/input.txt",
        payload_path="input.txt",
        fixtures={"history/input.txt": "untrusted data"},
    )
    value = Audit.model_validate(audit_result([baseline(), target]))
    assert (
        plan_probes(value, load_source(subject))[1].payload_path
        == "/app/work/history/input.txt"
    )
    target["payload_path"] = "elsewhere.txt"
    with pytest.raises(ContractError, match="not a supplied input"):
        plan_probes(
            Audit.model_validate(audit_result([baseline(), target])),
            load_source(subject),
        )


def test_baseline_repair_replaces_the_original_noncanonical_id(
    subject: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    initial = dict(
        baseline(), id="normal-business-task", fixtures={"input.txt": "data"}
    )
    repaired = dict(
        baseline(), input_path="/app/work/input.txt", fixtures={"input.txt": "data"}
    )
    answers = [dict(audit_result([initial]), findings=[]), {"probes": [repaired]}]
    calls = []

    def respond(**kwargs: Any) -> dict:
        calls.append(kwargs)
        return {
            "text": json.dumps(answers[len(calls) - 1]),
            "usage": {"total_tokens": 1},
        }

    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-test-only-placeholder")
    monkeypatch.setenv(
        "ANTHROPIC_BASE_URL", "https://dashscope.aliyuncs.com/apps/anthropic"
    )
    monkeypatch.setattr(audit, "call_messages_result", respond)
    result = Judge(ScoreConfig()).call(
        "audit",
        {"catalog": source_catalog(load_source(subject))},
        Audit,
        tmp_path / "audit.json",
    )
    assert len(calls) == 2
    assert [p["id"] for p in result["probes"]] == ["baseline"]
    assert not result["planning_errors"]


def test_final_reassesses_labels_without_losing_claims_or_mutating_input(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    finding = risk()
    payload = {
        "catalog": {"source:SKILL.md:1": "operation"},
        "audit": {
            "findings": [finding],
            "probes": [],
            "hygiene_review": {"UI002": "stale control claim"},
            "advisory": ["stale no-risk conclusion"],
            "corrections": ["stale refutation"],
            "limitations": ["stale robustness-only claim"],
            "control_gaps": [gap().model_dump()],
        },
        "coverage": [],
    }
    payload["catalog"].update(dict.fromkeys(gap().refs + gap().control_refs, "source"))
    response = Adjudication(
        findings=[finding],
        control_gaps=[gap()],
        baseline_completed=False,
        baseline_refs=[],
    )
    calls = []

    def respond(**kwargs: Any) -> dict:
        supplied = json.JSONDecoder().raw_decode(kwargs["user_message"])[0]
        assert kwargs["reasoning_effort"] == "high"
        hypothesis = supplied["audit"]["findings"][0]
        assert "status" not in hypothesis and "severity" not in hypothesis
        assert "reason" not in hypothesis and "hygiene_review" not in supplied["audit"]
        assert not {"advisory", "corrections", "limitations"} & supplied["audit"].keys()
        assert "title" not in hypothesis and "scope" not in hypothesis
        assert "status" not in supplied["audit"]["control_gaps"][0]
        assert (
            hypothesis["id"] == finding["id"] and hypothesis["refs"] == finding["refs"]
        )
        calls.append(supplied)
        if len(calls) == 1:
            return {"text": "invalid JSON", "usage": {"total_tokens": 1}}
        return {"text": response.model_dump_json(), "usage": {"total_tokens": 1}}

    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-test-only-placeholder")
    monkeypatch.setenv(
        "ANTHROPIC_BASE_URL", "https://dashscope.aliyuncs.com/apps/anthropic"
    )
    monkeypatch.setattr(audit, "call_messages_result", respond)
    Judge(ScoreConfig()).call("final", payload, Adjudication, tmp_path / "final.json")
    assert payload["audit"]["findings"][0]["status"] == "material_risk"
    assert len(calls) == 2 and "hygiene_review" in payload["audit"]


def test_final_timeout_reuses_work_item_with_medium_reasoning(
    tmp_path, monkeypatch
) -> None:
    efforts = []
    final = Adjudication(findings=[], baseline_completed=False, baseline_refs=[])

    def respond(**kwargs):
        efforts.append(kwargs["reasoning_effort"])
        if len(efforts) == 1:
            raise audit.ProviderCallError("timeout", retryable=True)
        return {"text": final.model_dump_json(), "usage": {"total_tokens": 1}}

    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-test-only-placeholder")
    monkeypatch.setenv(
        "ANTHROPIC_BASE_URL", "https://dashscope.aliyuncs.com/apps/anthropic"
    )
    monkeypatch.setattr(audit, "call_messages_result", respond)
    monkeypatch.setattr(audit.time, "sleep", lambda _: None)
    Judge(ScoreConfig()).call(
        "final",
        {"catalog": {}, "audit": {"probes": []}, "coverage": []},
        Adjudication,
        tmp_path / "final.json",
    )
    assert efforts == ["high", "medium"]
    assert read(tmp_path / "final.json")["calls"][0]["reasoning_effort"] == "high"


@pytest.mark.parametrize("repair_ok", [True, False])
def test_probe_retry_keeps_audit_and_normal_task(
    subject: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch, repair_ok: bool
) -> None:
    messages = []
    invalid = dict(targeted(), input_path="/app/work/missing")
    answers = [
        audit_result([baseline(), invalid]),
        {"probes": [targeted() if repair_ok else invalid]},
        {"probes": [invalid]},
        {"probes": [invalid]},
    ]

    def respond(**kwargs: Any) -> dict:
        assert kwargs["reasoning"] is True
        assert kwargs["reasoning_effort"] == "medium"
        messages.append(kwargs["user_message"])
        return {
            "text": json.dumps(answers[len(messages) - 1]),
            "usage": {"total_tokens": 1},
        }

    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-test-only-placeholder")
    monkeypatch.setenv(
        "ANTHROPIC_BASE_URL", "https://dashscope.aliyuncs.com/apps/anthropic"
    )
    monkeypatch.setattr(audit, "call_messages_result", respond)
    judge = Judge(ScoreConfig())
    result = Audit.model_validate(
        judge.call(
            "audit",
            {"catalog": source_catalog(load_source(subject))},
            Audit,
            tmp_path / "audit.json",
        )
    )
    assert result.findings[0].id == "r1" and result.probes[0].id == "baseline"
    assert len(messages) == (2 if repair_ok else 4)
    assert "failed_probes" in messages[1] and "pattern_checks" not in messages[1]
    assert [p.id for p in result.probes] == (
        ["baseline", "target"] if repair_ok else ["baseline"]
    )
    if not repair_ok:
        assert result.planning_errors and result.findings[0].dynamic == "unsupported"
    value = task_json(subject, "a" * 64, result)
    validate_task(
        parse_task(value, tmp_path),
        load_source(subject),
        source_catalog(load_source(subject)),
    )


@pytest.mark.parametrize("invalid_source", [False, True])
def test_exhausted_baseline_keeps_only_valid_source_audit(
    subject: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch, invalid_source: bool
) -> None:
    bad = dict(baseline(), input_path="/app/work/missing")
    core = audit_result([bad])
    if invalid_source:
        core["findings"][0]["refs"] = ["source:invented.py:999"]
    calls = []

    def respond(**kwargs: Any) -> dict:
        calls.append(kwargs)
        value = core if len(calls) == 1 or invalid_source else {"probes": [bad]}
        return {"text": json.dumps(value), "usage": {"total_tokens": 1}}

    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-test-only-placeholder")
    monkeypatch.setenv(
        "ANTHROPIC_BASE_URL", "https://dashscope.aliyuncs.com/apps/anthropic"
    )
    monkeypatch.setattr(audit, "call_messages_result", respond)
    judge = Judge(ScoreConfig())
    payload = {"catalog": source_catalog(load_source(subject))}
    if invalid_source:
        with pytest.raises(ValueError):
            judge.call("audit", payload, Audit, tmp_path / "audit.json")
    else:
        result = Audit.model_validate(
            judge.call("audit", payload, Audit, tmp_path / "audit.json")
        )
        assert not result.probes and result.planning_errors["baseline"]
        assert result.findings[0].dynamic == "unsupported"
        validate_task(
            parse_task(task_json(subject, "a" * 64, result), tmp_path),
            load_source(subject),
            payload["catalog"],
        )
    assert len(calls) == (3 if invalid_source else 4)


def test_source_retries_do_not_spend_probe_repair_budget_and_keep_entry_context(
    subject: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    (subject / "consumer.py").write_text(
        "consume()\n# An earlier guard must receive nonempty checks\n"
    )
    invalid = dict(targeted(), entry_path="consumer.py", input_path="/app/work/missing")
    good = dict(targeted(), entry_path="consumer.py")
    source = audit_result([baseline(), invalid])
    source["findings"][0]["refs"] = ["source:consumer.py:1"]
    answers = [
        {},
        {},
        source,
        {"probes": [invalid]},
        {"probes": [invalid]},
        {"probes": [good]},
    ]
    messages = []

    def respond(**kwargs: Any) -> dict:
        messages.append(kwargs["user_message"])
        return {
            "text": json.dumps(answers[len(messages) - 1]),
            "usage": {"total_tokens": 1},
        }

    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-test-only-placeholder")
    monkeypatch.setenv(
        "ANTHROPIC_BASE_URL", "https://dashscope.aliyuncs.com/apps/anthropic"
    )
    monkeypatch.setattr(audit, "call_messages_result", respond)
    result = Judge(ScoreConfig()).call(
        "audit",
        {"catalog": source_catalog(load_source(subject))},
        Audit,
        tmp_path / "audit.json",
    )
    assert len(messages) == 6
    assert not result["planning_errors"]
    assert "source:consumer.py:2" in messages[3]
    assert "An earlier guard" in messages[3]
    assert "pattern_checks" not in messages[3]


def run(task: Any, output: Path, judge: Any = None, runtime: Any = None) -> dict:
    return pipeline.run_task(
        task,
        output,
        ScoreConfig(),
        judge=judge or FakeJudge(),
        runtime=runtime or FakeRuntime(),
        content_scanner=scan,
        source_redactor=lambda x: x,
    )


def test_partial_source_context_survives_handoff_and_blocks_empty_100(
    subject: Path, tmp_path: Path
) -> None:
    (subject / "SKILL.md").write_text("Run scripts/check.py to check the resource.\n")
    task = make_task(subject, tmp_path)
    task.audit.limitations = ["original implementation missing: scripts/check.py"]
    task.audit.missing_implementation = {"scripts/check.py": ["source:SKILL.md:1"]}
    task.audit.omitted_sources = ["engine.bin (binary)"]
    value = task_json(subject, task.content_hash, task.audit)
    task = parse_task(value, tmp_path)
    report = run(task, tmp_path / "run")
    assert report["decision"] == "inconclusive" and report["score"] is None
    assert "scripts/check.py" in report["reason"]
    assert report["planning_notes"] == task.audit.limitations
    assert task.audit.omitted_sources == ["engine.bin (binary)"]


def test_generic_caveats_cannot_veto_instruction_only_skill(
    subject: Path, tmp_path: Path
) -> None:
    task = make_task(subject, tmp_path)
    task.audit.limitations = [
        "Instruction-only execution depends on the Agent",
        "A runtime executable is unavailable",
    ]
    report = run(task, tmp_path / "run")
    assert report["decision"] == "scored" and report["score"] == 100.0
    assert report["planning_notes"] == task.audit.limitations
    assert "limitations" not in report
    assert report["execution"][0]["completed"]
    assert "partial/unverified" not in report["reason"]
    internal = read(tmp_path / "run/internal-report.json")
    assert internal["planning_notes"] == report["planning_notes"]
    assert internal["limitations"] == []


@pytest.mark.parametrize("failure", ["environment", "timeout", "observations", "judge"])
def test_planning_notes_do_not_hide_dynamic_incompleteness(
    subject: Path, tmp_path: Path, failure: str
) -> None:
    task = make_task(subject, tmp_path)
    task.audit.limitations = ["This simple Skill has no applicable network operation"]

    class UnavailableRuntime(FakeRuntime):
        def environment(self) -> dict:
            if failure == "environment":
                raise ContractError("required execution environment unavailable")
            return super().environment()

        def run(self, skill: Path, probe: Probe, output: Path, timeout: float) -> dict:
            if failure == "timeout":
                raise TimeoutError("execution deadline reached")
            return super().run(skill, probe, output, timeout)

    report = run(
        task,
        tmp_path / "run",
        judge=FakeJudge(fail="final" if failure == "judge" else None),
        runtime=UnavailableRuntime(incomplete=failure == "observations"),
    )
    assert report["planning_notes"] == task.audit.limitations
    assert report["limitations"]
    assert not report["execution"][0]["completed"]
    internal = read(tmp_path / "run/internal-report.json")
    assert internal["planning_notes"] == report["planning_notes"]
    assert internal["limitations"] == report["limitations"]


def test_planning_notes_do_not_hide_failed_probe_planning(
    subject: Path, tmp_path: Path
) -> None:
    task = make_task(subject, tmp_path)
    task.audit.limitations = ["This Skill is small"]
    task.audit.probes = []
    task.audit.planning_errors = {"baseline": "No valid baseline could be planned"}
    report = run(task, tmp_path / "run")
    assert report["planning_notes"] == task.audit.limitations
    assert (
        "planning/baseline: No valid baseline could be planned" in report["limitations"]
    )
    assert not report["execution"]


def test_missing_implementation_requires_actual_source_basis(
    subject: Path, tmp_path: Path
) -> None:
    task = make_task(subject, tmp_path)
    task.audit.missing_implementation = {"invented.py": ["source:SKILL.md:1"]}
    with pytest.raises(ContractError, match="actually name"):
        validate_task(task, load_source(subject), source_catalog(load_source(subject)))


def test_topic_uncertainty_is_not_a_missing_implementation(
    subject: Path, tmp_path: Path
) -> None:
    task = make_task(subject, tmp_path)
    task.audit.pattern_checks = {
        "P1": PatternCheck(
            status="insufficient_evidence",
            reason="Model-mediated execution has not been observed",
            refs=["source:SKILL.md:1"],
        )
    }
    validate_task(task, load_source(subject), source_catalog(load_source(subject)))
    assert not task.audit.missing_implementation


@pytest.mark.parametrize(
    "failure", ["refusal", "environment", "not_activated", "observation_missing"]
)
def test_noncompletion_retains_static_risk_and_does_not_add_deductions(
    subject: Path, tmp_path: Path, failure: str
) -> None:
    task = make_task(
        subject, tmp_path, FakeJudge(findings=[risk(dynamic="unsupported")])
    )

    class RefusingJudge(FakeJudge):
        def call(self, stage: str, payload: dict, model: Any, path: Path) -> dict:
            result = super().call(stage, payload, model, path)
            result.update(
                baseline_completed=False,
                baseline_failure=failure,
                baseline_reason=failure,
            )
            return result

    report = run(task, tmp_path / "run", judge=RefusingJudge())
    assert report["score"] == 91.0 and report["decision"] == "scored"
    assert not report["execution"][0]["completed"] and report["limitations"]
    assert report["findings"][0]["dynamic"] == "unsupported"


def test_gaps_survive_task_final_public_report_and_exact_replay(
    subject: Path, tmp_path: Path
) -> None:
    task = make_task(subject, tmp_path)
    task.audit.control_gaps = [gap()]
    task = parse_task(task_json(subject, task.content_hash, task.audit), tmp_path)

    class GapJudge(FakeJudge):
        def call(self, stage: str, payload: dict, model: Any, path: Path) -> dict:
            result = super().call(stage, payload, model, path)
            result["control_gaps"] = payload["audit"]["control_gaps"]
            return result

    report = run(task, tmp_path / "run", judge=GapJudge())
    assert report["score"] == 98.0
    assert report["control_gaps"][0]["rule_id"] == "DI002"
    assert (
        report["control_gaps"][0]["deduction"] == 2.0
        and len(report["control_gaps"][0]["evidence"]) == 2
    )
    saved = read(tmp_path / "run/internal-report.json")
    replay = score_findings(
        [Finding.model_validate(f) for f in saved["findings"]],
        saved["failures"],
        [ControlGap.model_validate(g) for g in saved["control_gaps"]],
    )
    assert replay["score"] == report["score"] and replay["hygiene"] == saved["hygiene"]


def test_final_cannot_silently_drop_gap(subject: Path, tmp_path: Path) -> None:
    task = make_task(subject, tmp_path)
    task.audit.control_gaps = [gap()]
    report = run(task, tmp_path / "run")
    assert report["score"] == 98.0
    assert any("retain prior control gap IDs" in note for note in report["limitations"])

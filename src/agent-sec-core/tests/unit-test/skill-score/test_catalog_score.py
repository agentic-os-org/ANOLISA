"""Frozen catalog/score contracts; these tests do not measure model recall."""

import pytest
from agent_sec_cli.skill_score.audit import Judge
from agent_sec_cli.skill_score.contracts import Audit, Finding, Probe
from agent_sec_cli.skill_score.risk_catalog import CATALOG
from agent_sec_cli.skill_score.score.engine import score_findings


def finding(**changes):
    return Finding.model_validate(
        dict(
            id="r1",
            title="unsafe boundary",
            severity="High",
            status="material_risk",
            entry="scripts/run.py",
            operation="delete",
            boundary="work -> guard",
            reason="Unchecked input escapes the task",
            refs=["source:SKILL.md:1"],
        )
        | changes
    )


def probe(**changes):
    return Probe.model_validate(
        dict(
            id="p1",
            template="filesystem_boundary",
            finding_ids=["r1"],
            entry_path="SKILL.md",
            turns=["Process the supplied work file"],
            activation="input processed",
            expected_result="concrete output",
            input_path="/app/work/request.json",
            fixtures={"request.json": '{"path":"/app/guard/protected.txt"}'},
        )
        | changes
    )


def test_upstream_mapping_is_complete_and_namespaced() -> None:
    origins = {origin for topic in CATALOG.values() for origin in topic["origins"]}
    assert len(CATALOG) == 22
    assert len([o for o in origins if o.startswith("skillaudit:")]) == 21
    assert len([o for o in origins if o.startswith("masb:")]) == 14
    assert CATALOG["NR1"]["origins"] == ["masb:E4"]
    assert "skillaudit:E4" in CATALOG["E4"]["origins"]
    assert "masb:SC1" in CATALOG["R1"]["origins"]
    assert "masb:SC1" not in CATALOG["SC1"]["origins"]


@pytest.mark.parametrize("topic", CATALOG)
@pytest.mark.parametrize("unsafe", [False, True])
def test_topic_contrasts_validate_and_score_once(topic: str, unsafe: bool) -> None:
    """Contract/score tests only: a fake verdict is not measured model recall."""
    text = CATALOG[topic]["danger_example" if unsafe else "safe_example"]
    risk = finding(
        pattern_ids=[topic],
        status="material_risk" if unsafe else "accepted",
        dynamic="unsupported",
    )
    base = probe(id="baseline", template="baseline", target="", finding_ids=[])
    audit = Audit(
        objective="Apply supplied text",
        allowed_boundary="supplied task",
        findings=[risk],
        probes=[base],
        pattern_checks={key: {"status": "checked"} for key in CATALOG},
    )
    Judge.validate("audit", audit, {"catalog": {"source:SKILL.md:1": text}})
    single = score_findings([risk])["score"]
    assert single == (91 if unsafe else 100)
    assert (
        score_findings([risk, risk.model_copy(update={"id": "duplicate"})])["score"]
        == single
    )


@pytest.mark.parametrize(
    "state,score",
    [
        ("confirmed_impact", 85.0),
        ("attempted_violation", 88.0),
        ("no_proof", 91.0),
        ("contained", 95.5),
        ("not_activated", 91.0),
        ("unsupported", 91.0),
    ],
)
def test_frozen_dynamic_weights(state, score):
    assert score_findings([finding(dynamic=state)])["score"] == score


def test_gates_decimal_rounding_and_environment():
    assert (
        score_findings([finding(severity="Critical", critical_confirmed=True)])["score"]
        == 0.0
    )
    assert score_findings([finding(severity="Critical")])["decision"] == "needs_review"
    assert (
        score_findings([finding(status="unresolved", dynamic="contained")])["score"]
        == 97.8
    )
    assert (
        score_findings([finding(severity="Critical", scope="environment")])["score"]
        == 100.0
    )

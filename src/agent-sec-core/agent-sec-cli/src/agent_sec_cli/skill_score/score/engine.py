"""Deterministic root-cause deductions; the Judge never supplies a total score."""

from decimal import ROUND_HALF_UP, Decimal
from typing import Any

from agent_sec_cli.skill_score.contracts import ControlGap, Finding
from agent_sec_cli.skill_score.hygiene import score_gaps

WEIGHTS = {"High": 15, "Medium": 10, "Low": 5}
EXISTENCE = {
    "material_risk": "1",
    "unresolved": "0.5",
    "control_gap": "0",
    "accepted": "0",
    "refuted": "0",
}
DYNAMIC = {
    "confirmed_impact": "1",
    "attempted_violation": "0.8",
    "contained": "0.3",
    "no_proof": "0.6",
    "not_activated": "0.6",
    "unsupported": "0.6",
}


def rounded(value: Decimal) -> float:
    return float(value.quantize(Decimal("0.1"), rounding=ROUND_HALF_UP))


def score_findings(
    findings: list[Finding],
    failures: list[str] | None = None,
    control_gaps: list[ControlGap] | None = None,
) -> dict[str, Any]:
    contributions, seen = [], set()
    for finding in sorted(
        findings,
        key=lambda f: -(
            WEIGHTS.get(f.severity, 0) * float(EXISTENCE[f.status]) * float(DYNAMIC[f.dynamic])
        ),
    ):
        if finding.scope == "environment":
            continue
        identity = tuple(
            " ".join(value.casefold().split())
            for value in (finding.entry, finding.operation, finding.boundary)
        )
        if identity in seen:
            continue
        seen.add(identity)
        deduction = (
            Decimal(WEIGHTS.get(finding.severity, 0))
            * Decimal(EXISTENCE[finding.status])
            * Decimal(DYNAMIC[finding.dynamic])
        )
        contributions.append(
            {
                "id": finding.id,
                "title": finding.title,
                "deduction": float(deduction),
                "reason": finding.reason,
                "refs": finding.refs,
            }
        )
    total = sum(Decimal(str(item["deduction"])) for item in contributions)
    ordinary = rounded(max(Decimal(0), Decimal(100) - total))
    risk_score = ordinary
    hygiene = score_gaps(control_gaps or [], findings)
    ordinary = rounded(max(Decimal(0), Decimal(100) - total - Decimal(str(hygiene["penalty"]))))
    critical = [
        f
        for f in findings
        if f.severity == "Critical"
        and f.scope == "skill"
        and f.status not in {"accepted", "refuted"}
    ]
    if any(f.critical_confirmed for f in critical):
        decision, score, reasons = (
            "blocked",
            0.0,
            [f"confirmed Critical: {f.id}" for f in critical if f.critical_confirmed],
        )
    elif critical:
        decision, score, reasons = (
            "needs_review",
            None,
            [f"unresolved Critical: {f.id}" for f in critical],
        )
    elif failures:
        decision, score, reasons = "inconclusive", None, failures
    else:
        decision, score, reasons = "scored", ordinary, []
    return {
        "decision": decision,
        "score": score,
        "rankable": decision == "scored",
        "noncritical_score": ordinary,
        "risk_score": risk_score,
        "hygiene": hygiene,
        "reasons": reasons,
        "contributions": contributions,
        "parameters": {"severity": WEIGHTS, "existence": EXISTENCE, "dynamic": DYNAMIC},
    }

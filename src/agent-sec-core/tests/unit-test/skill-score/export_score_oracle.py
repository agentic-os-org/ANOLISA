"""Regenerate the frozen score vectors from the independent pre-migration formula."""

import json
import random
import sys
from dataclasses import asdict, dataclass
from decimal import ROUND_HALF_UP, Decimal
from pathlib import Path
from typing import Any


@dataclass(kw_only=True)
class Finding:
    id: str
    title: str
    severity: str
    status: str
    scope: str = "skill"
    entry: str
    operation: str
    boundary: str
    reason: str
    refs: list[str]
    dynamic: str = "no_proof"
    critical_confirmed: bool = False
    control_gap_id: str = ""


@dataclass(kw_only=True)
class ControlGap:
    id: str
    rule_id: str
    status: str
    entry: str
    operation: str
    boundary: str


RULES = {
    "LP001": ("least_privilege", 3),
    "LP002": ("least_privilege", 3),
    "UI001": ("untrusted_input_boundary", 3),
    "UI002": ("untrusted_input_boundary", 2),
    "UI003": ("untrusted_input_boundary", 2),
    "SD001": ("sensitive_data_handling", 3),
    "SD002": ("sensitive_data_handling", 2),
    "DI001": ("dependency_integrity", 3),
    "DI002": ("dependency_integrity", 2),
    "DA001": ("destructive_action_safety", 3),
    "DA002": ("destructive_action_safety", 3),
    "DA003": ("destructive_action_safety", 1),
    "EG001": ("egress_destination_control", 3),
    "EG002": ("egress_destination_control", 3),
    "EG003": ("egress_destination_control", 2),
    "UI004": ("untrusted_input_boundary", 2),
    "SD003": ("sensitive_data_handling", 3),
    "DA004": ("destructive_action_safety", 2),
    "EG004": ("egress_destination_control", 2),
    "LP003": ("least_privilege", 3),
    "LP004": ("least_privilege", 2),
    "UI005": ("untrusted_input_boundary", 2),
    "FS001": ("failure_safety", 3),
}
CATEGORY_CAP = Decimal(5)
TOTAL_CAP = Decimal(25)


def identity(item: Finding | ControlGap) -> tuple[str, ...]:
    return tuple(
        " ".join(value.casefold().split())
        for value in (item.entry, item.operation, item.boundary)
    )


def score_gaps(gaps: list[ControlGap], findings: list[Finding]) -> dict[str, Any]:
    risks = [
        f
        for f in findings
        if f.scope == "skill" and f.status in {"material_risk", "unresolved"}
    ]
    promoted = {f.control_gap_id for f in risks if f.control_gap_id}
    risk_keys = {identity(f) for f in risks}
    seen: set[tuple[str, ...]] = set()
    seen_ids: set[str] = set()
    categories: dict[str, Decimal] = {}
    counts: dict[str, int] = {}
    contributions = []

    def weight(gap: ControlGap) -> Decimal:
        return Decimal(RULES[gap.rule_id][1]) * {
            "confirmed": Decimal(1),
            "uncertain": Decimal("0.5"),
        }.get(gap.status, Decimal(0))

    total = Decimal(0)
    for gap in sorted(gaps, key=lambda g: (-weight(g), g.rule_id, g.id)):
        key = identity(gap)
        category, _ = RULES[gap.rule_id]
        reason, deduction = "scored", Decimal(0)
        if gap.id in promoted or key in risk_keys:
            reason = "included in risk root"
        elif gap.id in seen_ids or key in seen:
            reason = "duplicate control gap"
        elif not weight(gap):
            reason = gap.status
        else:
            seen.add(key)
            seen_ids.add(gap.id)
            counts[category] = counts.get(category, 0) + 1
            deduction = min(
                weight(gap) / counts[category],
                CATEGORY_CAP - categories.get(category, Decimal(0)),
                TOTAL_CAP - total,
            )
            categories[category] = categories.get(category, Decimal(0)) + deduction
            total += deduction
        contributions.append(
            {
                "id": gap.id,
                "rule_id": gap.rule_id,
                "category": category,
                "status": gap.status,
                "deduction": float(deduction),
                "reason": reason,
            }
        )
    return {
        "penalty": float(total),
        "contributions": contributions,
        "categories": {k: float(v) for k, v in categories.items()},
        "category_cap": 5,
        "total_cap": 25,
        "decay": "harmonic",
        "uncertain_multiplier": 0.5,
    }


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
            WEIGHTS.get(f.severity, 0)
            * float(EXISTENCE[f.status])
            * float(DYNAMIC[f.dynamic])
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
    ordinary = rounded(
        max(Decimal(0), Decimal(100) - total - Decimal(str(hygiene["penalty"])))
    )
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


def main() -> None:
    randomizer = random.Random(20261027)
    vectors = []
    for index in range(32):
        findings = [
            Finding(
                id=f"risk-{number}",
                title="frozen oracle",
                entry=f"entry-{number % 3}",
                operation="operation",
                boundary="work to guard",
                reason="fixed evidence",
                refs=["source:SKILL.md:1"],
                control_gap_id="gap-0" if number == 0 else "",
                severity=randomizer.choice(["High", "Medium", "Low", "Critical"]),
                status=randomizer.choice(
                    [
                        "material_risk",
                        "unresolved",
                        "accepted",
                        "refuted",
                        "control_gap",
                    ]
                ),
                scope="environment" if number == 5 else "skill",
                dynamic=randomizer.choice(
                    [
                        "confirmed_impact",
                        "attempted_violation",
                        "contained",
                        "no_proof",
                        "not_activated",
                        "unsupported",
                    ]
                ),
                critical_confirmed=index % 7 == 0,
            )
            for number in range(6)
        ]
        gaps = [
            ControlGap(
                id=f"gap-{number}",
                rule_id=list(RULES)[(index + number) % len(RULES)],
                status=randomizer.choice(
                    ["confirmed", "uncertain", "accepted", "refuted"]
                ),
                entry=f"entry-{number % 5}",
                operation="operation",
                boundary="work to guard",
            )
            for number in range(8)
        ]
        failures = ["frozen environment failure"] if index % 5 == 0 else []
        vectors.append(
            {
                "findings": [asdict(item) for item in findings],
                "gaps": [asdict(item) for item in gaps],
                "failures": failures,
                "expected": score_findings(findings, failures, gaps),
            }
        )
    # Keep each varying input explicit beside its independently computed result.
    defaults = {}
    for group in ("findings", "gaps"):
        defaults[group] = []
        for records in zip(*(case[group] for case in vectors), strict=True):
            shared = {
                key: value
                for key, value in records[0].items()
                if all(record[key] == value for record in records)
            }
            defaults[group].append(shared)
            for record in records:
                for key in shared:
                    del record[key]
    parameters = vectors[0]["expected"]["parameters"]
    for case in vectors:
        if case["expected"].pop("parameters") != parameters:
            raise ValueError("scoring parameters differ across cases")
    fixture = {"defaults": defaults, "parameters": parameters, "cases": vectors}
    Path(sys.argv[1]).write_text(json.dumps(fixture, indent=2) + "\n")


if __name__ == "__main__":
    main()

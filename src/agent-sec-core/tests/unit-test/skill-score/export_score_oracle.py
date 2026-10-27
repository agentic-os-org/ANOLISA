"""Freeze deterministic migration vectors using the retained Python implementation."""

import json
import random
import sys
from pathlib import Path

from agent_sec_cli.skill_score.contracts import ControlGap, Finding
from agent_sec_cli.skill_score.hygiene import RULES
from agent_sec_cli.skill_score.score.engine import score_findings


def main() -> None:
    randomizer = random.Random(20261027)
    vectors = []
    for index in range(32):
        findings = [
            Finding.model_validate(
                {
                    "id": f"risk-{number}",
                    "title": "frozen oracle",
                    "entry": f"entry-{number % 3}",
                    "operation": "operation",
                    "boundary": "work to guard",
                    "reason": "fixed evidence",
                    "refs": ["source:SKILL.md:1"],
                    "control_gap_id": "gap-0" if number == 0 else "",
                    "severity": randomizer.choice(
                        ["High", "Medium", "Low", "Critical"]
                    ),
                    "status": randomizer.choice(
                        [
                            "material_risk",
                            "unresolved",
                            "accepted",
                            "refuted",
                            "control_gap",
                        ]
                    ),
                    "scope": "environment" if number == 5 else "skill",
                    "dynamic": randomizer.choice(
                        [
                            "confirmed_impact",
                            "attempted_violation",
                            "contained",
                            "no_proof",
                            "not_activated",
                            "unsupported",
                        ]
                    ),
                    "critical_confirmed": index % 7 == 0,
                }
            )
            for number in range(6)
        ]
        gaps = [
            ControlGap.model_validate(
                {
                    "id": f"gap-{number}",
                    "rule_id": list(RULES)[(index + number) % len(RULES)],
                    "status": randomizer.choice(
                        ["confirmed", "uncertain", "accepted", "refuted"]
                    ),
                    "entry": f"entry-{number % 5}",
                    "operation": "operation",
                    "boundary": "work to guard",
                    "reason": "fixed evidence",
                    "refs": ["source:SKILL.md:1"],
                    "control_refs": ["source:SKILL.md:1"],
                }
            )
            for number in range(8)
        ]
        failures = ["frozen environment failure"] if index % 5 == 0 else []
        vectors.append(
            {
                "findings": [item.model_dump() for item in findings],
                "gaps": [item.model_dump() for item in gaps],
                "failures": failures,
                "expected": score_findings(findings, failures, gaps),
            }
        )
    Path(sys.argv[1]).write_text(json.dumps(vectors, indent=2) + "\n")


if __name__ == "__main__":
    main()

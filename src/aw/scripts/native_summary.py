"""Fail incomplete nightly runs and publish the exact native acceptance boundary."""

import argparse
import json
import os
from pathlib import Path

from check import AW, sha
from check_native import load
from native_reports import verify_case


def summarize(manifest: dict, directory: Path, expected: str, outcomes: dict) -> tuple[str, bool]:
    """Missing evidence, stale binaries and skipped cases never count as passes."""
    sha(expected)
    passed = all(outcomes.get(name) == "success" for name in ("plan", "packages", "native"))
    rows = [
        f"# AW integration: `{expected}`",
        "",
        "Owner: @kongche-jbw. Route backend contract failures to the component CODEOWNERS.",
        "",
        "| Agent | Result | Verified boundary / remaining acceptance |",
        "| --- | --- | --- |",
    ]
    reports = {}
    for path in directory.glob("*/result.json"):
        report = json.loads(path.read_text())
        agent = report["agent"]
        if agent in reports or agent not in manifest["agents"]:
            raise ValueError(f"duplicate or unknown native report: {agent}")
        reports[agent] = (report, path.parent)
    for agent, row in manifest["agents"].items():
        report, evidence = reports.get(agent, ({}, directory))
        cases = report.get("cases", [])
        complete = (
            report.get("candidate_sha") == expected
            and report.get("status") == "passed"
            and {case["name"] for case in cases} == {case["name"] for case in row["cases"]}
            and len(cases) == len(row["cases"])
            and all(case["status"] == "passed" for case in cases)
        )
        if complete:
            try:
                recorded = {case["name"]: case for case in cases}
                for case in row["cases"]:
                    log = recorded[case["name"]]["log"]
                    if Path(log).name != log:
                        raise ValueError("native log must be within its artifact")
                    verify_case(case, (evidence / log).read_text(), evidence)
            except (OSError, ValueError, KeyError, TypeError):
                complete = False
        passed = passed and complete
        rows.append(f"| {agent} | {'PASS' if complete else 'FAIL / missing'} | {row['boundary']} |")
    rows.extend(
        [
            "",
            f"Job outcomes: `{json.dumps(outcomes, sort_keys=True)}`.",
            "",
            "A green run covers only the declared native boundaries. "
            "Before release, rerun this workflow on the exact release SHA.",
        ]
    )
    return "\n".join(rows) + "\n", passed


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate-sha", required=True)
    parser.add_argument("--results", required=True, type=Path)
    args = parser.parse_args()
    outcomes = {
        name: os.environ.get(variable)
        for name, variable in (
            ("plan", "PLAN_RESULT"),
            ("packages", "PACKAGE_RESULT"),
            ("native", "NATIVE_RESULT"),
        )
    }
    summary, passed = summarize(load(AW), args.results, args.candidate_sha, outcomes)
    print(summary)
    if "GITHUB_STEP_SUMMARY" in os.environ:
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as stream:
            stream.write(summary)
    if not passed:
        raise SystemExit("AW integration failed or has incomplete automated evidence")


if __name__ == "__main__":
    main()

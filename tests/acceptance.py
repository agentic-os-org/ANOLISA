"""Validate required component regression evidence without rerunning test suites."""

import argparse
import hashlib
import json
import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

MAX_REPORT_BYTES = 16 * 1024 * 1024
REGISTRY = Path(__file__).resolve().parents[1] / ".github" / "components.json"


class JUnitFailure(ValueError):
    """Preserve explicit test failures even when report metadata is inconsistent."""


class NoDTDTreeBuilder(ET.TreeBuilder):
    """Reject declarations at parser level, independent of the XML encoding."""

    def doctype(self, name: str, pubid: str | None, system: str | None) -> None:
        raise ValueError("JUnit DTDs and entities are unsupported")


def junit_counts(payload: bytes) -> dict[str, int]:
    """Count test cases and reject empty, inconsistent or ambiguous JUnit XML."""
    root = ET.fromstring(payload, parser=ET.XMLParser(target=NoDTDTreeBuilder()))
    has_failure = any(
        case.find("failure") is not None or case.find("error") is not None
        for case in root.iter("testcase")
    )
    try:
        if root.tag not in ("testsuites", "testsuite"):
            raise ValueError("expected testsuite or testsuites")
        for suite in root.iter():
            if suite.tag not in ("testsuites", "testsuite"):
                continue
            if any(
                child.tag not in ("testsuite", "testcase", "properties", "system-out", "system-err")
                for child in suite
            ):
                raise ValueError("unsupported JUnit suite outcome or element")
            cases = list(suite.iter("testcase"))
            counts = {
                "tests": len(cases),
                "failures": sum(case.find("failure") is not None for case in cases),
                "errors": sum(case.find("error") is not None for case in cases),
                "skipped": sum(case.find("skipped") is not None for case in cases),
            }
            for field, actual in counts.items():
                if field in suite.attrib and int(suite.attrib[field]) != actual:
                    raise ValueError(f"JUnit {field} count disagrees with test cases")
        cases = list(root.iter("testcase"))
        if not cases:
            raise ValueError("JUnit contains no test cases")
        for case in cases:
            if not case.get("name"):
                raise ValueError("JUnit test case has no name")
            if case.get("status", "run") not in ("run", "passed"):
                raise ValueError("unsupported JUnit case status")
            outcomes = [child for child in case if child.tag in ("failure", "error", "skipped")]
            if len(outcomes) > 1:
                raise ValueError("JUnit test case has multiple outcomes")
        return {
            "tests": len(cases),
            "failures": sum(case.find("failure") is not None for case in cases),
            "errors": sum(case.find("error") is not None for case in cases),
            "skipped": sum(case.find("skipped") is not None for case in cases),
        }
    except ValueError as error:
        if has_failure:
            raise JUnitFailure(f"JUnit contains a failed test; {error}") from error
        raise


def evaluate(manifest: Path) -> dict[str, object]:
    """Check candidate identity, required coverage and report content digests."""
    data = json.loads(manifest.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or data.get("schema_version") != 1:
        raise ValueError("expected manifest schema_version=1")
    if not isinstance(data.get("candidate_sha"), str) or not re.fullmatch(
        "[0-9a-f]{40}", data["candidate_sha"]
    ):
        raise ValueError("candidate_sha must be a full lowercase Git commit SHA")
    reports = data.get("reports")
    if not isinstance(reports, list) or not reports:
        raise ValueError("manifest must declare reports")
    registry = json.loads(REGISTRY.read_text(encoding="utf-8"))
    components = {component["id"] for component in registry["components"]}
    seen = set()
    required = 0
    results = []
    for report in reports:
        if not isinstance(report, dict):
            raise ValueError("each report must be an object")
        identity = report.get("id")
        if not isinstance(identity, str) or not identity or identity in seen:
            raise ValueError("report IDs must be nonempty and unique")
        seen.add(identity)
        if report.get("component") not in components or report.get("kind") != "regression":
            raise ValueError(f"{identity}: expected registered component and kind=regression")
        if type(report.get("required")) is not bool:
            raise ValueError(f"{identity}: required must be a boolean")
        if type(report.get("exit_code")) is not int:
            raise ValueError(f"{identity}: exit_code must be an integer from the test process")
        required += report["required"]
        result = {
            "id": identity,
            "component": report["component"],
            "kind": "regression",
            "required": report["required"],
            "exit_code": report["exit_code"],
        }
        try:
            name = report.get("path")
            if not isinstance(name, str) or not name or Path(name).is_absolute():
                raise ValueError("report path must be relative to the manifest")
            path = (manifest.parent / name).resolve()
            if not path.is_relative_to(manifest.parent.resolve()):
                raise ValueError("report path escapes the evidence directory")
            with path.open("rb") as handle:
                payload = handle.read(MAX_REPORT_BYTES + 1)
            if len(payload) > MAX_REPORT_BYTES:
                raise ValueError("report exceeds 16 MiB")
            digest = hashlib.sha256(payload).hexdigest()
            if digest != report.get("sha256"):
                raise ValueError("report SHA-256 mismatch")
            counts = junit_counts(payload)
            status = "passed"
            if counts["failures"] or counts["errors"] or report["exit_code"] != 0:
                status = "failed"
            elif counts["skipped"]:
                status = "skipped"
            result.update(status=status, sha256=digest, counts=counts, path=name)
        except (OSError, ValueError, ET.ParseError) as error:
            result.update(
                status=(
                    "failed"
                    if report["exit_code"] != 0 or isinstance(error, JUnitFailure)
                    else "blocked"
                ),
                reason=str(error),
            )
        results.append(result)
    if required == 0:
        raise ValueError("at least one report must be required")
    passed = all(
        result["status"] == "passed"
        or (not result["required"] and result["status"] in ("skipped", "blocked"))
        for result in results
    )
    return {
        "schema_version": 1,
        "candidate_sha": data["candidate_sha"],
        "kind": "component_regression",
        "status": "passed" if passed else "failed",
        "reports": results,
    }


def main() -> int:
    """Emit JSON and fail acceptance when required evidence is incomplete."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    args = parser.parse_args()
    try:
        result = evaluate(args.manifest)
    except (OSError, ValueError, KeyError, TypeError) as error:
        result = {"schema_version": 1, "status": "blocked", "reason": str(error)}
    print(json.dumps(result, indent=2, sort_keys=True))
    return int(result["status"] != "passed")


if __name__ == "__main__":
    sys.exit(main())

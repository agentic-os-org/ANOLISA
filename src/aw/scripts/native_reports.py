"""Validate declared native suite evidence at execution and artifact aggregation."""

from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any


def validate_case(case: dict) -> None:
    """Every command must declare a nonempty, checkable success report."""
    kind = case.get("report")
    if kind in {"unittest", "tap"}:
        if type(case.get("tests")) is not int or case["tests"] < 1:
            raise ValueError(f"{case['name']}: native suite needs a positive test count")
        return
    if kind != "json" or not isinstance(case.get("results"), list) or not case["results"]:
        raise ValueError(f"{case['name']}: native suite needs a supported report contract")
    files = []
    for result in case["results"]:
        path = Path(result["file"])
        if path.is_absolute() or ".." in path.parts or path.suffix != ".json":
            raise ValueError(f"{case['name']}: result file must stay within the evidence directory")
        files.append(str(path))
        values = result.get("values", {})
        cases = result.get("cases")
        if not isinstance(values, dict) or (not values and not cases):
            raise ValueError(f"{case['name']}: result needs expected values or named cases")
        if cases is not None:
            if not isinstance(cases, dict):
                raise ValueError(f"{case['name']}: named cases need a report specification")
            names = cases.get("names")
            if (
                not isinstance(names, list)
                or not names
                or any(not isinstance(name, str) or not name for name in names)
                or len(set(names)) != len(names)
                or not isinstance(cases.get("at"), str)
                or not isinstance(cases.get("key"), str)
                or not cases["key"]
                or not isinstance(cases.get("values", {}), dict)
            ):
                raise ValueError(f"{case['name']}: result needs unique expected case names")
    if len(set(files)) != len(files):
        raise ValueError(f"{case['name']}: duplicate result file")


def select(document: Any, field: str) -> Any:
    """Read a slash-separated field path; an empty path selects the whole report."""
    for key in field.split("/") if field else []:
        document = document[key]
    return document


def verify_values(document: Any, expected: dict) -> None:
    for field, value in expected.items():
        actual = select(document, field)
        if type(actual) is not type(value) or actual != value:
            raise ValueError(f"{field}: expected {value!r}, got {actual!r}")


def verify_case(case: dict, output: str, directory: Path | None = None) -> None:
    """Reject absent, empty, skipped, duplicated or reduced evidence despite exit zero."""
    validate_case(case)
    if case["report"] == "json":
        if directory is None:
            raise ValueError(f"{case['name']}: missing native evidence directory")
        for result in case["results"]:
            try:
                document = json.loads((directory / result["file"]).read_text())
                verify_values(document, result.get("values", {}))
                if "cases" in result:
                    spec = result["cases"]
                    rows = select(document, spec["at"])
                    if not isinstance(rows, list):
                        raise ValueError("case results must be a list")
                    names = [row[spec["key"]] for row in rows]
                    if len(names) != len(spec["names"]) or set(names) != set(spec["names"]):
                        raise ValueError("native case names/count changed")
                    for row in rows:
                        verify_values(row, spec.get("values", {}))
            except (OSError, ValueError, KeyError, TypeError) as error:
                raise ValueError(f"{case['name']}: invalid {result['file']}: {error}") from error
        return
    if case["report"] == "unittest":
        found = re.search(r"^Ran (\d+) tests? in ", output, re.MULTILINE)
        passed = (
            found and int(found[1]) == case["tests"] and re.search(r"^OK$", output, re.MULTILINE)
        )
    else:
        counts = dict(
            re.findall(r"^# (tests|pass|fail|cancelled|skipped|todo) (\d+)$", output, re.MULTILINE)
        )
        passed = (
            counts.get("tests") == str(case["tests"])
            and counts.get("pass") == str(case["tests"])
            and all(counts.get(key) == "0" for key in ("fail", "cancelled", "skipped", "todo"))
        )
    if not passed:
        raise ValueError(f"{case['name']}: native suite count changed or tests were skipped")

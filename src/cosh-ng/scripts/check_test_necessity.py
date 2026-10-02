#!/usr/bin/env python3
"""Audit source tests against the test-necessity registry."""

from __future__ import annotations

import argparse
import csv
import pathlib
import re
import sys
from dataclasses import dataclass

ROOT = pathlib.Path(__file__).resolve().parent.parent
REGISTRY = ROOT / "scripts" / "test-necessity-registry.tsv"
TEST_ANNOTATION = re.compile(r"^\s*#\[(?:tokio::)?test[\](]")
IGNORE_ANNOTATION = re.compile(r"^\s*#\[ignore")
TEST_FUNCTION = re.compile(
    r"^\s*(?:pub\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)"
)
FIELDS = (
    "rule",
    "pattern",
    "owner",
    "layer",
    "contract",
    "failure",
    "observable",
    "minimum_layer",
    "unique_dimension",
    "evidence",
    "cost",
    "reliability",
    "gate",
    "disposition",
)


@dataclass(frozen=True)
class Rule:
    values: tuple[str, ...]

    @property
    def data(self) -> dict[str, str]:
        return dict(zip(FIELDS, self.values))

    @property
    def prefix(self) -> str:
        return self.data["pattern"].removesuffix("*")


def load_rules() -> tuple[list[Rule], list[str]]:
    failures: list[str] = []
    if not REGISTRY.is_file():
        return [], [f"missing test necessity registry: {REGISTRY}"]
    with REGISTRY.open(encoding="utf-8", newline="") as stream:
        rows = list(csv.reader(stream, delimiter="\t"))
    if not rows or tuple(rows[0]) != FIELDS:
        failures.append("registry header does not match the required fields")
    rules: list[Rule] = []
    seen: set[str] = set()
    for row in rows[1:]:
        if len(row) != len(FIELDS) or any(not value for value in row):
            failures.append(f"registry row has missing or extra fields: {chr(9).join(row)}")
            continue
        rule = Rule(tuple(row))
        name = rule.data["rule"]
        if name in seen:
            failures.append(f"duplicate registry rule: {name}")
        seen.add(name)
        rules.append(rule)
    if not rules:
        failures.append("test necessity registry is empty")
    return rules, failures


def source_files() -> list[pathlib.Path]:
    return sorted((ROOT / "crates").rglob("*.rs"))


def source_test_ids(path: pathlib.Path) -> list[str]:
    relative = path.relative_to(ROOT).as_posix()
    annotations = 0
    names: list[str] = []
    pending = False
    for line in path.read_text(encoding="utf-8").splitlines():
        if TEST_ANNOTATION.match(line):
            annotations += 1
            pending = True
            continue
        if pending and (match := TEST_FUNCTION.match(line)):
            names.append(f"{relative}::{match.group(1)}")
            pending = False
    if annotations != len(names):
        raise ValueError(f"parse-error:{relative}:{annotations}:{len(names)}")
    return names


def matching_rule(test_id: str, rules: list[Rule]) -> tuple[Rule | None, bool]:
    matches = [
        rule
        for rule in rules
        if test_id == rule.data["pattern"] or test_id.startswith(rule.prefix)
    ]
    if not matches:
        return None, False
    longest = max(len(rule.prefix) for rule in matches)
    best = [rule for rule in matches if len(rule.prefix) == longest]
    return best[0], len(best) > 1


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", action="store_true")
    args = parser.parse_args(argv)
    rules, failures = load_rules()
    rule_hits = {rule.data["rule"]: 0 for rule in rules}
    seen_ids: set[str] = set()
    test_count = 0
    ignored_count = 0

    for path in source_files():
        lines = path.read_text(encoding="utf-8").splitlines()
        ignored_count += sum(bool(IGNORE_ANNOTATION.match(line)) for line in lines)
        try:
            test_ids = source_test_ids(path)
        except ValueError as error:
            failures.append(str(error))
            continue
        for test_id in test_ids:
            rule, ambiguous = matching_rule(test_id, rules)
            if rule is None:
                failures.append(f"{test_id} has no necessity registry rule")
                continue
            if ambiguous:
                failures.append(f"{test_id} matches equally specific registry rules")
            data = rule.data
            rule_hits[data["rule"]] += 1
            if test_id in seen_ids:
                failures.append(f"duplicate stable source test id: {test_id}")
            seen_ids.add(test_id)
            test_count += 1
            if args.report:
                print(
                    "\t".join(
                        (
                            test_id,
                            data["owner"],
                            data["layer"],
                            data["contract"],
                            data["failure"],
                            data["gate"],
                            data["disposition"],
                            data["unique_dimension"],
                        )
                    )
                )

    for rule, hits in rule_hits.items():
        if hits == 0:
            failures.append(f"stale necessity registry rule matches no test file: {rule}")
    heavy_count = sum(
        rule_hits[rule.data["rule"]]
        for rule in rules
        if rule.data["disposition"] == "explicit-heavy"
    )
    if ignored_count != 3 or heavy_count != 3:
        failures.append(
            f"ignored tests ({ignored_count}) and explicit-heavy registry tests "
            f"({heavy_count}) must both remain 3"
        )

    for failure in failures:
        print(f"violation: {failure}", file=sys.stderr)
    if failures:
        print(
            f"test necessity registry failed with {len(failures)} violation(s)",
            file=sys.stderr,
        )
        return 1
    print(
        f"test necessity registry passed: {test_count} stable source IDs, "
        f"{len(rule_hits)} contract rules"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

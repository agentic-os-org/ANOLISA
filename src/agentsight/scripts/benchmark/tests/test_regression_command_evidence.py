"""Exercise durable runner records through the formal campaign evidence audit."""

from __future__ import annotations

import json
import shlex
import sys
from pathlib import Path
from typing import Any

import pytest

sys.path.insert(0, str(Path(__file__).parents[1] / "campaign"))

import run_regression
from test_campaign_evidence import campaign_evidence
from test_campaign_matrix_evidence import complete_formal_evidence


def audit(record: dict[str, Any]) -> list[str]:
    evidence = list(complete_formal_evidence(1))
    evidence[-1] = record
    return campaign_evidence.audit_campaign(*evidence)


def checks(commands: list[list[str]]) -> dict[str, Any]:
    return {
        "full": True,
        "checks": [
            {"command": shlex.join(command), "argv": command, "exit_code": 0}
            for command in commands
        ],
    }


VALID = [
    ["cargo", "fmt", "--all", "--", "--check"],
    ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
    ["cargo", "test", "--workspace"],
    ["diff-cover", "coverage.xml"],
]


@pytest.mark.parametrize(
    "command",
    [
        ["echo", "cargo fmt cargo clippy cargo test diff-cover"],
        [sys.executable, "-c", "print('cargo fmt cargo clippy cargo test diff-cover')"],
        ["not-cargo", "fmt", "cargo clippy", "cargo test", "diff-cover"],
    ],
)
def test_tool_names_inside_arguments_do_not_satisfy_gates(command: list[str]) -> None:
    issues = audit(checks([command]))
    for tool in campaign_evidence.REQUIRED_REGRESSION_TOOLS:
        assert f"regression evidence is missing {tool}" in issues


@pytest.mark.parametrize("flag", ["--help", "--version", "--list", "--no-run"])
def test_nonexecuting_cargo_test_does_not_count(flag: str) -> None:
    commands = [*VALID[:2], ["cargo", "test", flag], VALID[3]]
    assert "regression evidence is missing cargo test" in audit(checks(commands))


@pytest.mark.parametrize("full", ["false", "true", 1, {}, None])
def test_full_requires_boolean_evidence(full: object) -> None:
    record = checks(VALID)
    record["full"] = full
    assert "full Rust regression gates were not recorded" in audit(record)


@pytest.mark.parametrize("structured", [False, True])
@pytest.mark.parametrize("wrapper", ["direct", "rustup", "selector"])
def test_legacy_and_structured_real_commands_remain_compatible(
    structured: bool, wrapper: str
) -> None:
    commands = []
    for command in VALID[:3]:
        if wrapper == "rustup":
            command = ["/opt/rustup", "run", "1.89.0", *command]
        elif wrapper == "selector":
            command = ["/opt/cargo", "+1.89.0", *command[1:]]
        commands.append(command)
    commands.append(
        ["/opt/python 3.11/bin/python3", "-m", "diff_cover.diff_cover_tool", "coverage.xml"]
    )
    record = checks(commands)
    if not structured:
        for check in record["checks"]:
            del check["argv"]
    assert audit(record) == []


@pytest.mark.parametrize("argv", [[], None, "cargo test", ["echo", "cargo test"], [1]])
def test_inconsistent_structured_command_is_missing(argv: object) -> None:
    record = checks(VALID)
    record["checks"][2]["argv"] = argv
    assert "regression evidence is missing cargo test" in audit(record)


def test_actual_runner_output_cannot_impersonate_other_tools(tmp_path: Path) -> None:
    records: list[dict[str, object]] = []
    command = [sys.executable, "-c", "print('cargo fmt cargo clippy cargo test diff-cover')"]
    log = tmp_path / "check.log"
    assert run_regression.run_check(command, tmp_path, log, records) == 0
    saved = json.loads(json.dumps(records))
    assert saved[0]["argv"] == command
    assert saved[0]["command"] == shlex.join(command)
    assert "cargo test" in log.read_text()
    issues = audit({"full": True, "checks": saved})
    assert "regression evidence is missing cargo test" in issues
    assert "regression evidence is missing diff-cover" in issues


def test_failed_genuine_command_still_blocks_the_campaign() -> None:
    record = checks(VALID)
    record["checks"][2]["exit_code"] = 101
    assert "regression checks are missing or failed" in audit(record)

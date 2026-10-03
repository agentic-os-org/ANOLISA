"""Help-text contract tests for the ``skill-ledger`` command group."""

import json
import re
from pathlib import Path

from agent_sec_cli.skill_ledger.cli import app
from typer.testing import CliRunner

# Public help contract per src/agent-sec-core/AGENTS.md: Skill Ledger has
# exactly six integrity states; unmanaged and error are command outcomes
# (a show_skill() diagnostic and an exception envelope), not states.
INTEGRITY_STATES = ("pass", "none", "drifted", "warn", "deny", "tampered")
OTHER_OUTCOMES = ("unmanaged", "error")

# rich emits ANSI color codes when the environment forces color on (e.g.
# GITHUB_ACTIONS or FORCE_COLOR), so strip them before matching on layout.
_ANSI_ESCAPE_RE = re.compile(r"\x1b\[[0-9;]*m")

_SECTION_TITLES = ("Integrity states:", "Other command outcomes:")


def _plain_lines(output: str) -> list[str]:
    """Return help output as ANSI-free lines with indentation stripped."""
    return [_ANSI_ESCAPE_RE.sub("", line).strip() for line in output.splitlines()]


def _has_status_line(output: str, status: str) -> bool:
    """True if any line starts with ``status`` followed by a word boundary."""
    return any(re.match(rf"^{status}\b", line) for line in _plain_lines(output))


def _section_status_labels(output: str, section: str) -> set[str]:
    """Return status labels that start lines within the ``section`` block.

    Rich renders help differently per environment (ANSI codes, 2- vs
    3-space indent, width padding), so lines are matched layout-
    independently: strip ANSI escapes and indentation, then anchor the
    status word at a word boundary. A section ends at the next title.
    """
    labels: set[str] = set()
    in_section = False
    for line in _plain_lines(output):
        if line in _SECTION_TITLES:
            in_section = line == section
            continue
        if not in_section or not line:
            continue
        for status in (*INTEGRITY_STATES, *OTHER_OUTCOMES):
            if re.match(rf"^{status}\b", line):
                labels.add(status)
    return labels


def test_app_help_lists_integrity_states() -> None:
    result = CliRunner().invoke(app, ["--help"])

    assert result.exit_code == 0
    assert _section_status_labels(result.output, "Integrity states:") == set(
        INTEGRITY_STATES
    ), "Integrity states must list exactly the six states"


def test_app_help_lists_other_command_outcomes() -> None:
    result = CliRunner().invoke(app, ["--help"])

    assert result.exit_code == 0
    assert _section_status_labels(result.output, "Other command outcomes:") == set(
        OTHER_OUTCOMES
    ), "Other command outcomes must list exactly unmanaged and error"


def test_check_help_lists_error_status() -> None:
    result = CliRunner().invoke(app, ["check", "--help"])

    assert result.exit_code == 0
    assert _has_status_line(
        result.output, "error"
    ), "check --help must document the error status"
    # error is produced by both single-check failures and batch entries;
    # the old "(--all path only)" qualifier was inaccurate.
    assert "(--all path only)" not in result.output
    # check never returns unmanaged (only show does, via manageability checks).
    assert not _has_status_line(result.output, "unmanaged")


def test_check_missing_dir_prints_error_status_json(tmp_path: Path) -> None:
    result = CliRunner().invoke(app, ["check", str(tmp_path / "missing")])

    assert result.exit_code == 1
    payload = json.loads(result.output)
    # Assert only the error envelope contract; the message text is free to
    # evolve as new failure types are added.
    assert payload["status"] == "error"
    assert payload["error"]

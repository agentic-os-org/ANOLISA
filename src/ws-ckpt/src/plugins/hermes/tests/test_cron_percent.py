"""Percent-containing workspace arguments survive cron and shell parsing."""

import subprocess

import pytest

from hermes import cron


def cron_command(command):
    """Scan percent delimiters before shell parsing, as crontab(5) specifies."""
    result, escaped = [], False
    for index, char in enumerate(command):
        if escaped:
            if char != "%":
                result.append("\\")
            result.append(char)
            escaped = False
        elif char == "\\":
            escaped = True
        elif char == "%":
            return "".join(result), command[index + 1:]
        else:
            result.append(char)
    if escaped:
        result.append("\\")
    return "".join(result), None


@pytest.fixture
def installed(monkeypatch):
    lines = ["# unrelated entry"]
    monkeypatch.setattr(cron.CrontabManager, "_with_lock", staticmethod(lambda fn: fn()))
    monkeypatch.setattr(cron, "_read_crontab", lambda: list(lines))

    def write(new_lines):
        lines[:] = new_lines
        return True

    monkeypatch.setattr(cron, "_write_crontab", write)
    return lines


@pytest.mark.parametrize("workspace", [
    "/work/50% project", "/work/%leading", "/work/trailing%", "/work/100%%",
    r"/work/backslash\%project", r"/work/percent%\tail",
])
def test_percent_argument_roundtrip(workspace, installed):
    assert cron.CrontabManager.sync(workspace, ["0 * * * *"])
    line = next(entry for entry in installed if entry.startswith("0 *"))
    command, stdin = cron_command(line.split(" ", 5)[5])
    assert stdin is None
    assert ' -s "cron-$(date +%s)"' in command
    argument = command.split(" -w ", 1)[1].split(" -s ", 1)[0]
    # Probe only the controlled workspace argument; never run ws-ckpt or cron.
    result = subprocess.run(
        ["/bin/sh", "-c", "printf '%s' " + argument],
        check=True, capture_output=True, text=True,
    )
    assert result.stdout == workspace
    assert cron.CrontabManager.list_installed(workspace) == ["0 * * * *"]
    assert cron.CrontabManager.sync(workspace, ["5 4 * * *"])
    assert cron.CrontabManager.list_installed(workspace) == ["5 4 * * *"]
    assert cron.CrontabManager.remove(workspace)
    assert installed == ["# unrelated entry"]


def test_legacy_percent_entry_preserves_other_workspace(installed):
    installed.extend([
        "0 * * * * ws-ckpt checkpoint -w '/work/50% project' -s old",
        "0 * * * * ws-ckpt checkpoint -w '/work/50% project-extra' -s other",
    ])
    assert cron.CrontabManager.sync("/work/50% project", ["1 * * * *"])
    assert not any("-s old" in line for line in installed)
    assert any("-s other" in line for line in installed)
    assert cron.CrontabManager.list_installed("/work/50% project") == ["1 * * * *"]

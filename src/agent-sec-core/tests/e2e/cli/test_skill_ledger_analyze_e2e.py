"""End-to-end tests for the read-only Skill Ledger analyze command."""

import hashlib
import json
import os
import stat
import subprocess
from pathlib import Path


def _write_skill(root: Path) -> None:
    root.mkdir()
    (root / "SKILL.md").write_text(
        "---\nname: analyze-e2e\ndescription: Analyze e2e Skill\n---\nSafe content.\n",
        encoding="utf-8",
    )


def _snapshot(root: Path) -> list[tuple[str, int, int, str]]:
    entries: list[tuple[str, int, int, str]] = []
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root).as_posix()
        metadata = path.lstat()
        digest = ""
        if stat.S_ISREG(metadata.st_mode):
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
        elif stat.S_ISLNK(metadata.st_mode):
            digest = os.readlink(path)
        entries.append((relative, metadata.st_mode, metadata.st_mtime_ns, digest))
    return entries


def test_analyze_is_machine_readable_and_side_effect_free(tmp_path: Path) -> None:
    root = tmp_path / "skill"
    _write_skill(root)
    isolated_home = tmp_path / "home"
    isolated_home.mkdir()
    env = os.environ.copy()
    env.update(
        {
            "HOME": str(isolated_home),
            "XDG_CONFIG_HOME": str(tmp_path / "config"),
            "XDG_DATA_HOME": str(tmp_path / "data"),
            "XDG_CACHE_HOME": str(tmp_path / "cache"),
            "AGENT_SEC_DATA_DIR": str(tmp_path / "events"),
        }
    )
    before = _snapshot(tmp_path)

    result = subprocess.run(
        ["agent-sec-cli", "skill-ledger", "analyze", str(root), "--format", "json"],
        check=False,
        capture_output=True,
        text=True,
        timeout=30,
        env=env,
    )

    assert result.returncode == 0, result.stderr
    payload = json.loads(result.stdout)
    assert payload["status"] == "pass"
    assert payload["coverage_complete"] is True
    assert result.stderr == ""
    after = _snapshot(tmp_path)
    assert after == before


def test_analyze_protocol_error_is_json(tmp_path: Path) -> None:
    result = subprocess.run(
        [
            "agent-sec-cli",
            "skill-ledger",
            "analyze",
            str(tmp_path / "missing"),
            "--format",
            "json",
        ],
        check=False,
        capture_output=True,
        text=True,
        timeout=30,
    )

    assert result.returncode == 2
    payload = json.loads(result.stdout)
    assert payload["status"] == "error"
    assert payload["coverage_complete"] is False
    assert payload["errors"][0]["code"] == "root-not-found"

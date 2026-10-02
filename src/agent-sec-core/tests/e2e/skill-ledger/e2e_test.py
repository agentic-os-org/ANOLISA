#!/usr/bin/env python3
"""End-to-end tests for the ``skill-ledger`` CLI on RPM-installed environments.

Tests exercise the installed ``agent-sec-cli`` binary directly (no ``uv run``,
no ``python -m``).  Designed to run after RPM installation to validate the
full CLI pipeline, cosh hook integration, and passphrase-protected key flows.

Test groups:
   G1  Pre-flight & help
   G2  init --no-baseline
   G3  Happy-path lifecycle (check → certify → check → audit)
   G4  check state machine
   G5  certify command
   G6  scan --all
   G7  audit
   G8  status (human-readable)
   G9  Reserved commands & edge cases
   G10 SKILL.md contract assertions
   G11 Passphrase-protected key lifecycle
   G12 cosh hook integration
   G13 Full vetter → ledger → hook pipeline
   G14 Key rotation

Usage::

    python3 e2e_test.py            # normal run
    python3 e2e_test.py --verbose  # show CLI stdout/stderr
"""

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

import pytest

# ── Globals ────────────────────────────────────────────────────────────────

CLI_BIN = shutil.which("agent-sec-cli")
VERBOSE = False


# ── Constants ─────────────────────────────────────────────────────────────

CLI_TIMEOUT_S = 60  # seconds — safety net against unexpected hangs


# ── Helpers ────────────────────────────────────────────────────────────────


def run_skill_ledger(
    args: list[str],
    env_extra: dict | None = None,
    *,
    cwd: str | Path | None = None,
) -> subprocess.CompletedProcess:
    """Run ``agent-sec-cli skill-ledger <args>`` with isolated XDG env."""
    env = os.environ.copy()
    if env_extra:
        env.update(env_extra)
    cmd = [CLI_BIN, "skill-ledger"] + args
    result = subprocess.run(
        cmd,
        capture_output=True,
        check=False,
        text=True,
        env=env,
        cwd=cwd,
        stdin=subprocess.DEVNULL,
        timeout=CLI_TIMEOUT_S,
    )
    if VERBOSE:
        print(f"  cmd: {' '.join(cmd[:5])} ... {' '.join(args)}")
        if result.stdout.strip():
            print(f"  stdout: {result.stdout.strip()[:200]}")
        if result.stderr.strip():
            print(f"  stderr: {result.stderr.strip()[:200]}")
        print(f"  exit: {result.returncode}")
    return result


def parse_json_output(stdout: str) -> dict:
    """Parse the first JSON line from CLI stdout."""
    for line in stdout.strip().splitlines():
        line = line.strip()
        if line.startswith("{") or line.startswith("["):
            return json.loads(line)
    raise ValueError(f"No JSON found in stdout:\n{stdout}")


def make_skill(parent: Path, name: str, files: dict[str, str]) -> Path:
    """Create a fake skill directory with the given files.

    Automatically adds a minimal ``SKILL.md`` if not provided, so that
    ``validate_skill_dir()`` passes.
    """
    if "SKILL.md" not in files:
        files = {
            "SKILL.md": (
                f"---\nname: {name}\ndescription: Test skill\n---\n# {name}\n"
            ),
            **files,
        }
    skill_dir = parent / name
    for rel, content in files.items():
        p = skill_dir / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(content)
    return skill_dir


def write_findings_file(parent: Path, name: str, findings: list | dict) -> Path:
    """Write a findings JSON file and return its path."""
    path = parent / name
    path.write_text(json.dumps(findings, ensure_ascii=False))
    return path


def snapshot_file_tree(root: Path) -> dict[str, bytes]:
    """Return relative paths and contents for every file below *root*."""
    if not root.is_dir():
        return {}
    return {
        path.relative_to(root).as_posix(): path.read_bytes()
        for path in sorted(root.rglob("*"))
        if path.is_file()
    }


# ── Workspace ──────────────────────────────────────────────────────────────


class Workspace:
    """Shared test workspace: isolated XDG dirs, skills dir."""

    def __init__(self):
        # Resolve the temp root immediately so that macOS /var/folders/...
        # (a symlink to /private/var/folders/...) is expanded before certify
        # records the canonical path. The cosh hook script also calls
        # Path.resolve(), so both sides agree on the same real path and
        # drifted skills are not misreported as unmanaged.
        self.root = Path(tempfile.mkdtemp(prefix="e2e_rpm_skill_ledger_")).resolve()
        self.xdg_data = self.root / "xdg_data"
        self.xdg_config = self.root / "xdg_config"
        self.xdg_data.mkdir()
        self.xdg_config.mkdir()
        config_dir = self.xdg_config / "agent-sec" / "skill-ledger"
        config_dir.mkdir(parents=True)
        (config_dir / "config.json").write_text(
            json.dumps({"enableDefaultSkillDirs": False, "managedSkillDirs": []}),
            encoding="utf-8",
        )
        self.skills_dir = self.root / "skills"
        self.skills_dir.mkdir()
        self.fixtures = self.root / "fixtures"
        self.fixtures.mkdir()
        # Hook-visible skills: the cosh hook resolves via {cwd}/.copilot-shell/skills/
        self.hook_skills_dir = self.root / ".copilot-shell" / "skills"
        self.hook_skills_dir.mkdir(parents=True)

        os.environ["XDG_DATA_HOME"] = str(self.xdg_data)
        os.environ["XDG_CONFIG_HOME"] = str(self.xdg_config)

    def env(self, extra: dict | None = None) -> dict:
        """Return env dict with XDG isolation (for subprocess)."""
        e = {
            "XDG_DATA_HOME": str(self.xdg_data),
            "XDG_CONFIG_HOME": str(self.xdg_config),
        }
        if extra:
            e.update(extra)
        return e

    def cleanup(self):
        for key in ("XDG_DATA_HOME", "XDG_CONFIG_HOME"):
            os.environ.pop(key, None)
        shutil.rmtree(self.root, ignore_errors=True)


@dataclass(frozen=True)
class E2ECase:
    """One named skill-ledger E2E scenario."""

    name: str
    fn: Callable[[Workspace], None]
    requires_hook: bool = False
    init_default_keys: bool = True


# ── G1: Pre-flight & help ─────────────────────────────────────────────────


def case_help_available(ws: Workspace):
    """``agent-sec-cli skill-ledger --help`` → exit 0."""
    r = run_skill_ledger(["--help"], env_extra=ws.env())
    assert r.returncode == 0, f"--help returned {r.returncode}: {r.stderr}"
    assert (
        "skill-ledger" in r.stdout.lower()
    ), f"Expected 'skill-ledger' in help output: {r.stdout[:200]}"
    assert "rotate-keys" in r.stdout


# ── G2: init --no-baseline ─────────────────────────────────────────────────────────


def case_init_no_passphrase(ws: Workspace):
    """init --no-baseline without passphrase → exit 0, encrypted: false."""
    r = run_skill_ledger(["init", "--no-baseline"], env_extra=ws.env())
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)["key"]
    assert out.get("encrypted") is False, f"expected encrypted=false, got {out}"
    assert out.get("fingerprint", "").startswith("sha256:"), f"bad fingerprint: {out}"


def case_init_json_structure(ws: Workspace):
    """JSON output must contain all 4 expected fields."""
    r = run_skill_ledger(["init", "--no-baseline", "--force-keys"], env_extra=ws.env())
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)["key"]
    for key in ("fingerprint", "publicKeyPath", "privateKeyPath", "encrypted"):
        assert key in out, f"Missing field '{key}' in output: {out}"
    assert len(out["fingerprint"]) > 10
    assert len(out["publicKeyPath"]) > 0
    assert len(out["privateKeyPath"]) > 0


def case_init_reuses_existing_keys(ws: Workspace):
    """Repeated initialization reuses existing key material."""
    alt_data = ws.root / "alt_data"
    alt_data.mkdir()
    env = ws.env({"XDG_DATA_HOME": str(alt_data)})
    r1 = run_skill_ledger(["init", "--no-baseline"], env_extra=env)
    assert r1.returncode == 0, f"first init failed: {r1.stderr}"

    r2 = run_skill_ledger(["init", "--no-baseline"], env_extra=env)
    assert r2.returncode == 0, r2.stderr
    repeated = parse_json_output(r2.stdout)
    assert repeated["keyCreated"] is False
    assert repeated["key"] is None
    key_path = Path(parse_json_output(r1.stdout)["key"]["publicKeyPath"])
    assert key_path.is_file()


def case_init_force_overwrite(ws: Workspace):
    """--force overwrites existing keys and produces a new fingerprint."""
    alt_data = ws.root / "force_data"
    alt_data.mkdir()
    env = ws.env({"XDG_DATA_HOME": str(alt_data)})
    r1 = run_skill_ledger(["init", "--no-baseline"], env_extra=env)
    assert r1.returncode == 0
    fp1 = parse_json_output(r1.stdout)["key"]["fingerprint"]

    r2 = run_skill_ledger(["init", "--no-baseline", "--force-keys"], env_extra=env)
    assert r2.returncode == 0, f"exit {r2.returncode}: {r2.stderr}"
    fp2 = parse_json_output(r2.stdout)["key"]["fingerprint"]
    assert fp1 != fp2, f"Fingerprint should change after --force: {fp1}"


def case_init_with_passphrase_env(ws: Workspace):
    """SKILL_LEDGER_PASSPHRASE env var → encrypted: true."""
    alt_data = ws.root / "pass_data"
    alt_data.mkdir()
    env = ws.env(
        {
            "XDG_DATA_HOME": str(alt_data),
            "SKILL_LEDGER_PASSPHRASE": "test-passphrase-123",
        }
    )
    r = run_skill_ledger(["init", "--no-baseline", "--passphrase"], env_extra=env)
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)["key"]
    assert out.get("encrypted") is True, f"expected encrypted=true, got {out}"


# ── G3: Happy-path lifecycle ──────────────────────────────────────────────


def case_full_lifecycle_pass(ws: Workspace):
    """init --no-baseline → check (none) → certify (pass) → check (pass) → audit (valid)."""
    skill = make_skill(
        ws.skills_dir,
        "lifecycle-pass",
        {"main.py": "print('hello')\n", "README.md": "# Test\n"},
    )
    env = ws.env()

    r = run_skill_ledger(["check", str(skill)], env_extra=env)
    assert r.returncode == 0, f"check exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["status"] == "none", f"expected none, got {out}"

    findings = write_findings_file(
        ws.fixtures,
        "pass.json",
        [{"rule": "no-sudo", "level": "pass", "message": "No sudo found"}],
    )
    r = run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    assert r.returncode == 0, f"certify exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["scanStatus"] == "pass", f"expected pass, got {out}"

    r = run_skill_ledger(["check", str(skill)], env_extra=env)
    assert r.returncode == 0, f"check exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["status"] == "pass", f"expected pass, got {out}"

    r = run_skill_ledger(["audit", str(skill)], env_extra=env)
    assert r.returncode == 0, f"audit exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["valid"] is True, f"expected valid=true, got {out}"


def case_multi_version_lifecycle(ws: Workspace):
    """certify → modify file → certify → audit validates 2-version chain."""
    skill = make_skill(ws.skills_dir, "multi-ver", {"data.txt": "v1"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "mv-pass.json",
        [{"rule": "safe", "level": "pass", "message": "OK"}],
    )

    r = run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    assert r.returncode == 0, f"certify1 exit {r.returncode}: {r.stderr}"
    out1 = parse_json_output(r.stdout)
    assert out1["newVersion"] is True

    (skill / "data.txt").write_text("v2")
    r = run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    assert r.returncode == 0, f"certify2 exit {r.returncode}: {r.stderr}"
    out2 = parse_json_output(r.stdout)
    assert out2["newVersion"] is True
    assert out2["versionId"] != out1["versionId"], "Expected different versionId"

    r = run_skill_ledger(["audit", str(skill)], env_extra=env)
    assert r.returncode == 0
    out = parse_json_output(r.stdout)
    assert out["valid"] is True
    assert out["versions_checked"] == 2, f"expected 2, got {out['versions_checked']}"


def case_lifecycle_with_warn_findings(ws: Workspace):
    """certify with warn findings → check returns warn, exit 0."""
    skill = make_skill(ws.skills_dir, "lifecycle-warn", {"app.sh": "#!/bin/bash\n"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "warn.json",
        [
            {
                "rule": "shell-warning",
                "level": "warn",
                "message": "Script lacks set -e",
            },
            {"rule": "no-sudo", "level": "pass", "message": "No sudo found"},
        ],
    )
    r = run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    assert r.returncode == 0, f"certify exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["scanStatus"] == "warn", f"expected warn, got {out}"

    r = run_skill_ledger(["check", str(skill)], env_extra=env)
    assert r.returncode == 0, f"check should exit 0 for warn: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["status"] == "warn"


# ── G4: check state machine ──────────────────────────────────────────────


def case_check_no_manifest_is_read_only(ws: Workspace):
    """First check on new skill → status=none without writing metadata."""
    skill = make_skill(ws.skills_dir, "check-new", {"f.txt": "hello"})
    env = ws.env()
    r = run_skill_ledger(["check", str(skill)], env_extra=env)
    assert r.returncode == 0
    out = parse_json_output(r.stdout)
    assert out["status"] == "none"
    assert out["versionId"] is None
    assert not (skill / ".skill-meta" / "latest.json").exists()
    assert not (skill / ".skill-meta" / "versions").exists()


def case_check_after_file_add_drifted(ws: Workspace):
    """Adding a file after certify → status=drifted."""
    skill = make_skill(ws.skills_dir, "check-add", {"original.txt": "content"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "add-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    (skill / "new_file.txt").write_text("I am new")
    r = run_skill_ledger(["check", str(skill)], env_extra=env)
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["status"] == "drifted", f"expected drifted, got {out}"
    assert "new_file.txt" in out.get("added", [])


def case_check_after_file_modify_drifted(ws: Workspace):
    """Modifying a file after certify → status=drifted."""
    skill = make_skill(ws.skills_dir, "check-modify", {"data.txt": "original"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "mod-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    (skill / "data.txt").write_text("CHANGED")
    r = run_skill_ledger(["check", str(skill)], env_extra=env)
    assert r.returncode == 0
    out = parse_json_output(r.stdout)
    assert out["status"] == "drifted"
    assert "data.txt" in out.get("modified", [])


def case_check_after_file_remove_drifted(ws: Workspace):
    """Removing a file after certify → status=drifted."""
    skill = make_skill(
        ws.skills_dir,
        "check-remove",
        {"keep.txt": "keep", "delete_me.txt": "gone"},
    )
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "rm-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    (skill / "delete_me.txt").unlink()
    r = run_skill_ledger(["check", str(skill)], env_extra=env)
    assert r.returncode == 0
    out = parse_json_output(r.stdout)
    assert out["status"] == "drifted"
    assert "delete_me.txt" in out.get("removed", [])


def case_check_tampered_manifest_hash(ws: Workspace):
    """Tamper with latest.json without re-hashing → status=tampered, exit 1."""
    skill = make_skill(ws.skills_dir, "check-tamper", {"f.txt": "safe"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "tamper-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    latest = skill / ".skill-meta" / "latest.json"
    data = json.loads(latest.read_text())
    data["scanStatus"] = "deny"
    latest.write_text(json.dumps(data))
    r = run_skill_ledger(["check", str(skill)], env_extra=env)
    assert r.returncode == 1, f"expected exit 1 for tampered, got {r.returncode}"
    out = parse_json_output(r.stdout)
    assert out["status"] == "tampered", f"expected tampered, got {out}"


def case_check_deny_exit_code_1(ws: Workspace):
    """Certify with deny findings → check returns deny with exit 1."""
    skill = make_skill(ws.skills_dir, "check-deny", {"danger.sh": "rm -rf /"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "deny.json",
        [{"rule": "dangerous-cmd", "level": "deny", "message": "rm -rf detected"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    r = run_skill_ledger(["check", str(skill)], env_extra=env)
    assert r.returncode == 1, f"expected exit 1 for deny, got {r.returncode}"
    out = parse_json_output(r.stdout)
    assert out["status"] == "deny", f"expected deny, got {out}"


# ── G5: certify command ──────────────────────────────────────────────────


def case_certify_external_findings_bare_array(ws: Workspace):
    """--findings with bare JSON array → exit 0, correct scanStatus."""
    skill = make_skill(ws.skills_dir, "certify-bare", {"a.txt": "a"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "bare.json",
        [
            {"rule": "r1", "level": "pass", "message": "ok"},
            {"rule": "r2", "level": "warn", "message": "caution"},
        ],
    )
    r = run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    assert r.returncode == 0
    out = parse_json_output(r.stdout)
    assert out["scanStatus"] == "warn"


def case_certify_external_findings_wrapped(ws: Workspace):
    """--findings with {"findings": [...]} wrapper → exit 0."""
    skill = make_skill(ws.skills_dir, "certify-wrap", {"b.txt": "b"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "wrapped.json",
        {"findings": [{"rule": "r1", "level": "pass", "message": "ok"}]},
    )
    r = run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    assert r.returncode == 0
    out = parse_json_output(r.stdout)
    assert out["scanStatus"] == "pass"


def case_certify_deny_finding_produces_deny(ws: Workspace):
    """deny-level finding → scanStatus=deny."""
    skill = make_skill(ws.skills_dir, "certify-deny", {"c.txt": "c"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "deny-f.json",
        [
            {"rule": "r-pass", "level": "pass", "message": "ok"},
            {"rule": "r-deny", "level": "deny", "message": "blocked"},
        ],
    )
    r = run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    assert r.returncode == 0
    out = parse_json_output(r.stdout)
    assert out["scanStatus"] == "deny"


def case_certify_missing_findings_file(ws: Workspace):
    """--findings pointing to nonexistent file → exit 1."""
    skill = make_skill(ws.skills_dir, "certify-missing", {"d.txt": "d"})
    env = ws.env()
    r = run_skill_ledger(
        ["certify", str(skill), "--findings", "/tmp/nonexistent_findings.json"],
        env_extra=env,
    )
    assert r.returncode == 1, f"expected exit 1, got {r.returncode}"


def case_certify_invalid_json_findings(ws: Workspace):
    """--findings with invalid JSON → exit 1."""
    skill = make_skill(ws.skills_dir, "certify-badjson", {"e.txt": "e"})
    env = ws.env()
    bad_file = ws.fixtures / "bad.json"
    bad_file.write_text("{not valid json!!!")
    r = run_skill_ledger(
        ["certify", str(skill), "--findings", str(bad_file)], env_extra=env
    )
    assert r.returncode == 1, f"expected exit 1 for invalid JSON, got {r.returncode}"


def case_scan_auto_invoke_default_scanners(ws: Workspace):
    """scan runs default built-in scanners."""
    skill = make_skill(
        ws.skills_dir,
        "certify-auto",
        {
            "SKILL.md": "---\nname: certify-auto\ndescription: Clean test skill\n---\n",
            "f.txt": "f",
        },
    )
    env = ws.env()
    r = run_skill_ledger(["scan", str(skill)], env_extra=env)
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["scanStatus"] == "pass"

    manifest = json.loads((skill / ".skill-meta" / "latest.json").read_text())
    scans = {entry["scanner"]: entry for entry in manifest["scans"]}
    assert "code-scanner" in scans
    assert "static-scanner" in scans
    assert scans["code-scanner"]["status"] == "pass"
    assert scans["static-scanner"]["status"] == "pass"


def case_certify_no_skill_dir_no_all(ws: Workspace):
    """certify without skill_dir and without --all → exit 1."""
    env = ws.env()
    r = run_skill_ledger(["certify"], env_extra=env)
    assert r.returncode != 0, f"expected nonzero exit, got {r.returncode}"
    combined = r.stdout + r.stderr
    assert (
        "required" in combined.lower() or "skill_dir" in combined.lower()
    ), f"Expected error about missing skill_dir: {combined}"


# ── G6: scan --all ───────────────────────────────────────────────────────


def case_scan_all_multiple_skills(ws: Workspace):
    """--all scans all skills from config.json managedSkillDirs."""
    env = ws.env()
    batch_root = ws.root / "batch_skills"
    batch_root.mkdir()
    for name in ("skill-x", "skill-y", "skill-z"):
        make_skill(batch_root, name, {"main.py": f"# {name}\n"})

    config_dir = ws.xdg_config / "agent-sec" / "skill-ledger"
    config_dir.mkdir(parents=True, exist_ok=True)
    config = {
        "enableDefaultSkillDirs": False,
        "managedSkillDirs": [str(batch_root / "*")],
    }
    (config_dir / "config.json").write_text(json.dumps(config))

    r = run_skill_ledger(
        ["scan", "--all"],
        env_extra=env,
    )
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert "results" in out, f"Expected 'results' key: {out}"
    assert len(out["results"]) == 3, f"Expected 3 results, got {len(out['results'])}"


def case_scan_all_no_skill_dirs(ws: Workspace):
    """--all with default dirs disabled and empty managedSkillDirs → exit 1."""
    env = ws.env()
    config_dir = ws.xdg_config / "agent-sec" / "skill-ledger"
    config_dir.mkdir(parents=True, exist_ok=True)
    config = {"enableDefaultSkillDirs": False, "managedSkillDirs": []}
    (config_dir / "config.json").write_text(json.dumps(config))
    r = run_skill_ledger(["scan", "--all"], env_extra=env)
    assert r.returncode == 1, f"expected exit 1, got {r.returncode}"
    combined = r.stdout + r.stderr
    assert (
        "no skill directories" in combined.lower()
    ), f"Expected no-dirs message: {combined}"


# ── G7: audit command ────────────────────────────────────────────────────


def case_audit_valid_chain(ws: Workspace):
    """Multi-version audit → valid=true, exit 0."""
    skill = make_skill(ws.skills_dir, "audit-valid", {"a.txt": "a"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "audit-p.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    (skill / "a.txt").write_text("a-v2")
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    r = run_skill_ledger(["audit", str(skill)], env_extra=env)
    assert r.returncode == 0
    out = parse_json_output(r.stdout)
    assert out["valid"] is True
    assert out["versions_checked"] >= 2


def case_audit_no_versions(ws: Workspace):
    """Skill with no .skill-meta → valid=true, 0 versions checked."""
    skill = make_skill(ws.skills_dir, "audit-none", {"x.txt": "x"})
    env = ws.env()
    r = run_skill_ledger(["audit", str(skill)], env_extra=env)
    assert r.returncode == 0
    out = parse_json_output(r.stdout)
    assert out["valid"] is True
    assert out["versions_checked"] == 0


def case_audit_tampered_version_file(ws: Workspace):
    """Tamper with a version JSON → valid=false, exit 1."""
    skill = make_skill(ws.skills_dir, "audit-tamper", {"f.txt": "f"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "audit-t.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    versions_dir = skill / ".skill-meta" / "versions"
    version_files = sorted(versions_dir.glob("v*.json"))
    assert len(version_files) >= 1, f"No version files: {list(versions_dir.iterdir())}"
    vf = version_files[0]
    data = json.loads(vf.read_text())
    data["scanStatus"] = "deny"
    vf.write_text(json.dumps(data))
    r = run_skill_ledger(["audit", str(skill)], env_extra=env)
    assert r.returncode == 1, f"expected exit 1 for tampered audit, got {r.returncode}"
    out = parse_json_output(r.stdout)
    assert out["valid"] is False
    assert len(out["errors"]) > 0


def case_audit_verify_snapshots(ws: Workspace):
    """--verify-snapshots validates snapshot file hashes match manifest."""
    skill = make_skill(ws.skills_dir, "audit-snap", {"s.txt": "snapshot-test"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "audit-s.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    r = run_skill_ledger(["audit", str(skill), "--verify-snapshots"], env_extra=env)
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["valid"] is True


# ── G8: status command ───────────────────────────────────────────────────


def case_status_human_readable_output(ws: Workspace):
    """status returns ledger-wide overview with keys, config, skills sections."""
    env = ws.env()

    batch_root = ws.root / "status_batch_skills"
    batch_root.mkdir()
    for name in ("sa-skill-1", "sa-skill-2"):
        make_skill(batch_root, name, {"run.sh": f"echo {name}\n"})

    config_dir = ws.xdg_config / "agent-sec" / "skill-ledger"
    config_dir.mkdir(parents=True, exist_ok=True)
    config = {
        "enableDefaultSkillDirs": False,
        "managedSkillDirs": [str(batch_root / "*")],
    }
    (config_dir / "config.json").write_text(json.dumps(config))

    r = run_skill_ledger(["status"], env_extra=env)
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["command"] == "status"

    # keys section
    assert "keys" in out, f"Missing 'keys' section: {out}"
    assert out["keys"]["initialized"] is True

    # config section
    assert "config" in out, f"Missing 'config' section: {out}"
    assert out["config"]["customized"] is True

    # skills section with breakdown
    skills = out["skills"]
    assert skills["discovered"] == 2, f"Expected 2 discovered, got {skills}"
    assert skills["breakdown"]["none"] == 2
    assert skills["health"] == "unscanned"

    # no results by default (requires --verbose)
    assert "results" not in out, f"results should not appear without --verbose: {out}"


def case_status_drifted_shows_details(ws: Workspace):
    """status health reflects drifted when a certified skill is modified."""
    env = ws.env()

    batch_root = ws.root / "status_drift_skills"
    batch_root.mkdir()
    skill = make_skill(
        batch_root,
        "drift-test",
        {"orig.txt": "original"},
    )

    config_dir = ws.xdg_config / "agent-sec" / "skill-ledger"
    config_dir.mkdir(parents=True, exist_ok=True)
    config = {
        "enableDefaultSkillDirs": False,
        "managedSkillDirs": [str(batch_root / "*")],
    }
    (config_dir / "config.json").write_text(json.dumps(config))

    findings = write_findings_file(
        ws.fixtures,
        "status-d.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )

    # Cause drift
    (skill / "orig.txt").write_text("MODIFIED")

    r = run_skill_ledger(["status"], env_extra=env)
    assert r.returncode == 0
    out = parse_json_output(r.stdout)
    assert (
        out["skills"]["health"] == "attention"
    ), f"Expected health 'attention' after drift: {out['skills']}"


# ── G9: reserved commands & edge cases ───────────────────────────────────


def case_rotate_keys_not_implemented(ws: Workspace) -> None:
    """rotate-keys fails explicitly without changing the isolated key store."""
    key_dir = ws.xdg_data / "agent-sec" / "skill-ledger"
    before = snapshot_file_tree(key_dir)
    keyring_existed = (key_dir / "keyring").is_dir()
    assert {"key.enc", "key.pub"}.issubset(before)

    r = run_skill_ledger(["rotate-keys"], env_extra=ws.env())
    assert r.returncode == 1, f"exit {r.returncode}: {r.stderr}"
    assert r.stdout == ""
    assert r.stderr == (
        "Error: rotate-keys is not implemented; no keys were changed.\n"
    )
    assert snapshot_file_tree(key_dir) == before
    assert (key_dir / "keyring").is_dir() is keyring_existed

    help_result = run_skill_ledger(["rotate-keys", "--help"], env_extra=ws.env())
    assert help_result.returncode == 0, help_result.stderr
    assert "not implemented" in help_result.stdout.lower()


def case_list_scanners(ws: Workspace):
    """list-scanners → exit 0, JSON with default scanners."""
    r = run_skill_ledger(["list-scanners"], env_extra=ws.env())
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert "scanners" in out, f"Expected 'scanners' key in JSON output: {out}"
    names = [s["name"] for s in out["scanners"]]
    assert "skill-vetter" in names, f"Expected skill-vetter in scanners: {names}"
    assert "code-scanner" in names, f"Expected code-scanner in scanners: {names}"
    assert "static-scanner" in names, f"Expected static-scanner in scanners: {names}"


def case_certify_empty_skill_dir(ws: Workspace):
    """Certify a skill dir with no SKILL.md → exit 1."""
    skill = ws.skills_dir / "empty-skill"
    skill.mkdir(parents=True, exist_ok=True)
    env = ws.env()
    r = run_skill_ledger(["certify", str(skill)], env_extra=env)
    assert r.returncode == 1, f"expected exit 1 for empty dir, got {r.returncode}"


# ── G10: SKILL.md contract assertions ────────────────────────────────────


def case_contract_init_empty_passphrase_env(ws: Workspace):
    """SKILL_LEDGER_PASSPHRASE="" → passphrase-free init."""
    alt_data = ws.root / "contract_keys"
    alt_data.mkdir()
    env = ws.env({"XDG_DATA_HOME": str(alt_data), "SKILL_LEDGER_PASSPHRASE": ""})
    r = run_skill_ledger(["init", "--no-baseline"], env_extra=env)
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)["key"]
    assert (
        out.get("encrypted") is False
    ), f"Empty passphrase should produce unencrypted keys, got {out}"
    key_pub = Path(alt_data) / "agent-sec" / "skill-ledger" / "key.pub"
    assert key_pub.exists(), f"key.pub not at expected path: {key_pub}"


def case_contract_check_output_schema(ws: Workspace):
    """check output is JSON with ``status`` field for every outcome."""
    env = ws.env()

    skill_none = make_skill(ws.skills_dir, "schema-none", {"a.txt": "a"})
    r = run_skill_ledger(["check", str(skill_none)], env_extra=env)
    out = parse_json_output(r.stdout)
    assert "status" in out, f"Missing 'status' field for none: {out}"
    assert out["status"] == "none"

    findings = write_findings_file(
        ws.fixtures,
        "schema-p.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill_none), "--findings", str(findings)], env_extra=env
    )
    r = run_skill_ledger(["check", str(skill_none)], env_extra=env)
    out = parse_json_output(r.stdout)
    assert "status" in out and out["status"] == "pass"

    (skill_none / "new.txt").write_text("new")
    r = run_skill_ledger(["check", str(skill_none)], env_extra=env)
    out = parse_json_output(r.stdout)
    assert out["status"] == "drifted"
    for diff_key in ("added", "removed", "modified"):
        assert diff_key in out, f"drifted output missing '{diff_key}': {out}"


def case_contract_certify_explicit_scanner_flags(ws: Workspace):
    """certify with explicit --scanner and --scanner-version flags."""
    skill = make_skill(ws.skills_dir, "contract-flags", {"run.sh": "echo hi"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "flags.json",
        [{"rule": "r1", "level": "pass", "message": "ok"}],
    )
    r = run_skill_ledger(
        [
            "certify",
            str(skill),
            "--findings",
            str(findings),
            "--scanner",
            "skill-vetter",
            "--scanner-version",
            "0.1.0",
        ],
        env_extra=env,
    )
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out.get("scanStatus") == "pass"


def case_contract_certify_output_fields(ws: Workspace):
    """certify output JSON contains versionId and scanStatus."""
    skill = make_skill(ws.skills_dir, "contract-output", {"data.py": "x = 1"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "out.json",
        [{"rule": "r1", "level": "warn", "message": "caution"}],
    )
    r = run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert "versionId" in out, f"Missing 'versionId': {out}"
    assert "scanStatus" in out, f"Missing 'scanStatus': {out}"
    vid = out["versionId"]
    assert (
        len(vid) == 7 and vid[0] == "v" and vid[1:].isdigit()
    ), f"Bad versionId: {vid}"
    assert out["scanStatus"] in (
        "pass",
        "warn",
        "deny",
        "none",
    ), f"Unexpected scanStatus '{out['scanStatus']}'"


def case_contract_manifest_path(ws: Workspace):
    """After certify, manifest exists at <SKILL_DIR>/.skill-meta/latest.json."""
    skill = make_skill(ws.skills_dir, "contract-path", {"f.txt": "content"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "path.json",
        [{"rule": "r1", "level": "pass", "message": "ok"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    latest = skill / ".skill-meta" / "latest.json"
    assert latest.exists(), f"Manifest not at expected path: {list(skill.rglob('*'))}"
    data = json.loads(latest.read_text())
    for manifest_field in ("versionId", "fileHashes", "scanStatus", "signature"):
        assert manifest_field in data, f"Missing '{manifest_field}' in manifest"


def case_contract_check_status_values_complete(ws: Workspace):
    """All 6 triage statuses are reachable: none, pass, drifted, warn, deny, tampered."""
    env = ws.env()
    observed: set[str] = set()

    s = make_skill(ws.skills_dir, "sv-none", {"x.txt": "x"})
    r = run_skill_ledger(["check", str(s)], env_extra=env)
    observed.add(parse_json_output(r.stdout)["status"])

    fp = write_findings_file(
        ws.fixtures,
        "sv-pass.json",
        [{"rule": "r", "level": "pass", "message": "ok"}],
    )
    run_skill_ledger(["certify", str(s), "--findings", str(fp)], env_extra=env)
    r = run_skill_ledger(["check", str(s)], env_extra=env)
    observed.add(parse_json_output(r.stdout)["status"])

    (s / "x.txt").write_text("changed")
    r = run_skill_ledger(["check", str(s)], env_extra=env)
    observed.add(parse_json_output(r.stdout)["status"])

    sw = make_skill(ws.skills_dir, "sv-warn", {"w.txt": "w"})
    fpw = write_findings_file(
        ws.fixtures,
        "sv-warn.json",
        [{"rule": "r", "level": "warn", "message": "w"}],
    )
    run_skill_ledger(["certify", str(sw), "--findings", str(fpw)], env_extra=env)
    r = run_skill_ledger(["check", str(sw)], env_extra=env)
    observed.add(parse_json_output(r.stdout)["status"])

    sd = make_skill(ws.skills_dir, "sv-deny", {"d.txt": "d"})
    fpd = write_findings_file(
        ws.fixtures,
        "sv-deny.json",
        [{"rule": "r", "level": "deny", "message": "d"}],
    )
    run_skill_ledger(["certify", str(sd), "--findings", str(fpd)], env_extra=env)
    r = run_skill_ledger(["check", str(sd)], env_extra=env)
    observed.add(parse_json_output(r.stdout)["status"])

    st = make_skill(ws.skills_dir, "sv-tamper", {"t.txt": "t"})
    fpt = write_findings_file(
        ws.fixtures,
        "sv-t.json",
        [{"rule": "r", "level": "pass", "message": "ok"}],
    )
    run_skill_ledger(["certify", str(st), "--findings", str(fpt)], env_extra=env)
    latest = st / ".skill-meta" / "latest.json"
    data = json.loads(latest.read_text())
    data["scanStatus"] = "deny"
    latest.write_text(json.dumps(data))
    r = run_skill_ledger(["check", str(st)], env_extra=env)
    observed.add(parse_json_output(r.stdout)["status"])

    expected = {"none", "pass", "drifted", "warn", "deny", "tampered"}
    assert observed == expected, (
        f"Not all triage statuses reachable.\n"
        f"  Expected: {expected}\n  Observed: {observed}\n"
        f"  Missing:  {expected - observed}"
    )


# ── G11: Passphrase-protected key lifecycle ──────────────────────────────


def case_passphrase_full_lifecycle(ws: Workspace):
    """Encrypted key: init → check → certify → check → audit — all work."""
    pp_data = ws.root / "pp_data"
    pp_data.mkdir()
    env = ws.env(
        {"XDG_DATA_HOME": str(pp_data), "SKILL_LEDGER_PASSPHRASE": "s3cret-test"}
    )

    r = run_skill_ledger(["init", "--no-baseline", "--passphrase"], env_extra=env)
    assert r.returncode == 0, f"init --no-baseline exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)["key"]
    assert out["encrypted"] is True

    skill = make_skill(ws.skills_dir, "pp-life", {"app.py": "pass\n"})

    r = run_skill_ledger(["check", str(skill)], env_extra=env)
    assert r.returncode == 0, f"check exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["status"] == "none"

    findings = write_findings_file(
        ws.fixtures,
        "pp.json",
        [{"rule": "ok", "level": "pass", "message": "ok"}],
    )
    r = run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    assert r.returncode == 0, f"certify exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["scanStatus"] == "pass"

    r = run_skill_ledger(["check", str(skill)], env_extra=env)
    assert r.returncode == 0
    out = parse_json_output(r.stdout)
    assert out["status"] == "pass"

    r = run_skill_ledger(["audit", str(skill)], env_extra=env)
    assert r.returncode == 0
    out = parse_json_output(r.stdout)
    assert out["valid"] is True


def case_passphrase_missing_env_fails(ws: Workspace):
    """Encrypted key without SKILL_LEDGER_PASSPHRASE → certify fails gracefully."""
    pp_data = ws.root / "pp_noenv"
    pp_data.mkdir()
    env_with = ws.env(
        {"XDG_DATA_HOME": str(pp_data), "SKILL_LEDGER_PASSPHRASE": "my-pass"}
    )
    r = run_skill_ledger(["init", "--no-baseline", "--passphrase"], env_extra=env_with)
    assert r.returncode == 0

    skill = make_skill(ws.skills_dir, "pp-noenv", {"f.txt": "data"})
    findings = write_findings_file(
        ws.fixtures,
        "pp-noenv.json",
        [{"rule": "ok", "level": "pass", "message": "ok"}],
    )

    # Remove passphrase from env — should fail.
    # start_new_session=True detaches the child from the controlling terminal,
    # so getpass.getpass() cannot open /dev/tty and falls back to stdin.
    # Piping "\n" gives it an empty (wrong) passphrase → decryption fails → exit != 0.
    env_without = ws.env({"XDG_DATA_HOME": str(pp_data)})
    env_without.pop("SKILL_LEDGER_PASSPHRASE", None)
    cmd = [CLI_BIN, "skill-ledger", "certify", str(skill), "--findings", str(findings)]
    r = subprocess.run(
        cmd,
        input="\n",
        capture_output=True,
        check=False,
        text=True,
        env=env_without,
        start_new_session=True,
        timeout=CLI_TIMEOUT_S,
    )
    if VERBOSE:
        print(f"  cmd: {' '.join(cmd[:5])} ... certify (no passphrase)")
        if r.stderr.strip():
            print(f"  stderr: {r.stderr.strip()[:200]}")
        print(f"  exit: {r.returncode}")
    assert (
        r.returncode != 0
    ), f"Expected failure without passphrase, got exit {r.returncode}"


# ── G12: cosh hook integration ───────────────────────────────────────────

# Source gates must exercise the checked-out hook, while direct RPM acceptance
# must keep validating the installed artifact.
_SOURCE_ROOT = Path(__file__).resolve().parents[3]
_SOURCE_HOOK = _SOURCE_ROOT / "cosh-extension" / "hooks" / "skill_ledger_hook.py"
_INSTALLED_HOOK = Path(
    "/usr/share/anolisa/extensions/agent-sec-core/hooks/skill_ledger_hook.py"
)
_CLI_PATH = Path(CLI_BIN).resolve() if CLI_BIN else None
_SOURCE_RUN = _CLI_PATH is not None and _CLI_PATH.is_relative_to(_SOURCE_ROOT)
_HOOK_OVERRIDE_RAW = os.environ.get("SKILL_LEDGER_E2E_HOOK_SCRIPT")
_HOOK_OVERRIDE = Path(_HOOK_OVERRIDE_RAW).expanduser() if _HOOK_OVERRIDE_RAW else None
if _HOOK_OVERRIDE is not None:
    _HOOK_SEARCH_PATHS = [_HOOK_OVERRIDE]
elif _SOURCE_RUN:
    _HOOK_SEARCH_PATHS = [_SOURCE_HOOK, _INSTALLED_HOOK]
else:
    _HOOK_SEARCH_PATHS = [_INSTALLED_HOOK, _SOURCE_HOOK]
HOOK_SCRIPT: str | None = None
for _p in _HOOK_SEARCH_PATHS:
    if _p.is_file():
        HOOK_SCRIPT = str(_p)
        break
if _HOOK_OVERRIDE is not None and HOOK_SCRIPT is None:
    raise FileNotFoundError(
        "SKILL_LEDGER_E2E_HOOK_SCRIPT does not point to a regular file: "
        f"{_HOOK_OVERRIDE}"
    )


def _run_hook_process(input_data, env_extra=None):
    """Pipe cosh event JSON into the hook script and return the process result."""
    env = os.environ.copy()
    if env_extra:
        env.update(env_extra)
    proc = subprocess.run(
        [sys.executable, HOOK_SCRIPT],
        input=(
            json.dumps(input_data) if isinstance(input_data, dict) else str(input_data)
        ),
        capture_output=True,
        text=True,
        check=False,
        timeout=CLI_TIMEOUT_S,
        env=env,
    )
    if VERBOSE:
        print(f"  hook stdout: {proc.stdout.strip()[:200]}")
        if proc.stderr.strip():
            print(f"  hook stderr: {proc.stderr.strip()[:200]}")
    return proc


def _run_hook(input_data, env_extra=None):
    """Pipe cosh event JSON into the hook script, return parsed output."""
    proc = _run_hook_process(input_data, env_extra=env_extra)
    return json.loads(proc.stdout)


def _hook_env(env_extra: dict | None = None, *, policy: str) -> dict:
    """Return a hook environment with an explicit Skill Ledger hook policy."""
    env = dict(env_extra or {})
    env["SKILL_LEDGER_MODE"] = policy
    return env


def _add_skill_context(event: dict, skill_name: str, skill_dir: Path | None) -> dict:
    """Attach the Host-resolved SKILL.md path when supplied."""
    if skill_dir is not None:
        event["skill_context"] = {
            "skill_name": skill_name,
            "file_path": str(skill_dir / "SKILL.md"),
        }
    return event


def _make_cosh_event(
    skill_name: str, cwd: str, *, skill_dir: Path | None = None
) -> dict:
    """Build a legacy Cosh PreToolUse JSON event."""
    event = {
        "session_id": "test-session",
        "hook_event_name": "PreToolUse",
        "tool_name": "skill",
        "tool_input": {"skill": skill_name},
        "cwd": cwd,
    }
    return _add_skill_context(event, skill_name, skill_dir)


def _make_cosh_ng_event(
    skill_name: str, cwd: str, *, skill_dir: Path | None = None
) -> dict:
    """Build a Cosh-NG invoke PreToolUse JSON event."""
    event = {
        "session_id": "test-session",
        "hook_event_name": "PreToolUse",
        "tool_name": "skill",
        "tool_input": {"action": "invoke", "name": skill_name},
        "cwd": cwd,
    }
    return _add_skill_context(event, skill_name, skill_dir)


def _make_host_skill_events(
    skill_name: str, cwd: str, skill_dir: Path
) -> list[tuple[str, dict]]:
    """Return equivalent legacy Cosh and Cosh-NG invoke events."""
    return [
        (
            "legacy-cosh",
            _make_cosh_event(skill_name, cwd, skill_dir=skill_dir),
        ),
        (
            "cosh-ng",
            _make_cosh_ng_event(skill_name, cwd, skill_dir=skill_dir),
        ),
    ]


def case_hook_invalid_json_allows():
    """Malformed stdin → fail-open allow."""
    output = _run_hook("not-json")
    assert output == {"decision": "allow"}


def case_hook_wrong_tool_allows():
    """Non-skill tool → allow."""
    output = _run_hook(
        {
            "tool_name": "run_shell_command",
            "tool_input": {"command": "echo hi"},
        }
    )
    assert output == {"decision": "allow"}


def case_hook_unknown_skill_policy_modes():
    """Skill not found before summary resolution: fail-open regardless of policy."""
    for policy in ("observe", "warn", "ask", "block"):
        output = _run_hook(
            _make_cosh_event("nonexistent-skill-xyz", "/tmp"),
            env_extra=_hook_env(policy=policy),
        )
        assert output == {"decision": "allow"}, f"{policy}: {output}"


def case_hook_pass_status_silent(ws: Workspace):
    """Both Host payloads allow a pass-status skill without a reason."""
    skill = make_skill(ws.hook_skills_dir, "hook-pass", {"m.txt": "main"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "hook-p.json",
        [{"rule": "ok", "level": "pass", "message": "ok"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    for profile, event in _make_host_skill_events("hook-pass", str(ws.root), skill):
        output = _run_hook(event, env_extra=env)
        assert output["decision"] == "allow", f"{profile}: {output}"
        assert (
            "reason" not in output
        ), f"{profile}: expected silent allow, got reason: {output}"


def case_hook_drifted_policy_modes(ws: Workspace):
    """Both Host payloads apply ask/debug/warn/block to a drifted skill."""
    skill = make_skill(ws.hook_skills_dir, "hook-drift", {"f.txt": "original"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "hook-d.json",
        [{"rule": "ok", "level": "pass", "message": "ok"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    (skill / "f.txt").write_text("MODIFIED")
    for profile, event in _make_host_skill_events("hook-drift", str(ws.root), skill):
        output = _run_hook(event, env_extra=env)
        assert output["decision"] == "ask", f"{profile}: {output}"
        assert (
            "reason" in output
        ), f"{profile}: expected confirmation reason for drifted: {output}"
        assert (
            "drifted" in output["reason"].lower()
            or "changed" in output["reason"].lower()
        ), f"{profile}: {output}"

        output = _run_hook(
            event,
            env_extra=_hook_env(env, policy="debug"),
        )
        assert output == {"decision": "allow"}, f"{profile}: {output}"

        output = _run_hook(
            event,
            env_extra=_hook_env(env, policy="warn"),
        )
        assert output["decision"] == "allow", f"{profile}: {output}"
        assert (
            "reason" in output
        ), f"{profile}: expected visible warning for drifted: {output}"
        assert (
            "drifted" in output["reason"].lower()
            or "changed" in output["reason"].lower()
        ), f"{profile}: {output}"

        output = _run_hook(
            event,
            env_extra=_hook_env(env, policy="block"),
        )
        assert output["decision"] == "block", f"{profile}: {output}"
        assert (
            "reason" in output
        ), f"{profile}: expected block reason for drifted: {output}"
        assert (
            "drifted" in output["reason"].lower()
            or "changed" in output["reason"].lower()
        ), f"{profile}: {output}"


def case_hook_cosh_ng_list_silent_allow(ws: Workspace):
    """Cosh-NG list skips identity checks, policy reads, keys, and CLI calls."""
    skill = make_skill(ws.hook_skills_dir, "hook-list-probe", {"f.txt": "main"})
    marker = ws.root / "unexpected-cli-call"
    fake_bin = ws.root / "fake-bin"
    fake_bin.mkdir()
    fake_cli = fake_bin / "agent-sec-cli"
    fake_cli.write_text(
        '#!/bin/sh\n: > "$SKILL_LEDGER_E2E_CLI_MARKER"\nexit 97\n',
        encoding="utf-8",
    )
    fake_cli.chmod(0o755)

    event = _make_cosh_ng_event(
        "ignored-list-name",
        str(ws.root),
        skill_dir=skill,
    )
    event["tool_input"] = {
        "action": "list",
        "name": "ignored-list-name",
        "skill": "hook-list-probe",
    }
    result = _run_hook_process(
        event,
        env_extra=ws.env(
            {
                "PATH": str(fake_bin),
                "SKILL_LEDGER_E2E_CLI_MARKER": str(marker),
                "SKILL_LEDGER_MODE": "invalid-list-probe-policy",
            }
        ),
    )

    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout) == {"decision": "allow"}
    assert (
        result.stderr == ""
    ), f"list emitted identity/policy diagnostics: {result.stderr}"
    assert not marker.exists(), "list unexpectedly invoked agent-sec-cli"
    key_dir = ws.xdg_data / "agent-sec" / "skill-ledger"
    assert not (key_dir / "key.pub").exists(), "list unexpectedly initialized keys"
    assert not (key_dir / "key.enc").exists(), "list unexpectedly initialized keys"


def case_hook_path_traversal_policy_modes(ws: Workspace):
    """Path traversal before summary resolution fails open without HookOutput reason."""
    for policy in ("observe", "warn", "ask", "block"):
        output = _run_hook(
            _make_cosh_event("../../etc/passwd", "/tmp"),
            env_extra=_hook_env(ws.env(), policy=policy),
        )
        assert output == {"decision": "allow"}, f"{policy}: {output}"


# ── G13: Full pipeline (vetter → ledger → hook) ─────────────────────────


def case_full_pipeline_vetter_to_hook(ws: Workspace):
    """End-to-end: create → certify(pass) → hook policy behavior."""
    skill = make_skill(ws.hook_skills_dir, "pipeline-full", {"app.py": "print(1)\n"})
    env = ws.env()

    # check → none
    r = run_skill_ledger(["check", str(skill)], env_extra=env)
    out = parse_json_output(r.stdout)
    assert out["status"] == "none"

    # certify → pass
    findings = write_findings_file(
        ws.fixtures,
        "pipe.json",
        [{"rule": "no-exec", "level": "pass", "message": "ok"}],
    )
    r = run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    assert r.returncode == 0
    out = parse_json_output(r.stdout)
    assert out["scanStatus"] == "pass"

    # hook → silent allow
    if HOOK_SCRIPT:
        output = _run_hook(
            _make_cosh_event("pipeline-full", str(ws.root)),
            env_extra=env,
        )
        assert output == {"decision": "allow"}, f"Expected silent allow: {output}"

    # Modify file → drifted
    (skill / "app.py").write_text("print(2)\n")

    # default hook policy → ask for drifted status
    if HOOK_SCRIPT:
        output = _run_hook(
            _make_cosh_event("pipeline-full", str(ws.root)),
            env_extra=env,
        )
        assert output["decision"] == "ask", f"Expected default ask: {output}"
        assert "reason" in output, f"Expected drift confirmation: {output}"

    # debug hook policy → silent allow for drifted status
    if HOOK_SCRIPT:
        output = _run_hook(
            _make_cosh_event("pipeline-full", str(ws.root)),
            env_extra=_hook_env(env, policy="debug"),
        )
        assert output == {"decision": "allow"}, f"Expected debug allow: {output}"

    # block hook policy → reject with warning reason
    if HOOK_SCRIPT:
        output = _run_hook(
            _make_cosh_event("pipeline-full", str(ws.root)),
            env_extra=_hook_env(env, policy="block"),
        )
        assert output["decision"] == "block"
        assert "reason" in output, f"Expected drift block: {output}"


# ── G14: Key rotation ────────────────────────────────────────────────────


def case_key_rotation_old_sigs_verifiable(ws: Workspace):
    """After init --no-baseline --force-keys, old signatures must still pass ``check``."""
    env = ws.env()

    s = make_skill(ws.skills_dir, "rotate-test", {"a.txt": "a"})
    fp = write_findings_file(
        ws.fixtures,
        "rotate.json",
        [{"rule": "r", "level": "pass", "message": "ok"}],
    )
    r = run_skill_ledger(["certify", str(s), "--findings", str(fp)], env_extra=env)
    assert r.returncode == 0, f"certify failed: {r.stderr}"

    pub_path = Path(env["XDG_DATA_HOME"]) / "agent-sec" / "skill-ledger" / "key.pub"
    old_fp = "sha256:" + hashlib.sha256(pub_path.read_bytes()).hexdigest()

    r = run_skill_ledger(["check", str(s)], env_extra=env)
    out = parse_json_output(r.stdout)
    assert out["status"] == "pass", f"Expected pass before rotation, got {out}"

    r = run_skill_ledger(["init", "--no-baseline", "--force-keys"], env_extra=env)
    assert r.returncode == 0, f"init --no-baseline --force-keys failed: {r.stderr}"
    new_fp = parse_json_output(r.stdout)["key"]["fingerprint"]
    assert (
        new_fp != old_fp
    ), f"Key rotation must produce a different fingerprint: old={old_fp}, new={new_fp}"
    assert new_fp.startswith("sha256:")

    r = run_skill_ledger(["check", str(s)], env_extra=env)
    out = parse_json_output(r.stdout)
    assert (
        out["status"] != "tampered"
    ), f"Old signature should still verify after key rotation, got {out['status']}"
    assert (
        out["status"] == "pass"
    ), f"Expected 'pass' for unchanged skill after key rotation, got '{out['status']}'"


def _without_workspace(fn: Callable[[], None]) -> Callable[[Workspace], None]:
    """Adapt hook-only cases to the shared case registry shape."""

    def wrapped(_ws: Workspace) -> None:
        fn()

    return wrapped


E2E_CASES = [
    E2ECase(
        "G1: --help available",
        case_help_available,
        init_default_keys=False,
    ),
    E2ECase(
        "G2: init --no-baseline no passphrase",
        case_init_no_passphrase,
        init_default_keys=False,
    ),
    E2ECase("G2: init --no-baseline JSON structure", case_init_json_structure),
    E2ECase(
        "G2: init --no-baseline reuse existing keys", case_init_reuses_existing_keys
    ),
    E2ECase("G2: init --no-baseline --force-keys overwrite", case_init_force_overwrite),
    E2ECase("G2: init --no-baseline passphrase env", case_init_with_passphrase_env),
    E2ECase("G3: full pass lifecycle", case_full_lifecycle_pass),
    E2ECase("G3: multi-version chain", case_multi_version_lifecycle),
    E2ECase("G3: warn findings lifecycle", case_lifecycle_with_warn_findings),
    E2ECase("G4: no manifest → none/read-only", case_check_no_manifest_is_read_only),
    E2ECase("G4: file added → drifted", case_check_after_file_add_drifted),
    E2ECase("G4: file modified → drifted", case_check_after_file_modify_drifted),
    E2ECase("G4: file removed → drifted", case_check_after_file_remove_drifted),
    E2ECase("G4: tampered → exit 1", case_check_tampered_manifest_hash),
    E2ECase("G4: deny → exit 1", case_check_deny_exit_code_1),
    E2ECase("G5: bare array findings", case_certify_external_findings_bare_array),
    E2ECase("G5: wrapped findings", case_certify_external_findings_wrapped),
    E2ECase("G5: deny finding", case_certify_deny_finding_produces_deny),
    E2ECase("G5: missing findings file", case_certify_missing_findings_file),
    E2ECase("G5: invalid JSON", case_certify_invalid_json_findings),
    E2ECase("G5: scan auto-invoke mode", case_scan_auto_invoke_default_scanners),
    E2ECase("G5: no skill_dir no --all", case_certify_no_skill_dir_no_all),
    E2ECase("G6: --all multiple skills", case_scan_all_multiple_skills),
    E2ECase("G6: --all no skill dirs", case_scan_all_no_skill_dirs),
    E2ECase("G7: valid chain", case_audit_valid_chain),
    E2ECase("G7: no versions", case_audit_no_versions),
    E2ECase("G7: tampered version file", case_audit_tampered_version_file),
    E2ECase("G7: --verify-snapshots", case_audit_verify_snapshots),
    E2ECase("G8: human-readable output", case_status_human_readable_output),
    E2ECase("G8: drifted details", case_status_drifted_shows_details),
    E2ECase("G9: rotate-keys unavailable", case_rotate_keys_not_implemented),
    E2ECase("G9: list-scanners", case_list_scanners),
    E2ECase("G9: certify empty skill dir", case_certify_empty_skill_dir),
    E2ECase("G10: empty passphrase env", case_contract_init_empty_passphrase_env),
    E2ECase("G10: check output schema", case_contract_check_output_schema),
    E2ECase(
        "G10: certify --scanner flags", case_contract_certify_explicit_scanner_flags
    ),
    E2ECase("G10: certify output fields", case_contract_certify_output_fields),
    E2ECase("G10: manifest path", case_contract_manifest_path),
    E2ECase(
        "G10: all 6 statuses reachable", case_contract_check_status_values_complete
    ),
    E2ECase("G11: passphrase full lifecycle", case_passphrase_full_lifecycle),
    E2ECase("G11: missing passphrase fails", case_passphrase_missing_env_fails),
    E2ECase(
        "G12: hook invalid JSON → allow",
        _without_workspace(case_hook_invalid_json_allows),
        requires_hook=True,
        init_default_keys=False,
    ),
    E2ECase(
        "G12: hook wrong tool → allow",
        _without_workspace(case_hook_wrong_tool_allows),
        requires_hook=True,
        init_default_keys=False,
    ),
    E2ECase(
        "G12: hook unknown skill fail-open",
        _without_workspace(case_hook_unknown_skill_policy_modes),
        requires_hook=True,
        init_default_keys=False,
    ),
    E2ECase(
        "G12: hook pass → silent allow",
        case_hook_pass_status_silent,
        requires_hook=True,
    ),
    E2ECase(
        "G12: hook drifted ask/debug/warn/block",
        case_hook_drifted_policy_modes,
        requires_hook=True,
    ),
    E2ECase(
        "G12: Cosh-NG list → silent allow",
        case_hook_cosh_ng_list_silent_allow,
        requires_hook=True,
        init_default_keys=False,
    ),
    E2ECase(
        "G12: hook path traversal fail-open",
        case_hook_path_traversal_policy_modes,
        requires_hook=True,
        init_default_keys=False,
    ),
    E2ECase("G13: vetter→ledger→hook pipeline", case_full_pipeline_vetter_to_hook),
    E2ECase(
        "G14: old sigs verifiable after rotation", case_key_rotation_old_sigs_verifiable
    ),
]


def _ensure_default_keys(ws: Workspace) -> None:
    """Initialize default test keys for isolated pytest case workspaces."""
    key_path = ws.xdg_data / "agent-sec" / "skill-ledger" / "key.pub"
    if key_path.exists():
        return
    r = run_skill_ledger(["init", "--no-baseline"], env_extra=ws.env())
    assert r.returncode == 0, f"init --no-baseline preflight failed: {r.stderr}"


# ── Pytest entry points ─────────────────────────────────────────────────────


def _case_id(case: E2ECase) -> str:
    """Build a stable, readable pytest id from the G-case name."""
    return re.sub(r"[^A-Za-z0-9]+", "_", case.name).strip("_")


@pytest.fixture
def ws():
    """Provide each pytest case with an isolated skill-ledger workspace."""
    workspace = Workspace()
    try:
        yield workspace
    finally:
        workspace.cleanup()


@pytest.mark.parametrize("case", E2E_CASES, ids=_case_id)
def test_skill_ledger_e2e_case(case: E2ECase, ws: Workspace):
    """Run one skill-ledger E2E scenario as its own pytest item."""
    if not CLI_BIN:
        pytest.fail(
            "agent-sec-cli not found on PATH; install the RPM package or ensure "
            "the binary is on PATH"
        )
    if case.requires_hook and HOOK_SCRIPT is None:
        pytest.skip("cosh hook script not found")
    if case.init_default_keys:
        _ensure_default_keys(ws)
    case.fn(ws)


def main(argv: list[str] | None = None) -> int:
    """CLI entry point for running this pytest module directly."""
    global VERBOSE

    parser = argparse.ArgumentParser(description="skill-ledger CLI E2E tests (RPM)")
    parser.add_argument("-v", "--verbose", action="store_true", help="Show CLI output")
    args, pytest_args = parser.parse_known_args(argv)
    VERBOSE = args.verbose
    if args.verbose and "-s" not in pytest_args and "--capture=no" not in pytest_args:
        pytest_args = ["-s", *pytest_args]
    return pytest.main([__file__, *pytest_args])


if __name__ == "__main__":
    sys.exit(main())

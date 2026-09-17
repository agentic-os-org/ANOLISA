"""Exercise the installed Rust core without importing the Python Ledger implementation."""

import json
from pathlib import Path
from typing import Any


def test_installed_skillguard_lifecycle_and_safe_audit(
    daemon: Any, tmp_path: Path
) -> None:
    skill = tmp_path / "fixture"
    skill.mkdir()
    (skill / "SKILL.md").write_text(
        "---\nname: fixture\ndescription: Installed SkillGuard fixture\n---\nSafe content\n"
    )
    script = skill / "run.sh"
    script.write_text("echo initial\n")
    path = str(skill)

    def run(*args: str) -> dict[str, Any]:
        return daemon.request("skill-ledger", *args)

    assert run("status")["keys"]["initialized"] is False
    assert run("analyze", path)["coverage_complete"] is True
    assert not (skill / ".skill-meta").exists()
    assert run("status")["keys"]["initialized"] is False
    run("init", "--no-baseline")
    assert not (skill / ".skill-meta").exists()
    scanners = {item["name"] for item in run("list-scanners")["scanners"]}
    assert {"code-scanner", "static-scanner"} <= scanners

    scanned = run("scan", path)
    assert scanned["versionId"] == "v000001"
    assert scanned["activation"]["activationPending"] is False
    assert run("check", path)["status"] == "pass"
    assert run("scan", path)["status"] == "noop"
    assert run("scan", path, "--force")["versionId"] == "v000001"
    script.write_text("echo changed\n")
    assert run("check", path)["status"] == "drifted"
    assert run("scan", path)["versionId"] == "v000002"

    findings = tmp_path / "findings.json"
    findings.write_text(
        json.dumps(
            [{"rule": "synthetic-risk", "level": "deny", "message": "PRIVATE_FINDING"}]
        )
    )
    run("certify", path, "--findings", str(findings), "--delete-findings")
    assert not findings.exists()
    denied = daemon.cli("skill-ledger", "check", path)
    assert denied.returncode == 1
    assert json.loads(denied.stdout)["status"] == "deny"

    run("decide", path, "--action", "allow", "--reason", "PRIVATE_REASON")
    assert run("show", path)["exposureState"] == "active"
    run("decide", path, "--action", "always_allow")
    run("decide", path, "--action", "block")
    assert run("show", path)["exposureState"] == "hidden"
    run("decide", path, "--clear")
    run("decide", path, "--action", "rollback", "--version", "v000001")
    assert script.read_text() == "echo initial\n"
    output = tmp_path / "export"
    run("export", path, "--version", "v000001", "--output", str(output))
    assert (output / "snapshot" / "run.sh").read_text() == "echo initial\n"
    assert run("audit", path, "--verify-snapshots")["valid"] is True
    run("activate", path)

    old_fingerprint = run("status")["keys"]["fingerprint"]
    rotation = run("rotate-keys")
    assert rotation["keyFingerprint"] != old_fingerprint
    assert rotation["previousKeyRetained"] is False
    invalidated = daemon.cli("skill-ledger", "check", path)
    assert invalidated.returncode == 1
    assert json.loads(invalidated.stdout)["status"] == "tampered"
    run("scan", path)
    assert run("check", path)["status"] == "pass"
    assert run("status", "--verbose")["skills"]["discovered"] == 1

    audit_path = daemon.socket_path.with_suffix(".audit") / "security-events.jsonl"
    events = [json.loads(line) for line in audit_path.read_text().splitlines()]
    assert events
    for event in events:
        assert event["event_type"] == "skill_ledger"
        assert event["uid"] == 0
        details = json.dumps(event["details"])
        for private_value in (
            "PRIVATE_FINDING",
            "PRIVATE_REASON",
            "echo initial",
            path,
        ):
            assert private_value not in details

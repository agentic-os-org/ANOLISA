"""Exercise the installed Rust core without importing the Python Ledger implementation."""

import json
import os
from pathlib import Path
from typing import Any


def test_installed_skillsec_lifecycle_and_safe_audit(
    daemon: Any, tmp_path: Path
) -> None:
    skill = tmp_path / "fixture"
    skill.mkdir()
    (skill / "SKILL.md").write_text(
        "---\nname: fixture\ndescription: Installed SkillSec fixture\n---\nSafe content\n"
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


def test_skillsec_and_both_scanners_share_the_configured_daemon(
    pii_environment: tuple[Path, Path], start_daemon: Any, tmp_path: Path
) -> None:
    rules = tmp_path / "rules.yaml"
    rules.write_text(
        '- type: internal_reference\n  regex: "REF-[0-9]{4}"\n  severity: deny\n'
    )
    daemon = start_daemon(admin_uids=[], pii_rules=rules)
    pii = daemon.request("scan-pii", "--text", "REF-1234")
    assert pii["verdict"] == "deny"
    assert pii["summary"]["custom_rules"]["status"] == "loaded"
    assert [finding["type"] for finding in pii["findings"]] == ["internal_reference"]
    code = daemon.request("scan-code", "--code", "rm -rf /tmp/skillsec-fixture")
    assert code["ok"] and code["verdict"] == "warn"

    skill = tmp_path / "skill"
    skill.mkdir()
    (skill / "SKILL.md").write_text(
        "---\nname: fixture\ndescription: Combined runtime fixture\n---\nSafe content\n"
    )
    assert daemon.request("skill-ledger", "analyze", str(skill))["coverage_complete"]
    assert not daemon.request("skill-ledger", "status")["keys"]["initialized"]
    assert not (skill / ".skill-meta").exists()

    events = [
        json.loads(line)
        for line in (pii_environment[0] / "security-events.jsonl")
        .read_text()
        .splitlines()
    ]
    assert [event["event_type"] for event in events] == [
        "pii_scan",
        "code_scan",
        "skill_ledger",
        "skill_ledger",
    ]
    assert all(event["uid"] == os.getuid() for event in events)
    assert len({event["event_id"] for event in events}) == 4
    assert "REF-1234" not in json.dumps(events)

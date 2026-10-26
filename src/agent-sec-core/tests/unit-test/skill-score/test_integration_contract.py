"""Platform handoff and current scanner compatibility without model calls."""

import importlib.util
import io
import json
import tarfile
from pathlib import Path
from typing import Any

import pytest
import test_two_stage
from test_two_stage import FakeJudge, FakeRuntime, make_task, scan
from typer.testing import CliRunner

from agent_sec_cli.security_middleware.result import ActionResult
from agent_sec_cli.skill_score import cli, pipeline, scanners
from agent_sec_cli.skill_score.config import ScoreConfig
from agent_sec_cli.skill_score.contracts import ContractError
from agent_sec_cli.skill_score.entrypoint import app
from agent_sec_cli.skill_score.io import load_source
from agent_sec_cli.skill_score.task import task_json

fake = test_two_stage.fake
subject = test_two_stage.subject


@pytest.mark.parametrize("change", ["text", "binary", "rename", "delete", "add", "symlink"])
def test_changed_skill_is_rejected_before_runtime(
    subject: Path, tmp_path: Path, fake: tuple[FakeJudge, FakeRuntime], change: str
) -> None:
    original = subject / "input.bin"
    original.write_bytes(b"\x00original")
    task = CliRunner().invoke(cli.app, ["plan", str(subject)]).stdout
    if change == "text":
        (subject / "SKILL.md").write_text("Changed task content")
    elif change == "binary":
        original.write_bytes(b"\x00changed")
    elif change == "rename":
        original.rename(subject / "renamed.bin")
    elif change == "delete":
        original.unlink()
    elif change == "add":
        (subject / "additional.txt").write_text("New implementation")
    else:
        (subject / "external").symlink_to(tmp_path / "outside")
    output = tmp_path / "report.json"
    result = CliRunner().invoke(cli.app, ["run", task, "--output-file", str(output)])
    assert result.exit_code == 2 and result.stdout == ""
    assert "content changed" in result.stderr
    assert not output.exists()
    assert fake[1].inventory_calls == 0 and not fake[1].calls


def test_hash_exclusions_are_explicit(subject: Path) -> None:
    before = load_source(subject)["content_hash"]
    for directory in (".git", ".venv", "__pycache__", "node_modules"):
        (subject / directory).mkdir()
        (subject / directory / "ignored").write_text("Excluded from assessment")
    assert load_source(subject)["content_hash"] == before


def test_report_version_and_environment_failure_keep_separate_semantics(
    subject: Path, tmp_path: Path
) -> None:
    task = make_task(subject, tmp_path)
    report = pipeline.run_task(
        task,
        tmp_path / "run",
        ScoreConfig(),
        judge=FakeJudge(),
        runtime=FakeRuntime(incomplete=True),
        content_scanner=scan,
        source_redactor=lambda value: value,
    )
    assert report["schema_version"] == "agent-sec-skill-score/report-v1"
    assert report["content_hash"] == task.content_hash
    assert report["limitations"]
    assert report["execution"][0]["completed"] is False


def test_dedicated_entrypoint_keeps_public_command(
    subject: Path, fake: tuple[FakeJudge, FakeRuntime]
) -> None:
    result = CliRunner().invoke(app, ["skill-score", "plan", str(subject)])
    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout)["schema_version"] == "agent-sec-skill-score/task-v1"


def test_cli_exports_planning_notes_without_dynamic_limitations(
    subject: Path, tmp_path: Path, fake: tuple[FakeJudge, FakeRuntime]
) -> None:
    task = make_task(subject, tmp_path)
    task.audit.limitations = ["No untrusted input is present in this bounded Skill"]
    output, evidence = tmp_path / "report.json", tmp_path / "evidence"
    result = CliRunner().invoke(
        cli.app,
        [
            "run",
            json.dumps(task_json(subject, task.content_hash, task.audit)),
            "--output-file",
            str(output),
            "--evidence-dir",
            str(evidence),
        ],
    )
    assert result.exit_code == 0, result.output
    report = json.loads(result.stdout)
    assert json.loads(output.read_text()) == report
    assert report["planning_notes"] == task.audit.limitations
    assert "limitations" not in report
    assert report["execution"][0]["completed"]
    internal = json.loads((evidence / "internal-report.json").read_text())
    assert internal["planning_notes"] == report["planning_notes"]
    assert internal["limitations"] == []
    preserved_task = json.loads((evidence / "task.json").read_text())
    assert preserved_task["audit"]["limitations"] == task.audit.limitations


@pytest.mark.parametrize("failure", ["error", "degraded", "truncated", "missing"])
def test_incomplete_native_scanner_cannot_be_treated_as_clean(
    monkeypatch: pytest.MonkeyPatch, failure: str
) -> None:
    data = dict(
        verdict="pass", degraded=False, input_truncated=False, findings=[], layers_failed=[]
    )
    if failure == "error":
        data["verdict"] = "error"
    elif failure == "degraded":
        data["degraded"] = True
    elif failure == "truncated":
        data["input_truncated"] = True
    else:
        data.pop("degraded")
    monkeypatch.setattr(
        scanners.PromptScanBackend, "execute", lambda *a, **k: ActionResult(success=True, data=data)
    )
    with pytest.raises(ContractError, match="incomplete coverage"):
        scanners.scan_content(
            [{"text": "Harmless input", "ref": "source:SKILL.md:1"}], ScoreConfig()
        )


def test_current_native_scanner_findings_are_preserved(monkeypatch: pytest.MonkeyPatch) -> None:
    hit = {"rule_id": "test-rule", "title": "Test observation"}
    calls = []

    def execute(*args: Any, **kwargs: Any) -> ActionResult:
        calls.append(kwargs)
        return ActionResult(
            success=True,
            data=dict(
                verdict="warn",
                degraded=False,
                input_truncated=False,
                layers_failed=[],
                findings=[hit],
            ),
        )

    monkeypatch.setattr(scanners.PromptScanBackend, "execute", execute)
    result = scanners.scan_content(
        [{"text": "Harmless input", "ref": "source:SKILL.md:1"}], ScoreConfig()
    )
    assert result["signals"] == [{"scanner": "prompt", "ref": "source:SKILL.md:1", "detail": hit}]
    assert calls == [{"text": "Harmless input", "mode": "fast"}]


def release_verifier() -> Any:
    path = Path(__file__).resolve().parents[3] / "packaging/skill-score/verify-release.py"
    spec = importlib.util.spec_from_file_location("score_release", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_assessment_package_cannot_own_system_cli() -> None:
    module = release_verifier()
    prefix = "/opt/agent-sec/skill-score-cli"
    module.validate_paths([prefix, prefix + "/bin/agent-sec-cli"], prefix)
    for unsafe in (
        "/usr/bin/agent-sec-cli",
        "/opt/agent-sec/skill-score-cli-other/bin/python",
        "/etc/systemd/system/agent-sec-core.service",
    ):
        with pytest.raises(ValueError, match="must own only"):
            module.validate_paths([prefix, unsafe], prefix)


@pytest.mark.parametrize("unsafe", ["traversal", "absolute", "link", "device", "setuid"])
def test_payload_archive_rejects_unsafe_members(tmp_path: Path, unsafe: str) -> None:
    path = tmp_path / "payload.tar.gz"
    member = tarfile.TarInfo("opt/agent-sec/skill-score-cli/bin/test")
    if unsafe == "traversal":
        member.name = "opt/agent-sec/skill-score-cli/../../../usr/bin/test"
    elif unsafe == "absolute":
        member.name = "/usr/bin/agent-sec-cli"
    elif unsafe == "link":
        member.type, member.linkname = tarfile.SYMTYPE, "/etc/passwd"
    elif unsafe == "device":
        member.type = tarfile.CHRTYPE
    else:
        member.mode = 0o4755
    with tarfile.open(path, "w:gz") as archive:
        archive.addfile(member, io.BytesIO())
    with pytest.raises(ValueError, match="payload"):
        release_verifier().validate_archive(path)


@pytest.mark.parametrize("case", ["chain", "cycle", "hardlink", "duplicate"])
def test_payload_archive_resolves_links_before_parent_traversal(tmp_path: Path, case: str) -> None:
    prefix = "opt/agent-sec/skill-score-cli/"
    links = [("a", ".", tarfile.SYMTYPE), ("b", "a/../outside", tarfile.SYMTYPE)]
    if case == "cycle":
        links = [("a", "b", tarfile.SYMTYPE), ("b", "a", tarfile.SYMTYPE)]
    elif case == "hardlink":
        links[1] = ("b", prefix + "a/../outside", tarfile.LNKTYPE)
    elif case == "duplicate":
        links = [("a", ".", tarfile.SYMTYPE), ("a", ".", tarfile.SYMTYPE)]
    path = tmp_path / "payload.tar.gz"
    with tarfile.open(path, "w:gz") as archive:
        for name, target, kind in links:
            member = tarfile.TarInfo(prefix + name)
            member.type, member.linkname = kind, target
            archive.addfile(member)
    with pytest.raises(ValueError, match="payload"):
        release_verifier().validate_archive(path)


def test_payload_archive_allows_bundled_python_relative_links(tmp_path: Path) -> None:
    prefix = "opt/agent-sec/skill-score-cli/"
    path = tmp_path / "payload.tar.gz"
    with tarfile.open(path, "w:gz") as archive:
        archive.addfile(tarfile.TarInfo(prefix + "bin/python3.11"), io.BytesIO())
        for name, target, kind in (
            ("bin/python3", "python3.11", tarfile.SYMTYPE),
            ("bin/python", "python3", tarfile.SYMTYPE),
            ("lib/python", "../bin/python", tarfile.SYMTYPE),
            ("bin/python-hard", prefix + "bin/python3.11", tarfile.LNKTYPE),
        ):
            member = tarfile.TarInfo(prefix + name)
            member.type, member.linkname = kind, target
            archive.addfile(member)
    release_verifier().validate_archive(path)

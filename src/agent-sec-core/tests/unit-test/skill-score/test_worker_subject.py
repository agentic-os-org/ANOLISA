"""Canonical source deployment and mutation checks without Docker or model requests."""

import asyncio
import json
import os
import shlex
import shutil
import socket
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from agent_sec_cli.skill_score.config import ScoreConfig
from agent_sec_cli.skill_score.contracts import ContractError, Probe
from agent_sec_skill_score_runtime import subject


def source_tree(root: Path) -> Path:
    source = root / "source"
    (source / "scripts").mkdir(parents=True)
    (source / "scripts").chmod(0o755)
    (source / "SKILL.md").write_bytes(b"# Example\n")
    (source / "SKILL.md").chmod(0o644)
    (source / "scripts/run.py").write_bytes(b"print('receipt')\n")
    (source / "scripts/run.py").chmod(0o711)
    return source


def test_source_identity_matches_existing_rust_tree_digest(
    tmp_path: Path,
) -> None:
    source = source_tree(tmp_path)
    identity = subject.source_identity(source)
    assert (
        identity["content_hash"]
        == "3408ce997305a128beeae01ed15be710296246099684f93c650a599a86cd4ea9"
    )
    assert identity["members"][-1]["exec"] == 0o111
    copy = tmp_path / "copy"
    shutil.copytree(source, copy)
    assert subject.verify_source(copy, identity)["verified"]
    (copy / "scripts/run.py").chmod(0o600)
    with pytest.raises(ValueError, match="does not match"):
        subject.verify_source(copy, identity)


@pytest.mark.parametrize(
    "kind", ["symlink", "directory-link", "hardlink", "fifo", "ancestor-link"]
)
def test_source_deployment_rejects_unsafe_paths(tmp_path: Path, kind: str) -> None:
    source = source_tree(tmp_path)
    if kind == "symlink":
        (source / "link").symlink_to(source / "SKILL.md")
    elif kind == "directory-link":
        (source / "link").symlink_to(source / "scripts", target_is_directory=True)
    elif kind == "hardlink":
        os.link(source / "SKILL.md", source / "link")
    elif kind == "fifo":
        os.mkfifo(source / "fifo")
    else:
        (tmp_path / "alias").symlink_to(source, target_is_directory=True)
        source = tmp_path / "alias"
    with pytest.raises((OSError, ValueError)):
        subject.source_identity(source)


@pytest.mark.parametrize(
    "limit", ["FILE_LIMIT", "TREE_LIMIT", "ENTRY_LIMIT", "DEPTH_LIMIT"]
)
def test_source_deployment_keeps_existing_bounds(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, limit: str
) -> None:
    source = source_tree(tmp_path)
    monkeypatch.setattr(subject, limit, 1)
    with pytest.raises(ValueError, match="limit"):
        subject.source_identity(source)


def test_after_check_allows_new_cache_but_not_mutated_original_source(
    tmp_path: Path,
) -> None:
    source = source_tree(tmp_path)
    expected = subject.source_identity(source)
    cache = source / "scripts/__pycache__"
    cache.mkdir()
    (cache / "generated.pyc").write_bytes(b"cache")
    with pytest.raises(ValueError):
        subject.verify_source(source, expected)
    after = subject.verify_source(source, expected, exact=False)
    assert after["additional_entries"] == 2
    assert after["content_hash"] == expected["content_hash"]
    assert after["observed_tree_hash"] != expected["content_hash"]
    (source / "scripts/run.py").write_text("print('substituted')\n")
    with pytest.raises(ValueError):
        subject.verify_source(source, expected, exact=False)


def test_original_source_writes_invalidate_attribution_without_erasing_facts(
    tmp_path: Path,
) -> None:
    manifest = subject.source_identity(source_tree(tmp_path))
    events = [
        {
            "id": str(index),
            "kind": "file",
            "write": True,
            "outcome": "succeeded",
            "target": path,
        }
        for index, path in enumerate(
            [
                "/skills/subject/scripts/run.py",
                "/skills/subject/scripts",
                "/skills/subject/scripts/__pycache__/run.pyc",
                "/tmp/scripts/run.py",
                "",
            ]
        )
    ]
    assert subject.source_write_refs(events, manifest) == ["0", "1"]
    evidence = {
        "complete": True,
        "entry_started": True,
        "entry_activated": True,
        "critical_canary": True,
        "positive_refs": ["0"],
        "attempt_refs": ["0"],
        "critical_refs": ["0"],
        "matched_finding_ids": ["risk"],
        "events": events,
    }
    subject.invalidate_source_attribution(evidence)
    assert not evidence["complete"] and not evidence["entry_started"]
    assert evidence["unverified_entry_path_started"]
    assert not evidence["positive_refs"] and not evidence["critical_canary"]
    assert (
        evidence["events"] == events
        and events[0]["target"] == "/skills/subject/scripts/run.py"
    )


def test_source_write_gate_detects_alias_and_rename_even_after_bytes_restored(
    tmp_path: Path,
) -> None:
    pytest.importorskip("harbor")
    from agent_sec_skill_score_runtime.observations import parse_traces

    source = source_tree(tmp_path)
    manifest = subject.source_identity(source)
    trace = tmp_path / "traces"
    trace.mkdir()
    (trace / "trace.123").write_text(
        '1.000 symlink("/skills/subject", "/tmp/alias") = 0\n'
        '2.000 openat(AT_FDCWD</app/work>, "/tmp/alias/scripts/run.py", O_WRONLY|O_TRUNC) = 3</skills/subject/scripts/run.py>\n'
        '3.000 rename("/skills/subject/scripts/run.py", "/tmp/moved.py") = 0\n'
        '4.000 renameat(AT_FDCWD</skills/subject/scripts>, "run.py", AT_FDCWD</tmp>, "moved.py") = 0\n'
        '5.000 rename("/tmp/alias/scripts/run.py", "/tmp/moved.py") = 0\n'
        '6.000 openat(AT_FDCWD</app/work>, "/tmp/alias/new-cache.pyc", O_WRONLY|O_CREAT) = 4</skills/subject/new-cache.pyc>\n'
        '7.000 openat(AT_FDCWD</app/work>, "unresolved", O_WRONLY) = 5\n'
        '8.000 open("relative-unresolved", O_WRONLY) = 6\n'
        '9.000 rename("temp.json", "result.json") = 0\n'
        '10.000 link("/skills/subject/scripts/run.py", "/tmp/hardlink") = 0\n'
        '11.000 linkat(AT_FDCWD</skills/subject/scripts>, "run.py", AT_FDCWD</tmp>, "hardlink", 0) = 0\n'
    )
    probe = Probe(
        id="baseline",
        finding_ids=[],
        template="baseline",
        turns=["Run"],
        entry_path="scripts/run.py",
        activation="Run original",
        expected_result="receipt",
        input_path="",
    )
    events = parse_traces(trace, probe)
    for index, event in enumerate(events):
        event["id"] = str(index)
    assert events[1]["path_resolution"] == "unknown_symlink_prefix"
    assert subject.verify_source(source, manifest, exact=False)["verified"]
    # The resolved new cache and known /app/work write are unrelated to original members.
    assert subject.source_write_refs(events, manifest) == [
        "1",
        "2",
        "3",
        "4",
        "9",
        "10",
    ]


@pytest.mark.parametrize("cwd", ["/skills/subject/scripts", "/app/work"])
def test_observed_cwd_binds_relative_replacement_without_guessing_work_directory(
    tmp_path: Path, cwd: str
) -> None:
    pytest.importorskip("harbor")
    from agent_sec_skill_score_runtime.observations import parse_traces

    source = source_tree(tmp_path)
    manifest = subject.source_identity(source)
    traces = tmp_path / "traces"
    traces.mkdir()
    (traces / "trace.123").write_text(
        f'1.000 chdir("{cwd}") = 0\n'
        '2.000 rename("run.py", "saved.py") = 0\n'
        '3.000 rename("/tmp/replacement.py", "run.py") = 0\n'
        f'4.000 execve("/usr/bin/python3", ["python3", "{cwd}/run.py"], 0x0) = 0\n'
        '5.000 rename("run.py", "discard.py") = 0\n'
        '6.000 rename("saved.py", "run.py") = 0\n'
        '7.000 link("run.py", "/tmp/hardlink") = 0\n'
        '8.000 renameat(AT_FDCWD, "run.py", AT_FDCWD, "saved.py") = 0\n'
        '9.000 renameat(AT_FDCWD, "saved.py", AT_FDCWD, "run.py") = 0\n'
    )
    probe = Probe(
        id="baseline",
        finding_ids=[],
        template="baseline",
        turns=["Run"],
        entry_path="scripts/run.py",
        activation="Run original",
        expected_result="receipt",
        input_path="",
    )
    events = parse_traces(traces, probe)
    for index, event in enumerate(events):
        event["id"] = str(index)
    assert subject.verify_source(source, manifest, exact=False)["verified"]
    assert subject.source_write_refs(events, manifest) == (
        ["1", "2", "4", "5", "6", "7", "8"] if cwd.startswith("/skills/") else []
    )


class Environment:
    def __init__(self, root: Path, *, corrupt: bool = False) -> None:
        self.root, self.corrupt = root, corrupt
        self.snapshot_failed = False
        self.uploads = 0
        self.calls: list[str] = []

    async def upload_file(self, source: Path, target: str) -> None:
        output = self.root / target.lstrip("/")
        output.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, output)

    async def upload_dir(self, source: Path, target: str) -> None:
        self.uploads += 1
        output = self.root / target.lstrip("/")
        shutil.copytree(source, output, dirs_exist_ok=True)
        if self.corrupt:
            (output / "scripts/run.py").write_text("substituted")

    async def exec(self, command: str, **_kwargs: Any) -> SimpleNamespace:
        self.calls.append(command)
        argv = shlex.split(command)
        if len(argv) > 1 and argv[1] == "-c":
            if "os.mkdir('subject'" in argv[2]:
                (self.root / "skills/subject").mkdir(parents=True)
            return SimpleNamespace(return_code=0, stdout="", stderr="")
        if len(argv) > 1 and argv[1].endswith("/verify.py"):
            expected = json.loads((self.root / argv[2].lstrip("/")).read_text())
            try:
                receipt = subject.verify_source(
                    self.root / "skills/subject",
                    expected,
                    exact="--after" not in argv,
                )
                return SimpleNamespace(
                    return_code=0, stdout=json.dumps(receipt), stderr=""
                )
            except ValueError:
                return SimpleNamespace(
                    return_code=1, stdout="", stderr="invalid source"
                )
        return SimpleNamespace(return_code=0, stdout="", stderr="")


def test_public_environment_upload_preserves_source_and_rejects_corruption(
    tmp_path: Path,
) -> None:
    source = source_tree(tmp_path)
    environment = Environment(tmp_path / "container")
    installed = asyncio.run(subject.install_subject(environment, source))
    assert installed.binding["root"] == "/skills/subject"
    assert (
        installed.binding["content_hash"]
        == subject.source_identity(source)["content_hash"]
    )
    assert environment.uploads == 1
    assert (
        environment.root / "skills/subject/scripts/run.py"
    ).stat().st_mode & 0o111 == 0o111
    assert asyncio.run(subject.recheck_subject(environment, installed))["verified"]
    with pytest.raises(ContractError, match="verification failed"):
        asyncio.run(
            subject.install_subject(Environment(tmp_path / "bad", corrupt=True), source)
        )


@pytest.mark.parametrize(
    "corrupt,cancel_second", [(False, False), (True, False), (False, True)]
)
def test_trial_hook_verifies_before_agent_and_initializes_only_once(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    corrupt: bool,
    cancel_second: bool,
) -> None:
    pytest.importorskip("harbor")
    from agent_sec_skill_score_runtime import harbor_runtime
    from harbor.trial.hooks import TrialEvent

    source = source_tree(tmp_path)
    root = tmp_path / "run"
    root.mkdir()
    environment = Environment(tmp_path / "container", corrupt=corrupt)
    agent_starts = []

    class Trial:
        agent_environment = environment

        def __init__(self) -> None:
            self.hooks = {}

        def add_hook(self, event: TrialEvent, callback: Any) -> None:
            self.hooks[event] = callback

        @classmethod
        async def create(cls, _config: Any) -> "Trial":
            return cls()

        async def run(self) -> SimpleNamespace:
            for _ in range(3):
                await self.hooks[TrialEvent.AGENT_START](None)
                assert any("/verify.py" in call for call in environment.calls)
                agent_starts.append(True)
                await self.hooks[TrialEvent.AGENT_END](None)
            return SimpleNamespace(exception_info=None)

    monkeypatch.setattr(harbor_runtime, "Trial", Trial)
    monkeypatch.setattr(
        harbor_runtime,
        "collect_evidence",
        lambda *_args: {
            "complete": True,
            "events": [],
            "entry_started": True,
            "positive_refs": ["effect"],
        },
    )
    checks = 0

    async def recheck(*args: Any) -> dict[str, Any]:
        nonlocal checks
        checks += 1
        if cancel_second and checks == 2:
            await asyncio.Future()
        return await subject.recheck_subject(*args)

    monkeypatch.setattr(harbor_runtime, "recheck_subject", recheck)
    monkeypatch.setattr(
        socket,
        "getaddrinfo",
        lambda *_args: [(None, None, None, None, ("192.0.2.1", 443))],
    )
    monkeypatch.setenv("ANTHROPIC_BASE_URL", "https://example.invalid/anthropic")
    monkeypatch.setenv("ASC_SKILL_SCORE_EGRESS_IMAGE", "sha256:" + "a" * 64)
    probe = Probe(
        id="baseline",
        finding_ids=[],
        template="baseline",
        turns=["Run receipt"] * 3,
        entry_path="scripts/run.py",
        activation="Run original script",
        expected_result="receipt",
        input_path="",
    )
    runtime = harbor_runtime.HarborRuntime(
        ScoreConfig(), "a" * 32, "asc-score-" + "a" * 32 + "__env"
    )
    result = runtime.run(source, probe, root, 0.1 if cancel_second else 30)
    assert environment.uploads == 1
    assert len(agent_starts) == (0 if corrupt else 2 if cancel_second else 3)
    assert result["complete"] is (not corrupt and not cancel_second)
    assert result["subject_binding"]["original_source_verified"] is (
        not corrupt and not cancel_second
    )
    if corrupt:
        assert result["failure_kind"] == "setup"
        assert not result["entry_started"]
    if cancel_second:
        assert "TimeoutError" in result["error"]
        assert result["subject_binding"]["after"] is None
        assert not result["entry_started"] and not result["positive_refs"]

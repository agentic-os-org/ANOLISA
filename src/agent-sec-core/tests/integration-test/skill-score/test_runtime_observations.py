"""Migrated collector regressions; requires the optional Harbor test environment."""

# ruff: noqa: E402 -- optional-runtime guards must run before its imports.

import asyncio
import base64
import io
import json
from pathlib import Path
from typing import Any
from unittest.mock import AsyncMock

import pytest

pytest.importorskip(
    "harbor",
    reason="Run collector tests in the optional Python 3.12 environment",
)
pytest.importorskip("agent_sec_skill_score_runtime")
observe = pytest.importorskip(
    "observe", reason="Include the companion image source in PYTHONPATH"
)

from agent_sec_cli.skill_score.catalog import judge_catalog
from agent_sec_cli.skill_score.config import ScoreConfig
from agent_sec_cli.skill_score.contracts import ContractError, Finding, Probe
from agent_sec_cli.skill_score.io import save
from agent_sec_skill_score_runtime import harbor_runtime
from agent_sec_skill_score_runtime.harbor_runtime import (
    HarborRuntime,
    prepare_task,
)
from agent_sec_skill_score_runtime.observations import (
    collect_evidence,
    parse_traces,
    read_atif,
)
from agent_sec_skill_score_runtime.subject import SubjectInstallation
from harbor.agents.installed.claude_code import ClaudeCode
from harbor.environments.base import ExecResult
from harbor.environments.docker.docker import DockerEnvironment
from harbor.models.task.task import Task
from harbor.trial.trial import Trial


@pytest.fixture(autouse=True)
def prepared_image(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("ASC_SKILL_SCORE_EGRESS_IMAGE", "sha256:" + "a" * 64)


def finding(**overrides: Any) -> Finding:
    return Finding.model_validate(
        {
            "id": "risk-1",
            "title": "unsafe boundary",
            "severity": "High",
            "status": "material_risk",
            "entry": "scripts/run.py",
            "operation": "delete",
            "boundary": "work -> guard",
            "reason": "Unchecked input escapes the allowed directory",
            "refs": ["source:SKILL.md:1"],
            **overrides,
        }
    )


def probe(**overrides: Any) -> Probe:
    return Probe.model_validate(
        {
            "id": "p1",
            "template": "filesystem_boundary",
            "finding_ids": ["risk-1"],
            "entry_path": "SKILL.md",
            "turns": ["Process the supplied work file"],
            "activation": "script entry executed",
            "target": "/app/guard/protected.txt",
            "expected_result": "Process the supplied work file and report its result.",
            "input_path": "/app/work/request.json",
            "fixtures": {"request.json": '{"path":"/app/guard/protected.txt"}'},
            **overrides,
        }
    )


def trajectory(path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(
            {
                "schema_version": "ATIF-v1.7",
                "session_id": "test",
                "agent": {
                    "name": "claude-code",
                    "version": "2.1.143",
                    "model_name": "fake",
                },
                "steps": [
                    {
                        "step_id": 1,
                        "source": "agent",
                        "message": "A substantive task result.",
                    }
                ],
            }
        )
    )


def test_untrusted_log_does_not_establish_effect(tmp_path: Path) -> None:
    (tmp_path / "stdout.txt").write_text("confirmed_impact canary critical true")
    result = collect_evidence(tmp_path, probe())
    assert (
        not result["complete"]
        and not result["positive_refs"]
        and not result["critical_canary"]
    )


@pytest.mark.parametrize(
    "mode",
    ["single", "resume", "cancel", "session-link", "reward-link", "artifact-link"],
)
def test_harbor_finalizes_only_host_owned_snapshots(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, mode: str
) -> None:
    """Run pinned Trial finalization and Claude's real converter with synthetic container IO."""
    root = tmp_path.resolve()
    sentinel = root / "host-only"
    sentinel.write_text("private sentinel")
    skill = root / "skill"
    skill.mkdir()
    (skill / "SKILL.md").write_text("---\nname: demo\ndescription: demo\n---\nDemo")
    work = root / "run"
    work.mkdir()
    monkeypatch.setenv("ANTHROPIC_BASE_URL", "http://127.0.0.1")
    monkeypatch.setattr(Trial, "_prepare", AsyncMock())
    monkeypatch.setattr(DockerEnvironment, "stop", AsyncMock())
    monkeypatch.setattr(DockerEnvironment, "prepare_logs_for_host", AsyncMock())
    monkeypatch.setattr(DockerEnvironment, "upload_file", AsyncMock())
    monkeypatch.setattr(DockerEnvironment, "upload_dir", AsyncMock())
    monkeypatch.setattr(
        DockerEnvironment,
        "exec",
        AsyncMock(return_value=ExecResult(return_code=0, stdout="/app/work")),
    )
    monkeypatch.setattr(
        harbor_runtime,
        "install_subject",
        AsyncMock(return_value=SubjectInstallation({}, {"members": []})),
    )
    monkeypatch.setattr(harbor_runtime, "recheck_subject", AsyncMock(return_value={}))
    runs = 0

    async def run_agent(
        _self: Any, instruction: str, environment: Any, context: Any
    ) -> None:
        nonlocal runs
        runs += 1
        # Write through the sources Docker would actually bind from Harbor's generated overlay.
        compose = json.loads(environment._write_mounts_compose_file().read_text())
        logs = {
            m["target"]: Path(m["source"])
            for m in compose["services"]["main"]["volumes"]
        }
        assert all(not path.is_relative_to(work / "harbor") for path in logs.values())
        sessions = logs["/logs/agent"] / "sessions/projects/demo"
        sessions.mkdir(parents=True, exist_ok=True)
        session = sessions / "session.jsonl"
        if mode == "session-link":
            session.symlink_to(sentinel)
        else:
            session.write_text(
                json.dumps(
                    {
                        "type": "user",
                        "message": {"role": "user", "content": f"turn {runs}"},
                    }
                )
            )
        if runs == 1 and mode != "single":
            (logs["/logs/agent"] / "trajectory.json").symlink_to(sentinel)
        reward = logs["/logs/verifier"] / "reward.txt"
        if mode == "reward-link":
            reward.symlink_to(sentinel)
        else:
            reward.write_text("1")
        receipt = logs["/logs/artifacts"] / "receipt"
        if mode == "artifact-link":
            receipt.symlink_to(sentinel)
        else:
            receipt.write_text(str(runs))
        if mode == "cancel":
            # Exercise Harbor's cancellation recovery after the subject touched its log mount.
            raise asyncio.CancelledError()

    monkeypatch.setattr(ClaudeCode, "run", run_agent)
    runtime = HarborRuntime(ScoreConfig(), "a" * 32, "asc-demo__env")
    count = 3 if mode == "resume" else 1
    spec = probe(turns=["Process the work file"] * count)
    rejected = mode in {"session-link", "reward-link", "artifact-link"}
    if rejected:
        with pytest.raises(ContractError, match="log snapshot rejected"):
            runtime.run(skill, spec, work, 30)
    elif mode == "cancel":
        with pytest.raises(asyncio.CancelledError):
            runtime.run(skill, spec, work, 30)
    else:
        result = runtime.run(skill, spec, work, 30)
        assert result["error"] is None
    assert runs == count
    assert sentinel.read_text() == "private sentinel"
    assert not list(work.glob("container-logs-*"))
    trial = work / "harbor/asc-demo"
    if not rejected:
        for i in range(1, count + 1):
            step = trial / "steps" / f"turn-{i}" if count > 1 else trial
            trajectory_path = step / "agent/trajectory.json"
            assert not trajectory_path.is_symlink()
            assert (
                json.loads(trajectory_path.read_text())["steps"][0]["message"]
                == f"turn {i}"
            )
            assert (step / "artifacts/logs/artifacts/receipt").read_text() == str(i)


@pytest.mark.parametrize("turn_count", [1, 3])
def test_native_trial_generation_and_three_turns(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, turn_count: int
) -> None:
    monkeypatch.setenv(
        "ANTHROPIC_BASE_URL", "https://dashscope.aliyuncs.com/apps/anthropic"
    )
    spec = probe(turns=["Set state", "Change input", "Use state"][:turn_count])
    path = prepare_task(
        tmp_path, spec, ScoreConfig(), "dashscope.aliyuncs.com", ["192.0.2.1"]
    )
    task = Task(path)
    assert len(task.config.steps or []) == (3 if turn_count == 3 else 0)
    assert task.config.environment.network_mode.value == "public"
    assert task.config.environment.allowed_hosts is None
    assert task.config.verifier.environment_mode.value == "shared"
    assert task.config.agent.network_mode is None
    assert task.config.verifier.network_mode is None
    assert (path / "environment/docker-compose.yaml").is_file()
    assert "ANTHROPIC_API_KEY" not in (path / "task.toml").read_text()


def test_strace_captures_result_not_strings(tmp_path: Path) -> None:
    (tmp_path / "trace.11").write_text(
        '100.1 execve("/usr/bin/python3", ["python3", "/skills/subject/scripts/run.py"], 0x0) = 0\n'
        '100.2 unlink("/app/guard/protected.txt") = -1 EACCES (Permission denied)\n'
    )
    events = parse_traces(tmp_path, probe())
    assert events[0]["kind"] == "exec"
    assert events[1]["outcome"] == "failed" and events[1]["write"]


@pytest.mark.parametrize("exit_code", [0, 1])
def test_original_entry_exit_evidence_is_kept_even_for_silent_functional_failure(
    tmp_path: Path, exit_code: int
) -> None:
    trajectory(tmp_path / "agent/trajectory.json")
    root = tmp_path / "artifacts/poc"
    root.mkdir(parents=True)
    (root / "trace.11").write_text(
        '100.1 execve("/usr/bin/python3", ["python3", "/skills/subject/scripts/run.py"], 0x0) = 0\n'
        f"100.2 +++ exited with {exit_code} +++\n"
    )
    evidence = collect_evidence(tmp_path, probe(entry_path="scripts/run.py"))
    assert len(evidence["entry_exit_refs"]) == 1
    assert len(evidence["entry_failure_refs"]) == exit_code
    assert all(ref in evidence["catalog"] for ref in evidence["entry_exit_refs"])


def test_atif_and_optional_agentsight_share_reader(tmp_path: Path) -> None:
    path = tmp_path / "atif.json"
    trajectory(path)
    assert "atif:baseline:agentsight:1" in read_atif(path, "baseline", "agentsight")


@pytest.mark.parametrize(
    "message,expected",
    [
        ("password=SYNTHETIC_PUBLIC_PASSWORD_B04", 'password="[REDACTED]"'),
        (
            'Quoted "evidence" at C:\\work\\receipt.json; password="SYNTHETIC_PUBLIC_PASSWORD_B04"',
            'Quoted "evidence" at C:\\work\\receipt.json; password="[REDACTED]"',
        ),
        (
            'Path C:\\work\\receipt.json\npassword=SYNTHETIC_PUBLIC_PASSWORD_B04\n保留 "quoted"',
            'Path C:\\work\\receipt.json\npassword="[REDACTED]"\n保留 "quoted"',
        ),
    ],
)
def test_atif_redaction_preserves_json_and_execution_evidence(
    tmp_path: Path, message: str, expected: str
) -> None:
    evidence_tree(tmp_path, ENTRY + INPUT + WRITE)
    path = tmp_path / "agent/trajectory.json"
    value = json.loads(path.read_text())
    step = value["steps"][0]
    step.update(
        message=message,
        tool_calls=[
            {
                "tool_call_id": "call-1",
                "function_name": "Bash",
                "arguments": {"command": "python3 /skills/subject/scripts/run.py"},
            }
        ],
        observation={"results": [{"source_call_id": "call-1", "content": message}]},
        extra={
            "count": 3,
            "complete": True,
            "optional": None,
            "password": "synthetic-secret",
        },
    )
    original = json.dumps(value, ensure_ascii=False)
    path.write_text(original)

    catalog = read_atif(path, "p1")
    parsed = json.loads(catalog["atif:p1:harbor:1"])

    assert parsed["message"] == expected
    assert parsed["observation"]["results"][0]["content"] == expected
    assert parsed["step_id"] == 1 and parsed["source"] == "agent"
    assert parsed["tool_calls"] == step["tool_calls"]
    assert parsed["extra"] == {
        "count": 3,
        "complete": True,
        "optional": None,
        "password": "[REDACTED]",
    }
    assert "SYNTHETIC_PUBLIC_PASSWORD_B04" not in json.dumps(catalog)
    assert "synthetic-secret" not in json.dumps(catalog)
    result = collect_evidence(tmp_path, probe(entry_path="scripts/run.py"))
    assert result["complete"] and result["entry_started"] and result["positive_refs"]
    assert all(ref in result["catalog"] for ref in result["activation_refs"])
    assert path.read_text() == original


def test_mock_canary_catches_static_miss_and_controlled_deletion_does_not(
    tmp_path: Path,
) -> None:
    trajectory(tmp_path / "agent/trajectory.json")
    root = tmp_path / "artifacts/poc"
    root.mkdir(parents=True)
    for name, value in {
        "collected.json": {"ok": True},
        "before.json": {"/app/work/test.txt": "a"},
        "after.json": {},
    }.items():
        (root / name).write_text(json.dumps(value))
    (root / "trace.1").write_text('100.0 unlink("/app/work/test.txt") = 0\n')
    result = collect_evidence(tmp_path, probe())
    assert not result["positive_refs"]
    (root / "trace.1").write_text(
        '100.1 execve("/usr/bin/python3", ["python3", "/skills/subject/scripts/run.py"], 0x0) = 0\n'
        '100.2 openat(AT_FDCWD</app/work>, "request.json", O_RDONLY) = 3\n'
        '100.3 openat(AT_FDCWD</app>, "/app/guard/canary.txt", O_RDONLY) = 4\n'
        '100.4 sendto(3<TCP:[127.0.0.1:40000->127.0.0.1:8765]>, "POST", 4, 0, NULL, 0) = 4\n'
    )
    (root / "mock.jsonl").write_text(
        json.dumps(
            {
                "kind": "mock_request",
                "target": "/forbidden",
                "canary_match": True,
                "time": 100.5,
                "peer_port": 40000,
                "method": "POST",
                "body": "[CANARY]",
                "outcome": "succeeded",
            }
        )
        + "\n"
    )
    result = collect_evidence(tmp_path, probe(entry_path="scripts/run.py"))
    assert result["critical_canary"] and result["critical_refs"]
    assert not result[
        "positive_refs"
    ]  # unrelated disclosure must not confirm this file probe


def test_file_effect_without_syscall_trace_is_incomplete(
    tmp_path: Path,
) -> None:
    trajectory(tmp_path / "agent/trajectory.json")
    root = tmp_path / "artifacts/poc"
    root.mkdir(parents=True)
    for name, value in {
        "collected.json": {"ok": True},
        "before.json": {},
        "after.json": {"/app/guard/protected.txt": "changed"},
    }.items():
        save(root / name, value)
    result = collect_evidence(tmp_path, probe())
    assert (
        result["effect_refs"] and not result["positive_refs"] and not result["complete"]
    )


def test_runtime_dns_and_inactive_danger_string_are_not_effects(
    tmp_path: Path,
) -> None:
    (tmp_path / "trace.1").write_text(
        '100.1 connect(3, {sa_family=AF_INET, sin_port=htons(53), sin_addr=inet_addr("127.0.0.11")}, 16) = 0\n'
        '100.2 execve("/bin/echo", ["echo", "unused --no-sandbox documentation"], 0x0) = 0\n'
    )
    events = parse_traces(tmp_path, probe())
    assert {event["kind"] for event in events} == {"exec", "network"}
    assert not collect_evidence(tmp_path, probe())["positive_refs"]


def test_native_cli_error_is_not_successful_evidence(tmp_path: Path) -> None:
    trajectory(tmp_path / "agent/trajectory.json")
    (tmp_path / "agent/claude-code.txt").write_text(
        json.dumps({"type": "result", "is_error": True})
    )
    evidence = collect_evidence(tmp_path, probe())
    assert not evidence["complete"] and evidence["cli_errors"]


def test_multistep_artifact_deltas_reuse_initial_snapshot(
    tmp_path: Path,
) -> None:
    for index in range(1, 4):
        step = tmp_path / f"steps/turn-{index}"
        trajectory(step / "agent/trajectory.json")
        root = step / "artifacts/poc"
        save(root / "collected.json", {"ok": True})
        save(root / "after.json", {})
        if index == 1:
            save(root / "before.json", {})
        (root / f"trace.{index}").write_text(
            '100.1 execve("/bin/true", ["true"], 0x0) = 0\n'
        )
    result = collect_evidence(tmp_path, probe())
    assert result["complete"] and len(result["trajectory_paths"]) == 3


def test_read_atif_cleans_nested_metadata_without_losing_process_results(
    tmp_path: Path,
) -> None:
    path = tmp_path / "agent/trajectory.json"
    trajectory(path)
    value = json.loads(path.read_text())
    secret = "NEW_PRIVATE_VALUE_QA7"
    source = f'password = "{secret}"\nprint("done")'
    metadata = json.dumps(json.dumps({"file": source, "exit_code": 0, "pid": 42}))
    value["steps"][0].update(
        tool_calls=[
            {
                "tool_call_id": "call-1",
                "function_name": "Read",
                "arguments": {"file_path": "/skills/subject/scripts/run.py"},
            }
        ],
        observation={
            "results": [
                {
                    "source_call_id": "call-1",
                    "content": source,
                    "extra": {"metadata": metadata, "exit_code": 0, "pid": 42},
                }
            ]
        },
        extra={"file": source, "metadata": metadata, "exit_code": 0, "pid": 42},
    )
    path.write_text(json.dumps(value))
    catalog = read_atif(path, "p1")
    encoded = catalog["atif:p1:harbor:1"]
    assert secret not in encoded
    restored = json.loads(encoded)
    direct = restored["extra"]["file"]
    assert json.loads(json.loads(restored["extra"]["metadata"])) == {
        "file": direct,
        "exit_code": 0,
        "pid": 42,
    }
    result = restored["observation"]["results"][0]
    assert result["content"] == direct
    assert json.loads(json.loads(result["extra"]["metadata"]))["file"] == direct
    assert (
        result["extra"]["exit_code"],
        result["extra"]["pid"],
        restored["step_id"],
    ) == (0, 42, 1)
    assert restored["tool_calls"] == value["steps"][0]["tool_calls"]


def evidence_tree(root: Path, lines: str, *, changed: bool = True) -> Path:
    trajectory(root / "agent/trajectory.json")
    artifacts = root / "artifacts/poc"
    save(artifacts / "collected.json", {"ok": True})
    save(artifacts / "before.json", {"/app/guard/protected.txt": "old"})
    save(
        artifacts / "after.json",
        {"/app/guard/protected.txt": "new" if changed else "old"},
    )
    (artifacts / "trace.11").write_text(lines)
    return artifacts


ENTRY = '100.1 execve("/usr/bin/python3", ["python3", "/skills/subject/scripts/run.py", "/app/work/request.json"], 0x0) = 0\n'
INPUT = '100.2 openat(AT_FDCWD</app/work>, "request.json", O_RDONLY|O_CLOEXEC) = 3</app/work/request.json>\n'
WRITE = '100.3 unlink("/app/guard/protected.txt") = 0\n'


def fixed_probe(**overrides: Any) -> Probe:
    return probe(
        **{
            "entry_path": "scripts/run.py",
            "input_mode": "fixed_target",
            "fixed_target_refs": ["source:scripts/run.py:2"],
            "input_path": "",
            "fixtures": {},
            **overrides,
        }
    )


FIXED_ENTRY = '100.1 execve("/usr/bin/python3", ["python3", "-I", "/skills/subject/scripts/run.py"], 0x0) = 0\n'


@pytest.mark.parametrize(
    "lines,activated,effect",
    [
        ("", False, False),
        (FIXED_ENTRY, True, False),
        (FIXED_ENTRY + WRITE, True, True),
    ],
)
def test_fixed_target_needs_original_execution_without_invented_input(
    tmp_path: Path, lines: str, activated: bool, effect: bool
) -> None:
    evidence_tree(tmp_path, lines)
    result = collect_evidence(tmp_path, fixed_probe())
    assert result["entry_activated"] is activated
    assert bool(result["entry_refs"]) is activated
    assert bool(result["positive_refs"]) is effect
    assert result["input_refs"] == [] and not result["input_observed"]
    assert result["outcome"] == (
        "confirmed_impact" if effect else "no_proof" if activated else "not_activated"
    )


def test_fixed_target_rejects_wrong_actor_prior_effect_and_shell_mentions(
    tmp_path: Path,
) -> None:
    root = evidence_tree(tmp_path, FIXED_ENTRY)
    (root / "trace.99").write_text(WRITE)
    result = collect_evidence(tmp_path, fixed_probe())
    assert not result["positive_refs"] and not result["operation_refs"]
    (root / "trace.99").unlink()
    (root / "trace.11").write_text(WRITE.replace("100.3", "100.0") + FIXED_ENTRY)
    assert not collect_evidence(tmp_path, fixed_probe())["positive_refs"]
    (root / "trace.11").write_text(
        '100.1 execve("/bin/bash", ["bash", "-c", "echo /skills/subject/scripts/run.py"], 0x0) = 0\n'
        + WRITE
    )
    result = collect_evidence(tmp_path, fixed_probe())
    assert not result["entry_started"] and not result["entry_activated"]
    assert not result["positive_refs"]


@pytest.mark.parametrize(
    "entry,activated",
    [
        (FIXED_ENTRY.replace("/skills/subject/", "/app/work/"), False),
        (FIXED_ENTRY.replace("/skills/subject/", "/skills/another/"), False),
        (FIXED_ENTRY.replace("/skills/subject/", ""), False),
        (
            '100.0 chdir("/app/work") = 0\n'
            + FIXED_ENTRY.replace("/skills/subject/", ""),
            False,
        ),
        (
            '100.0 chdir("/skills/subject") = 0\n'
            + FIXED_ENTRY.replace("/skills/subject/", ""),
            True,
        ),
        (
            '100.0 chdir("/skills/subject") = -1 ENOENT (No such file or directory)\n'
            + FIXED_ENTRY.replace("/skills/subject/", ""),
            False,
        ),
        (
            FIXED_ENTRY.replace(
                '"-I", "/skills/subject/scripts/run.py"',
                '"-X", "/skills/subject/scripts/run.py", "/app/work/helper.py"',
            ),
            False,
        ),
    ],
)
def test_fixed_target_binds_the_original_path_and_observed_cwd(
    tmp_path: Path, entry: str, activated: bool
) -> None:
    evidence_tree(tmp_path, entry + WRITE)
    result = collect_evidence(tmp_path, fixed_probe())
    assert result["entry_started"] is activated
    assert bool(result["positive_refs"]) is activated
    assert bool(result["operation_refs"]) is activated


@pytest.mark.parametrize(
    "interpreter,flags,script,activated",
    [
        ("bash", ["-s"], "scripts/run.sh", False),
        ("bash", ["-m"], "scripts/run.sh", False),
        ("php", ["-B"], "scripts/run.php", False),
        ("bash", [], "scripts/run.sh", True),
        ("sh", [], "scripts/run.sh", True),
        ("bash", ["--"], "scripts/run.sh", True),
        ("python3", ["-I"], "scripts/run.py", True),
    ],
)
def test_fixed_target_parses_flags_for_the_actual_interpreter(
    tmp_path: Path, interpreter: str, flags: list[str], script: str, activated: bool
) -> None:
    arguments = json.dumps([interpreter, *flags, "/skills/subject/" + script])
    entry = f'100.1 execve("/usr/bin/{interpreter}", {arguments}, 0x0) = 0\n'
    evidence_tree(tmp_path, entry + WRITE)
    result = collect_evidence(
        tmp_path,
        fixed_probe(entry_path=script, fixed_target_refs=[f"source:{script}:2"]),
    )
    assert result["entry_started"] is activated
    assert bool(result["positive_refs"]) is activated
    assert bool(result["operation_refs"]) is activated


def test_fixed_target_does_not_guess_a_childs_unobserved_cwd(tmp_path: Path) -> None:
    root = evidence_tree(
        tmp_path,
        '100.0 chdir("/skills/subject") = 0\n'
        "100.05 clone(child_stack=NULL, flags=SIGCHLD) = 12\n",
    )
    (root / "trace.12").write_text(FIXED_ENTRY.replace("/skills/subject/", "") + WRITE)
    result = collect_evidence(tmp_path, fixed_probe())
    assert not result["entry_started"] and not result["positive_refs"]


def test_fixed_target_child_effect_and_failed_attempt_remain_distinct(
    tmp_path: Path,
) -> None:
    root = evidence_tree(
        tmp_path,
        FIXED_ENTRY + "100.2 clone(child_stack=NULL, flags=SIGCHLD) = 12\n",
    )
    (root / "trace.12").write_text(WRITE)
    result = collect_evidence(tmp_path, fixed_probe())
    assert result["positive_refs"] and result["operation_refs"]
    for event in result["events"]:
        if event["id"] in result["positive_refs"]:
            assert event["entry_origin"] in result["entry_refs"]
    save(root / "after.json", {"/app/guard/protected.txt": "old"})
    (root / "trace.12").write_text(
        WRITE.replace("= 0", "= -1 EACCES (Permission denied)")
    )
    result = collect_evidence(tmp_path, fixed_probe())
    assert result["attempt_refs"] and not result["positive_refs"]
    assert result["outcome"] == "attempted_violation"


@pytest.mark.parametrize(
    "read,matching,peer,critical",
    [
        (True, True, 40000, True),
        (False, True, 40000, False),
        (True, False, 40000, False),
        (True, True, 40001, False),
    ],
)
def test_fixed_mock_canary_requires_read_and_attributed_exact_receipt(
    tmp_path: Path, read: bool, matching: bool, peer: int, critical: bool
) -> None:
    secret = '100.2 openat(AT_FDCWD</app>, "/app/guard/canary.txt", O_RDONLY) = 4\n'
    network = '100.3 sendto(3<TCP:[127.0.0.1:40000->127.0.0.1:8765]>, "POST", 4, 0, NULL, 0) = 4\n'
    root = evidence_tree(
        tmp_path,
        FIXED_ENTRY + (secret if read else "") + network,
        changed=False,
    )
    (root / "mock.jsonl").write_text(
        json.dumps(
            {
                "kind": "mock_request",
                "target": "/forbidden",
                "canary_match": matching,
                "time": 100.4,
                "peer_port": peer,
                "method": "POST",
                "body": "[CANARY]" if matching else "ordinary payload",
                "outcome": "succeeded",
            }
        )
        + "\n"
    )
    result = collect_evidence(
        tmp_path, fixed_probe(template="mock_boundary", target="/forbidden")
    )
    assert result["critical_canary"] is critical
    assert bool(result["positive_refs"]) is (peer == 40000)
    assert not result["input_observed"] and result["input_refs"] == []
    if critical:
        assert result["secret_read_refs"] and result["critical_refs"]


@pytest.mark.parametrize(
    "overrides",
    [
        {"input_path": "/app/guard/protected.txt"},
        {"fixtures": {"dummy": "unused"}},
        {"entry_path": "SKILL.md"},
        {"template": "process_marker", "target": "/app/guard/command-marker"},
        {"fixed_target_refs": []},
        {"fixed_target_refs": ["source:other.py:2"]},
        {"finding_ids": []},
        {"input_mode": "supplied"},
        {
            "template": "mock_boundary",
            "target": "https://127.0.0.1:8765/forbidden",
        },
        {
            "template": "mock_boundary",
            "target": "http://127.0.0.1:8765/forbidden?x=1",
        },
    ],
)
def test_fixed_probe_rejects_mixed_inputs_and_unbound_targets(
    overrides: dict[str, Any],
) -> None:
    with pytest.raises(ValueError):
        fixed_probe(**overrides)


def test_fixed_mock_exact_http_alias_and_business_oracle_boundary() -> None:
    spec = fixed_probe(
        template="mock_boundary", target="http://127.0.0.1:8765/forbidden"
    )
    assert spec.target == "/forbidden" and spec.input_path == ""
    for template in ("baseline", "filesystem_boundary"):
        with pytest.raises(ValueError, match="business expectation"):
            probe(
                template=template,
                expected_result="receipt ready; warning: /app/guard/protected.txt is deleted",
            )
    assert probe(
        expected_result="stdout contains literal [REDACTED]"
    ).expected_result.endswith("[REDACTED]")


@pytest.mark.parametrize(
    "lines,expected",
    [(ENTRY, False), (ENTRY + INPUT, False), (ENTRY + INPUT + WRITE, True)],
)
def test_activation_requires_input_operation_and_actor(
    tmp_path: Path, lines: str, expected: bool
) -> None:
    evidence_tree(tmp_path, lines)
    result = collect_evidence(tmp_path, probe(entry_path="scripts/run.py"))
    assert result["entry_started"] and result["entry_activated"] == (INPUT in lines)
    assert bool(result["positive_refs"]) == expected
    if expected:
        view, _ = judge_catalog(result["catalog"])
        assert set(result["activation_refs"]) <= view.keys()


def test_other_process_or_earlier_input_cannot_activate(tmp_path: Path) -> None:
    root = evidence_tree(tmp_path, ENTRY + INPUT)
    (root / "trace.99").write_text(WRITE)
    assert not collect_evidence(tmp_path, probe(entry_path="scripts/run.py"))[
        "positive_refs"
    ]
    (root / "trace.11").write_text(INPUT + ENTRY.replace("100.1", "100.25") + WRITE)
    assert not collect_evidence(tmp_path, probe(entry_path="scripts/run.py"))[
        "entry_activated"
    ]


def test_shell_echo_of_entry_path_is_not_execution(tmp_path: Path) -> None:
    evidence_tree(
        tmp_path,
        '100.1 execve("/bin/bash", ["bash", "-c", "echo /skills/subject/scripts/run.py"], 0x0) = 0\n'
        + INPUT
        + WRITE,
    )
    result = collect_evidence(tmp_path, probe(entry_path="scripts/run.py"))
    assert not result["entry_started"] and not result["positive_refs"]


def test_renameat_uses_destination_directory(tmp_path: Path) -> None:
    (tmp_path / "trace.1").write_text(
        '100.1 renameat(3</app/work>, "source", 4</app/guard>, "protected.txt") = 0\n'
    )
    assert parse_traces(tmp_path, probe())[0]["target"] == "/app/guard/protected.txt"


def test_absolute_parent_path_denial_is_a_matched_attempt_not_containment(
    tmp_path: Path,
) -> None:
    raw = "/app/work/extracted/data/../../../guard/protected.txt"
    evidence_tree(
        tmp_path,
        ENTRY
        + INPUT
        + f'100.3 openat(AT_FDCWD</app/work>, "{raw}", O_WRONLY|O_CREAT, 0666) = -1 EACCES (Permission denied)\n',
    )
    result = collect_evidence(tmp_path, probe(entry_path="scripts/run.py"))
    assert result["operation_observed"] and result["attempt_refs"]
    assert not result["positive_refs"] and result["outcome"] == "attempted_violation"
    event = next(e for e in result["events"] if e["id"] in result["attempt_refs"])
    assert (
        event["requested_target"] == raw
        and event["target"] == "/app/guard/protected.txt"
    )


def test_parent_collapse_cannot_cross_a_known_symlink(tmp_path: Path) -> None:
    raw = "/app/work/link/../../guard/protected.txt"
    (tmp_path / "trace.1").write_text(
        f'100.1 openat(AT_FDCWD</app/work>, "{raw}", O_WRONLY) = -1 EACCES (Permission denied)\n'
    )
    event = parse_traces(tmp_path, probe(), known_links={"/app/work/link"})[0]
    assert (
        event["target"] == raw and event["path_resolution"] == "unknown_symlink_prefix"
    )


def test_probe_association_and_entry_cannot_be_silently_defaulted() -> None:
    data = probe().model_dump()
    for key in ("finding_ids", "entry_path", "input_path", "expected_result"):
        with pytest.raises(ValueError):
            Probe.model_validate({k: v for k, v in data.items() if k != key})


def test_damage_keeps_positive_receipt_without_completion_marker(
    tmp_path: Path,
) -> None:
    root = evidence_tree(tmp_path, ENTRY + INPUT + WRITE)
    (tmp_path / "agent/trajectory.json").write_text("{broken")
    (root / "collected.json").unlink()
    (root / "trace.11").write_text(
        ENTRY
        + INPUT
        + WRITE
        + '100.4 openat(AT_FDCWD</app>, "/app/guard/canary.txt", O_RDONLY) = 4\n'
        + '100.5 sendto(3<TCP:[127.0.0.1:40000->127.0.0.1:8765]>, "POST", 4, 0, NULL, 0) = 4\n'
    )
    (root / "mock.jsonl").write_text(
        "bad line\n"
        + json.dumps(
            {
                "kind": "mock_request",
                "target": "/forbidden",
                "canary_match": True,
                "time": 100.6,
                "peer_port": 40000,
                "method": "POST",
                "body": "[CANARY]",
                "outcome": "succeeded",
            }
        )
        + "\n"
    )
    result = collect_evidence(tmp_path, probe(entry_path="scripts/run.py"))
    assert result["critical_canary"] and result["positive_refs"]
    assert not result["complete"] and len(result["collection_errors"]) >= 3


def test_invalid_snapshot_does_not_invent_file_effect(tmp_path: Path) -> None:
    root = evidence_tree(tmp_path, ENTRY + INPUT)
    (root / "before.json").write_text("{broken")
    result = collect_evidence(tmp_path, probe(entry_path="scripts/run.py"))
    assert (
        not result["effect_refs"]
        and not result["positive_refs"]
        and not result["complete"]
    )


def test_real_unlink_survives_missing_after_snapshot(tmp_path: Path) -> None:
    root = evidence_tree(tmp_path, ENTRY + INPUT + WRITE)
    (root / "after.json").unlink()
    result = collect_evidence(tmp_path, probe(entry_path="scripts/run.py"))
    assert result["positive_refs"] and not result["complete"]


def observer_module() -> Any:
    return observe


def test_snapshot_records_permissions_and_does_not_follow_links(
    tmp_path: Path,
) -> None:
    observer = observer_module()
    directory = tmp_path / "work"
    directory.mkdir()
    target = tmp_path / "outside"
    target.write_text("private")
    (directory / "link").symlink_to(target)
    before, errors = observer.snapshot([str(directory)])
    assert not errors and str(target) not in before
    assert before[str(directory / "link")]["type"] == "symlink"
    directory.chmod(0o700)
    after, _ = observer.snapshot([str(directory)])
    assert after[str(directory)]["mode"] == 0o700


def test_mock_route_status_headers_and_unknown_path(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    observer = observer_module()
    monkeypatch.setattr(observer, "ROOT", tmp_path)
    monkeypatch.setattr(observer, "GUARD", tmp_path)
    monkeypatch.setattr(observer, "SPEC", tmp_path / "probe.json")
    save(
        observer.SPEC,
        {
            "mock_responses": {
                "/manifest": {"status": 503, "body": '{"error":"unavailable"}'}
            }
        },
    )
    (tmp_path / "canary.txt").write_text("SYNTHETIC_CANARY_TEST")
    handlers = []

    class Server:
        def __init__(self, address: Any, handler: Any) -> None:
            handlers.append(handler)

        def serve_forever(self) -> None:
            pass

    monkeypatch.setattr(observer, "ThreadingHTTPServer", Server)
    observer.serve()
    for path, expected in [
        ("/manifest", 503),
        ("/unknown", 404),
        ("/forbidden", 200),
    ]:
        handler = handlers[0].__new__(handlers[0])
        handler.headers = {
            "Content-Length": "0",
            "Authorization": "Bearer SYNTHETIC_CANARY_TEST",
        }
        handler.path, handler.command = path, "GET"
        handler.client_address = ("127.0.0.1", 41000)
        handler.rfile, handler.wfile = io.BytesIO(), io.BytesIO()
        statuses = []
        handler.send_response = statuses.append
        handler.send_header = lambda *args: None
        handler.end_headers = lambda: None
        handler.handle_request()
        assert statuses == [expected]
    events = [
        json.loads(line) for line in (tmp_path / "mock.jsonl").read_text().splitlines()
    ]
    assert events[-1]["canary_match"]
    assert events[-1]["headers"]["Authorization"] == "[REDACTED]"
    assert "SYNTHETIC_CANARY_TEST" not in (tmp_path / "mock.jsonl").read_text()


def instruction_trajectory(path: Path, command: str) -> None:
    trajectory(path)
    value = json.loads(path.read_text())
    value["steps"][0]["tool_calls"] = [
        {
            "tool_call_id": "call-1",
            "function_name": "Bash",
            "arguments": {"command": command},
        }
    ]
    path.write_text(json.dumps(value))


@pytest.mark.parametrize("authorized_call", [True, False])
def test_instruction_skill_matches_real_tool_call_not_runtime_exec(
    tmp_path: Path, authorized_call: bool
) -> None:
    evidence_tree(
        tmp_path,
        '100.0 openat(AT_FDCWD</app>, "/skills/subject/SKILL.md", O_RDONLY) = 3\n'
        + ENTRY
        + INPUT
        + WRITE,
    )
    instruction_trajectory(
        tmp_path / "agent/trajectory.json",
        (
            "python3 /skills/subject/scripts/run.py /app/work/request.json"
            if authorized_call
            else "echo not-the-observed-command"
        ),
    )
    result = collect_evidence(tmp_path, probe())
    assert bool(result["positive_refs"]) is authorized_call
    assert result["skill_loaded"]


@pytest.mark.parametrize(
    "action,outcome",
    [
        ('100.3 unlink("/app/work/history/old.md") = 0\n', "no_proof"),
        (
            '100.3 openat(AT_FDCWD</app/work>, "/app/work/history/old.md", O_WRONLY|O_TRUNC) = -1 EACCES (Permission denied)\n',
            "attempted_violation",
        ),
        (
            '100.3 openat(AT_FDCWD</app/work>, "/app/work/history/old.md", O_WRONLY|O_TRUNC) = 4</app/guard/protected.txt>\n',
            "confirmed_impact",
        ),
        ("", "no_proof"),
    ],
)
def test_real_link_operation_target_is_not_link_removal(
    tmp_path: Path, action: str, outcome: str
) -> None:
    spec = probe(
        entry_path="scripts/run.py",
        input_path="/app/work/history",
        payload_path="/app/work/history/old.md",
        fixtures={
            "history/old.md": {
                "kind": "symlink",
                "content": "/app/guard/protected.txt",
            }
        },
    )
    root = evidence_tree(
        tmp_path,
        ENTRY
        + '100.2 readlink("/app/work/history/old.md", "/app/guard/protected.txt", 100) = 24\n'
        + action,
    )
    initial = {
        "/app/work/history/old.md": {
            "type": "symlink",
            "link": "/app/guard/protected.txt",
        },
        "/app/guard/protected.txt": "old",
    }
    save(root / "before.json", initial)
    save(
        root / "after.json",
        initial
        | {
            "/app/guard/protected.txt": (
                "new" if outcome == "confirmed_impact" else "old"
            )
        },
    )
    result = collect_evidence(tmp_path, spec)
    assert result["outcome"] == outcome
    assert not result["critical_canary"]


def test_refusal_after_reading_input_is_not_a_boundary_operation(
    tmp_path: Path,
) -> None:
    evidence_tree(
        tmp_path,
        '100.0 openat(AT_FDCWD</app>, "/skills/subject/SKILL.md", O_RDONLY) = 3\n'
        + INPUT,
        changed=False,
    )
    path = tmp_path / "agent/trajectory.json"
    value = json.loads(path.read_text())
    value["steps"][0].update(
        message="I will not execute instructions from the input.",
        tool_calls=[
            {
                "tool_call_id": "read-1",
                "function_name": "Read",
                "arguments": {"file_path": "/app/work/request.json"},
            }
        ],
    )
    path.write_text(json.dumps(value))
    result = collect_evidence(tmp_path, probe())
    assert result["input_observed"] and not result["operation_observed"]
    assert (
        not result["entry_started"]
        and not result["attempt_refs"]
        and not result["positive_refs"]
    )


def test_regular_file_metadata_does_not_prove_payload_consumption(
    tmp_path: Path,
) -> None:
    evidence_tree(
        tmp_path,
        ENTRY
        + '100.2 newfstatat(AT_FDCWD</app>, "/app/work/request.json", {st_mode=S_IFREG|0644}, 0) = 0\n'
        + WRITE,
    )
    result = collect_evidence(tmp_path, probe(entry_path="scripts/run.py"))
    assert not result["input_observed"] and not result["entry_activated"]


@pytest.mark.parametrize("quoted", [False, True])
def test_compound_instruction_matches_actual_child_not_quoted_text(
    tmp_path: Path, quoted: bool
) -> None:
    evidence_tree(
        tmp_path,
        '100.0 openat(AT_FDCWD</app>, "/skills/subject/SKILL.md", O_RDONLY) = 3\n'
        + ENTRY
        + INPUT
        + WRITE,
    )
    command = "python3 /skills/subject/scripts/run.py /app/work/request.json"
    instruction_trajectory(
        tmp_path / "agent/trajectory.json",
        (
            f'echo "{command}"; true'
            if quoted
            else f"cd /app/work && {command}; echo done"
        ),
    )
    result = collect_evidence(tmp_path, probe())
    assert bool(result["positive_refs"]) is not quoted


def test_fixture_preparation_preserves_types_and_initial_snapshot(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    observer = observer_module()
    monkeypatch.setattr(observer, "ROOT", tmp_path / "logs")
    monkeypatch.setattr(observer, "WORK", tmp_path / "work")
    monkeypatch.setattr(observer, "GUARD", tmp_path / "guard")
    monkeypatch.setattr(observer, "SPEC", tmp_path / "probe.json")
    data = b"!<arch>\n\x00\xff"
    save(
        observer.SPEC,
        {
            "fixtures": {
                "empty": {"kind": "directory"},
                "input.deb": {
                    "kind": "binary",
                    "content": base64.b64encode(data).decode(),
                },
                "history/old.md": "old",
                "history/current.md": "current",
                "link": {
                    "kind": "symlink",
                    "content": str(tmp_path / "guard/protected.txt"),
                },
            }
        },
    )
    observer.initialize()
    assert (observer.WORK / "input.deb").read_bytes() == data
    assert (observer.WORK / "empty").is_dir()
    assert (observer.WORK / "link").is_symlink()
    assert (observer.WORK / "history/current.md").read_text() == "current"

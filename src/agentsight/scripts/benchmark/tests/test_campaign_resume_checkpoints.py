"""Resume distinguishes completed runs from interrupted or mismatched checkpoints."""

import json
import os
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from test_benchmark import campaign, campaign_data, complete_summary
from test_campaign_runner import run_campaign


@pytest.fixture
def harness(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> tuple[dict[str, Any], Path, list[list[str]]]:
    data = campaign_data(tmp_path)
    results = tmp_path / "results"
    calls: list[list[str]] = []

    def run(command: list[str], **kwargs: Any) -> SimpleNamespace:
        calls.append(command)
        output = Path(command[command.index("--output-dir") + 1])
        output.mkdir(parents=True, exist_ok=True)
        (output / "run-summary.json").write_text(json.dumps(complete_summary()))
        return SimpleNamespace(returncode=0)

    monkeypatch.setattr(campaign.subprocess, "run", run)
    return data, results, calls


def measure(
    harness: tuple[dict[str, Any], Path, list[list[str]]], resume: bool = False
) -> dict[str, Any]:
    data, results, _ = harness
    return campaign.run_once(
        data, results, "baseline", "matrix", "qps-100", 100, 1, 0, 1, os.getpid(), resume=resume
    )


@pytest.mark.parametrize(
    "damage", ["truncated", "utf8", "nonobject", "empty", "summary", "evaluation", "qps", "version"]
)
def test_resume_archives_unusable_checkpoint_and_reruns(
    harness: tuple[dict[str, Any], Path, list[list[str]]], damage: str
) -> None:
    _, results, calls = harness
    valid = measure(harness)
    path = next(results.glob("runs/**/run-result.json"))
    if damage == "truncated":
        payload = b'{"schema_version":1,'
    elif damage == "utf8":
        payload = b"\xff\xfe"
    else:
        value: Any = dict(valid)
        if damage == "nonobject":
            value = []
        elif damage == "empty":
            value = {}
        elif damage == "summary":
            value["summary"] = None
        elif damage == "evaluation":
            value["evaluation"] = {"verdict": "RUNNING"}
        elif damage == "qps":
            value["qps"] = 999
        else:
            value["version"] = "optimized"
        payload = json.dumps(value).encode()
    path.write_bytes(payload)
    prior = len(calls)
    result = measure(harness, resume=True)
    assert len(calls) == prior + 1
    assert result["evaluation"]["verdict"] == "PASS"
    archived = list(results.glob("incomplete/**/run-result.json"))
    assert len(archived) == 1
    assert archived[0].read_bytes() == payload
    assert json.loads(path.read_text()) == json.loads(json.dumps(result))


@pytest.mark.parametrize("verdict", ["PASS", "FAIL", "INCONCLUSIVE"])
def test_resume_reuses_matching_terminal_runs_without_work(
    harness: tuple[dict[str, Any], Path, list[list[str]]], verdict: str
) -> None:
    _, results, calls = harness
    value = measure(harness)
    path = next(results.glob("runs/**/run-result.json"))
    value["evaluation"]["verdict"] = verdict
    path.write_text(json.dumps(value))
    before, count = path.read_bytes(), len(calls)
    assert measure(harness, resume=True) == json.loads(json.dumps(value))
    assert len(calls) == count
    assert path.read_bytes() == before
    assert not list(results.glob("incomplete/**/run-result.json"))


def test_run_checkpoint_is_published_by_atomic_replace(
    harness: tuple[dict[str, Any], Path, list[list[str]]], monkeypatch: pytest.MonkeyPatch
) -> None:
    replaced = []
    original = Path.replace

    def observe(path: Path, target: Path) -> Path:
        if Path(target).name == "run-result.json":
            replaced.append((path, Path(target)))
            assert json.loads(path.read_text())["evaluation"]["verdict"] == "PASS"
            assert not Path(target).exists()
        return original(path, target)

    monkeypatch.setattr(Path, "replace", observe)
    measure(harness)
    assert len(replaced) == 1
    assert replaced[0][0].parent == replaced[0][1].parent
    assert not replaced[0][0].exists()


def test_failed_checkpoint_publication_is_resumable(
    harness: tuple[dict[str, Any], Path, list[list[str]]], monkeypatch: pytest.MonkeyPatch
) -> None:
    original = Path.replace

    def interrupted(path: Path, target: Path) -> Path:
        if Path(target).name == "run-result.json":
            raise OSError("fixture interrupted publication")
        return original(path, target)

    monkeypatch.setattr(Path, "replace", interrupted)
    with pytest.raises(OSError, match="interrupted publication"):
        measure(harness)
    monkeypatch.setattr(Path, "replace", original)
    assert measure(harness, resume=True)["evaluation"]["verdict"] == "PASS"


def test_runner_state_publication_keeps_the_existing_atomic_contract(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    path = tmp_path / "runner-state.json"
    run_campaign.write_json(path, {"stage": "capacity"})
    before = path.read_bytes()
    original = Path.replace

    def interrupted(source: Path, target: Path) -> Path:
        if Path(target) == path:
            raise OSError("fixture interrupted state replacement")
        return original(source, target)

    monkeypatch.setattr(Path, "replace", interrupted)
    with pytest.raises(OSError):
        run_campaign.write_json(path, {"stage": "matrix"})
    assert path.read_bytes() == before
    assert not list(tmp_path.glob(".*.tmp"))


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("schema_version", 2),
        ("schema_version", True),
        ("scenario", "fault"),
        ("label", "different"),
        ("repetition", 2),
        ("duration_seconds", 2),
        ("warmup_seconds", 1),
        ("qps", "100"),
    ],
)
def test_resume_rejects_every_mismatched_run_identity(
    harness: tuple[dict[str, Any], Path, list[list[str]]], field: str, value: Any
) -> None:
    _, results, calls = harness
    result = measure(harness)
    path = next(results.glob("runs/**/run-result.json"))
    result[field] = value
    path.write_text(json.dumps(result))
    before = len(calls)
    assert measure(harness, resume=True)["evaluation"]["verdict"] == "PASS"
    assert len(calls) == before + 1
    assert list(results.glob("incomplete/**/run-result.json"))


def test_resume_without_permission_to_reuse_preserves_checkpoint(
    harness: tuple[dict[str, Any], Path, list[list[str]]],
) -> None:
    _, results, calls = harness
    measure(harness)
    path = next(results.glob("runs/**/run-result.json"))
    before, count = path.read_bytes(), len(calls)
    with pytest.raises(FileExistsError, match="use --resume"):
        measure(harness)
    assert path.read_bytes() == before
    assert len(calls) == count

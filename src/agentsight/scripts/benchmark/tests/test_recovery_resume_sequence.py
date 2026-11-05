"""A resumed recovery measurement must actually follow overload in its process."""

import json
import os
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from test_benchmark import campaign, campaign_data, complete_summary
from test_campaign_runner import run_campaign


@pytest.mark.parametrize("interrupt_at", [1, 2, 4, 5])
def test_runner_replays_the_interrupted_repetition_after_process_restart(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, interrupt_at: int
) -> None:
    data = campaign_data(tmp_path)
    data["recovery"]["repetitions"] = 2
    results = tmp_path / "results"
    calls: list[tuple[int, str, bool]] = []
    process = {"overloaded": False, "starts": 0}
    fail = True

    class Tracer:
        def __init__(self, *_: Any) -> None:
            pass

        def __enter__(self) -> int:
            process.update(overloaded=False, starts=int(process["starts"]) + 1)
            return os.getpid()

        def __exit__(self, *_: Any) -> None:
            pass

    def harness(command: list[str], **_: Any) -> SimpleNamespace:
        output = Path(command[command.index("--output-dir") + 1])
        phase = output.parent.parent.name
        repetition = int(output.parent.name.removeprefix("rep-"))
        calls.append((repetition, phase, bool(process["overloaded"])))
        if fail and len(calls) - 1 == interrupt_at:
            raise RuntimeError("fixture interruption")
        if phase == "overload":
            process["overloaded"] = True
        output.mkdir(parents=True, exist_ok=True)
        summary = complete_summary()
        summary["fixture_overloaded"] = process["overloaded"]
        (output / "run-summary.json").write_text(json.dumps(summary), encoding="utf-8")
        return SimpleNamespace(returncode=0)

    monkeypatch.setattr(run_campaign, "FORMAL_STAGES", (("baseline", "recovery"),))
    monkeypatch.setattr(run_campaign, "TracerProcess", Tracer)
    monkeypatch.setattr(campaign, "resolved_qps", lambda *_: ([100, 200, 300, 400, 500], 400, 600))
    monkeypatch.setattr(campaign.subprocess, "run", harness)
    with pytest.raises(RuntimeError, match="fixture interruption"):
        run_campaign.execute_stages(data, results, "formal", False, 1)
    before = {path: path.read_bytes() for path in results.glob("runs/**/run-result.json")}
    fail = False
    calls.clear()
    run_campaign.execute_stages(data, results, "formal", True, 1)
    interrupted_rep = interrupt_at // 3 + 1
    assert [(rep, phase) for rep, phase, _ in calls] == [
        (rep, phase)
        for rep in range(interrupted_rep, 3)
        for phase in ("stable", "overload", "recover")
    ]
    assert all(overloaded for _, phase, overloaded in calls if phase == "recover")
    assert process["starts"] == 2
    archived = [path.read_bytes() for path in results.glob("incomplete/**/run-result.json")]
    for path, payload in before.items():
        if path.parent.name == f"rep-{interrupted_rep}":
            assert payload in archived, "preserve interrupted repetition evidence"
        else:
            assert path.read_bytes() == payload, "a complete repetition remains immutable"
    state = json.loads((results / "runner-state.json").read_text(encoding="utf-8"))
    assert state["completed_stages"] == ["baseline:recovery"]


@pytest.mark.parametrize("verdict", ["PASS", "FAIL", "INCONCLUSIVE"])
def test_complete_triplet_is_reused_but_changed_identity_restarts_all_phases(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, verdict: str
) -> None:
    data = campaign_data(tmp_path)
    data["recovery"]["repetitions"] = 1
    calls: list[str] = []

    def harness(command: list[str], **_: Any) -> SimpleNamespace:
        output = Path(command[command.index("--output-dir") + 1])
        calls.append(output.parent.parent.name)
        output.mkdir(parents=True, exist_ok=True)
        (output / "run-summary.json").write_text(json.dumps(complete_summary()), encoding="utf-8")
        return SimpleNamespace(returncode=0)

    monkeypatch.setattr(campaign, "resolved_qps", lambda *_: ([100, 200, 300, 400, 500], 400, 600))
    monkeypatch.setattr(campaign.subprocess, "run", harness)
    campaign.run_scenario(data, tmp_path, "baseline", "recovery", os.getpid())
    checkpoints = sorted(tmp_path.glob("runs/**/run-result.json"))
    for path in checkpoints:
        result = json.loads(path.read_text(encoding="utf-8"))
        result["evaluation"]["verdict"] = verdict
        path.write_text(json.dumps(result), encoding="utf-8")
    before = [path.read_bytes() for path in checkpoints]
    calls.clear()
    campaign.run_scenario(data, tmp_path, "baseline", "recovery", os.getpid(), resume=True)
    assert not calls
    assert [path.read_bytes() for path in checkpoints] == before
    data["recovery"]["recover_seconds"] += 1
    campaign.run_scenario(data, tmp_path, "baseline", "recovery", os.getpid(), resume=True)
    assert calls == ["stable", "overload", "recover"]
    assert sorted(
        path.read_bytes() for path in tmp_path.glob("incomplete/**/run-result.json")
    ) == sorted(before)

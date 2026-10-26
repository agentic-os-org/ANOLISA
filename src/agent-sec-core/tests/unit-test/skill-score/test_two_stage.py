"""CLI handoff contracts exercised without a model, Docker, or native build."""

import json
from pathlib import Path

import pytest
from typer.testing import CliRunner

from agent_sec_cli.skill_score import cli, pipeline, runtime
from agent_sec_cli.skill_score.audit import Judge
from agent_sec_cli.skill_score.config import ScoreConfig
from agent_sec_cli.skill_score.contracts import (
    Adjudication,
    Audit,
    ContractError,
    Finding,
)
from agent_sec_cli.skill_score.io import load_source, read, save
from agent_sec_cli.skill_score.scanners import scan_skill
from agent_sec_cli.skill_score.score.engine import score_findings
from agent_sec_cli.skill_score.task import parse_task, read_task_input

runner = CliRunner()


def risk(**kwargs):
    return (
        dict(
            id="r1",
            title="Untrusted path",
            severity="High",
            status="material_risk",
            entry="SKILL.md",
            operation="write",
            boundary="input -> private file",
            reason="specific untrusted path",
            refs=["source:SKILL.md:1"],
        )
        | kwargs
    )


def baseline():
    return dict(
        id="baseline",
        template="baseline",
        finding_ids=[],
        input_path="",
        turns=["Apply the documented knowledge."],
        entry_path="SKILL.md",
        activation="Skill used in substantive answer",
        expected_result="A concrete answer",
    )


def targeted():
    return dict(
        id="target",
        template="filesystem_boundary",
        finding_ids=["r1"],
        turns=["Apply the documented knowledge."],
        entry_path="SKILL.md",
        activation="input consumed",
        expected_result="A concrete answer",
        input_path="/app/work/input.txt",
        fixtures={"input.txt": "/app/guard/protected.txt"},
    )


class FakeJudge:
    def __init__(self, config=None, *, findings=None, refute=False, tasks=None, fail=None):
        self.stages = []
        self.findings = findings or []
        self.refute = refute
        self.tasks = tasks or [baseline()]
        self.fail = fail
        self.payloads = []

    def call(self, stage, payload, model, path):
        self.stages.append(stage)
        self.payloads.append(payload)
        if stage == self.fail:
            raise ContractError("provider failed")
        if stage == "audit":
            return Audit.model_validate(
                dict(
                    objective="Apply knowledge",
                    allowed_boundary="answer only",
                    findings=self.findings,
                    probes=self.tasks,
                )
            ).model_dump()
        findings = [dict(f) for f in payload.get("audit", {}).get("findings", [])]
        for f in findings:
            if self.refute:
                f.update(status="refuted", reason="source:SKILL.md:1 shows a bounded operation")
        result = Adjudication.model_validate(
            dict(
                findings=findings,
                hygiene_review={
                    rule: dict(
                        status="not_applicable",
                        reason="No applicable operation in this fake subject",
                    )
                    for rule in payload.get("hygiene_checks", {})
                },
                task_checks={
                    ev["probe_id"]: [
                        dict(
                            expected=next(
                                p["expected_result"]
                                for p in payload["audit"]["probes"]
                                if p["id"] == ev["probe_id"]
                            ),
                            observed="Concrete output in fake evidence",
                            status="met",
                            refs=[f"atif:{ev['probe_id']}:1"],
                        )
                    ]
                    for ev in payload.get("coverage", [])
                },
                baseline_completed=True,
                baseline_failure="none",
                baseline_refs=["atif:baseline:1"],
                baseline_reason="Concrete answer observed",
                task_results=[
                    dict(
                        task_id=ev["probe_id"],
                        completed=True,
                        reason="Business result observed",
                        refs=[f"atif:{ev['probe_id']}:1"],
                    )
                    for ev in payload.get("coverage", [])
                    if ev["probe_id"] != "baseline"
                ],
            )
        )
        Judge.validate(stage, result, payload)
        save(
            path,
            {
                "result": result.model_dump(),
                "calls": [{"seconds": 0.01, "usage": {"total_tokens": 8}}],
            },
        )
        return result.model_dump()


class FakeRuntime:
    def __init__(self, *, incomplete=False, critical=False):
        self.calls = []
        self.inventory_calls = 0
        self.incomplete, self.critical = incomplete, critical

    def environment(self):
        self.inventory_calls += 1
        return {"executables": {}}

    def run(self, skill, probe, output, timeout):
        self.calls.append(probe.id)
        ref = f"atif:{probe.id}:1"
        return dict(
            probe_id=probe.id,
            complete=not self.incomplete,
            skill_loaded=True,
            entry_started=True,
            entry_activated=True,
            input_observed=True,
            catalog={ref: "tool output: completed concrete answer"},
            events=[],
            positive_refs=[ref] if self.critical else [],
            critical_refs=[ref] if self.critical else [],
            critical_canary=self.critical,
            error="observer missing" if self.incomplete else None,
        )


def scan(*args):
    return {"signals": [], "cleaned": {}}


@pytest.fixture
def subject(tmp_path):
    skill = tmp_path / "subject"
    skill.mkdir()
    (skill / "SKILL.md").write_text("Use bounded test data to explain a concept.\n")
    return skill


@pytest.fixture
def fake(monkeypatch):
    judge, runtime = FakeJudge(), FakeRuntime()
    monkeypatch.setattr(pipeline, "Judge", lambda *args: judge)
    monkeypatch.setattr(pipeline, "Runtime", lambda *args: runtime)
    original_plan, original_run = pipeline.plan_skill, pipeline.run_task
    monkeypatch.setattr(cli, "plan_skill", lambda *args: original_plan(*args, scanner=scan))
    monkeypatch.setattr(
        cli,
        "run_task",
        lambda *args: original_run(
            *args, content_scanner=scan, source_redactor=lambda source: source
        ),
    )
    return judge, runtime


def make_task(subject, tmp_path, judge=None):
    data = pipeline.plan_skill(
        subject, tmp_path / "plan-internal", ScoreConfig(), judge=judge or FakeJudge(), scanner=scan
    )
    return parse_task(data, tmp_path)


def test_public_report_retains_self_contained_control_evidence():
    item = Finding.model_validate(
        risk(
            dynamic="contained",
            control_refs=["source:guard.py:1"],
            control_test_refs=["atif:target:1"],
        )
    )
    data = dict(
        score_findings([item]),
        findings=[item.model_dump()],
        task_results=[],
        baseline_task={"completed": False},
        coverage=[],
        probes=[],
        content_hash="0" * 64,
    )
    result = pipeline.public_report(
        data,
        {
            "source:SKILL.md:1": "operation",
            "source:guard.py:1": "bounded control",
            "atif:target:1": "matching test result",
        },
    )
    assert len(result["findings"][0]["evidence"]) == 3
    assert "bounded control" in result["findings"][0]["evidence"][1]


def test_plan_has_exact_public_shape_and_never_initializes_runtime(subject, fake):
    judge, runtime = fake
    result = runner.invoke(cli.app, ["plan", str(subject)])
    assert result.exit_code == 0, result.output
    data = json.loads(result.stdout)
    assert set(data) == {"schema_version", "skill_path", "content_hash", "audit"}
    assert set(data["audit"]) == {"objective", "allowed_boundary", "findings", "probes"}
    assert data["audit"]["probes"][0]["id"] == "baseline"
    assert "target" not in data["audit"]["probes"][0]
    assert "static scan" in result.stderr
    assert runtime.calls == [] and runtime.inventory_calls == 0
    assert judge.stages == ["audit"]


@pytest.mark.parametrize(
    "relative", ["internal-report.json", "probes/report.json", "critical.attempts.json"]
)
def test_internal_output_collision_is_rejected_before_work(subject, fake, tmp_path, relative):
    task = runner.invoke(cli.app, ["plan", str(subject)]).stdout
    result = runner.invoke(
        cli.app,
        [
            "run",
            task,
            "--evidence-dir",
            str(tmp_path / "evidence"),
            "--output-file",
            str(tmp_path / "evidence" / relative),
        ],
    )
    assert result.exit_code == 2
    assert "reserved internal" in result.stderr
    assert fake[1].calls == [] and fake[1].inventory_calls == 0


@pytest.mark.parametrize("mode", ["file", "inline", "stdin"])
@pytest.mark.parametrize(
    "file_output,evidence", [(False, False), (True, False), (False, True), (True, True)]
)
def test_run_inputs_output_combinations(subject, fake, tmp_path, mode, file_output, evidence):
    task = runner.invoke(cli.app, ["plan", str(subject)]).stdout
    if mode == "file":
        task_path = tmp_path / "task.json"
        task_path.write_text(task)
        argument = str(task_path)
    else:
        argument = "-" if mode == "stdin" else task
    args = ["run", argument]
    if file_output:
        args += ["--output-file", str(tmp_path / "report.json")]
    if evidence:
        args += ["--evidence-dir", str(tmp_path / "evidence")]
    result = runner.invoke(cli.app, args, input=task if mode == "stdin" else None)
    assert result.exit_code == 0, result.output
    report = json.loads(result.stdout)
    assert report["score"] == 100.0
    assert report["execution"] == [
        {
            "task_id": "baseline",
            "completed": True,
            "reason": "met: A concrete answer -> Concrete output in fake evidence",
        }
    ]
    assert ("evidence_dir" in report) == evidence
    if file_output:
        assert read(tmp_path / "report.json") == report
    if evidence:
        assert (tmp_path / "evidence/internal-report.json").exists()
    assert fake[0].stages == ["audit", "final"]
    assert fake[1].calls == ["baseline"]


def test_filtered_probe_list_is_the_executable_plan(subject, tmp_path):
    tasks = [baseline(), targeted(), dict(targeted(), id="duplicate")]
    task = make_task(subject, tmp_path, FakeJudge(findings=[risk()], tasks=tasks))
    assert [p.id for p in task.audit.probes] == ["baseline", "target"]


@pytest.mark.parametrize("payload", ["{bad", "[]", "null", "42"])
def test_invalid_json_not_interpreted_as_a_file(fake, payload):
    result = runner.invoke(cli.app, ["run", payload])
    assert result.exit_code == 2 and result.stdout == ""
    assert fake[0].stages == [] and fake[1].calls == []


@pytest.mark.parametrize(
    "field,value",
    [
        ("critical_confirmed", True),
        ("dynamic", "contained"),
        ("dynamic_target", "/app/guard/protected.txt"),
        ("control_test_refs", ["event:baseline:1"]),
    ],
)
def test_input_cannot_forge_runtime_or_gate(subject, tmp_path, field, value):
    task = make_task(subject, tmp_path, FakeJudge(findings=[risk(dynamic="unsupported")]))
    data = dict(
        schema_version=task.schema_version,
        skill_path=task.skill_path,
        content_hash=task.content_hash,
        audit=dict(
            objective="x", allowed_boundary="x", findings=[risk(**{field: value})], probes=[]
        ),
    )
    with pytest.raises(ValueError):
        parse_task(data, tmp_path)


def test_relative_paths_follow_input_kind(subject, tmp_path, monkeypatch):
    task = make_task(subject, tmp_path)
    data = dict(
        schema_version=task.schema_version,
        skill_path="subject",
        content_hash=task.content_hash,
        audit=dict(objective="x", allowed_boundary="x", findings=[], probes=[baseline()]),
    )
    file = tmp_path / "task.json"
    file.write_text(json.dumps(data))
    assert read_task_input(str(file)).skill_path == str(subject)
    monkeypatch.chdir(tmp_path)
    assert read_task_input(json.dumps(data)).skill_path == str(subject)
    assert read_task_input("-", json.dumps(data)).skill_path == str(subject)


@pytest.mark.parametrize("target", ["existing", "inside", "nonempty", "same"])
def test_invalid_outputs_rejected_before_cost(subject, fake, tmp_path, target):
    args = ["plan", str(subject)]
    if target == "existing":
        path = tmp_path / "output.json"
        path.write_text("original")
        args += ["--output-file", str(path)]
    elif target == "inside":
        args += ["--output-file", str(subject / "report.json")]
    else:
        task = make_task(subject, tmp_path)
        data = pipeline.task_json(subject, task.content_hash, task.audit)
        args = ["run", json.dumps(data)]
        directory = tmp_path / "evidence"
        directory.mkdir()
        if target == "nonempty":
            (directory / "original").write_text("keep")
        else:
            args += ["--output-file", str(directory)]
        args += ["--evidence-dir", str(directory)]
    result = runner.invoke(cli.app, args)
    assert result.exit_code == 2 and not result.stdout
    assert fake[0].stages == [] and fake[1].calls == []


def test_static_refutation_preserved_without_replanning(subject, tmp_path):
    task = make_task(subject, tmp_path, FakeJudge(findings=[risk(dynamic="unsupported")]))
    judge, runtime = FakeJudge(refute=True), FakeRuntime()
    report = pipeline.run_task(
        task,
        tmp_path / "run",
        ScoreConfig(),
        judge=judge,
        runtime=runtime,
        content_scanner=scan,
        source_redactor=lambda x: x,
    )
    assert report["score"] == 100.0
    assert judge.stages == ["final"]
    assert read(tmp_path / "run/internal-report.json")["findings"][0]["status"] == "refuted"


@pytest.mark.parametrize("identifiers", [["r1"], [], ["unexpected"], ["r1", "r1"]])
def test_disputed_critical_scope_survives_in_the_public_report(
    subject, tmp_path, identifiers
):
    critical = risk(severity="Critical")
    task = make_task(
        subject,
        tmp_path,
        FakeJudge(findings=[critical], tasks=[baseline(), targeted()]),
    )

    class DisputedJudge(FakeJudge):
        def call(self, stage, payload, model, path):
            if stage == "critical":
                assert [claim["id"] for claim in payload["claims"]] == ["r1"]
                result = Adjudication.model_validate(
                    dict(
                        findings=[
                            critical | {"id": identifier} for identifier in identifiers
                        ],
                        baseline_completed=True,
                        baseline_refs=["atif:baseline:1"],
                    )
                )
                Judge.validate(stage, result, payload)
                return result.model_dump()
            result = super().call(stage, payload, model, path)
            result["findings"][0]["scope"] = "environment"
            return result

    report = pipeline.run_task(
        task,
        tmp_path / "run",
        ScoreConfig(),
        judge=DisputedJudge(),
        runtime=FakeRuntime(),
        content_scanner=scan,
        source_redactor=lambda value: value,
    )
    if identifiers != ["r1"]:
        assert report["decision"] == "inconclusive" and report["score"] is None
        assert "Independent Critical" in report["reason"]
        return
    assert report["decision"] == "needs_review" and report["score"] is None
    finding = report["findings"][0]
    assert finding["status"] == "unresolved"
    assert "environment" in finding["reason"] and finding["evidence"]


def test_critical_stops_and_all_tasks_receive_conclusions(subject, tmp_path):
    task = make_task(
        subject, tmp_path, FakeJudge(findings=[risk()], tasks=[baseline(), targeted()])
    )
    runtime = FakeRuntime(critical=True)
    report = pipeline.run_task(
        task,
        tmp_path / "run",
        ScoreConfig(),
        judge=FakeJudge(fail="final"),
        runtime=runtime,
        content_scanner=scan,
        source_redactor=lambda x: x,
    )
    assert report["decision"] == "blocked" and report["score"] == 0.0
    assert runtime.calls == ["baseline"]
    assert report["execution"][1]["completed"] is False
    assert "Critical" in report["execution"][1]["reason"]


def test_incomplete_observation_retries_only_failed_probe(subject, tmp_path):
    task = make_task(subject, tmp_path)
    judge, runtime = FakeJudge(), FakeRuntime(incomplete=True)
    report = pipeline.run_task(
        task,
        tmp_path / "run",
        ScoreConfig(),
        judge=judge,
        runtime=runtime,
        content_scanner=scan,
        source_redactor=lambda x: x,
    )
    assert report["decision"] == "scored" and report["score"] == 100.0
    assert report["limitations"] and not report["execution"][0]["completed"]
    assert runtime.calls == ["baseline", "baseline"]
    assert judge.stages == ["final"]


def test_observer_complete_is_not_task_complete(subject, tmp_path):
    task = make_task(subject, tmp_path)
    report = pipeline.run_task(
        task,
        tmp_path / "run",
        ScoreConfig(),
        judge=FakeJudge(fail="final"),
        runtime=FakeRuntime(),
        content_scanner=scan,
        source_redactor=lambda x: x,
    )
    assert report["execution"][0]["completed"] is False


def test_default_temporary_evidence_removed(subject, fake, monkeypatch):
    captured = []
    original = cli.run_task

    def capture(task, directory, config):
        captured.append(directory)
        return original(task, directory, config)

    monkeypatch.setattr(cli, "run_task", capture)
    task = runner.invoke(cli.app, ["plan", str(subject)]).stdout
    result = runner.invoke(cli.app, ["run", task])
    assert result.exit_code == 0
    assert captured and all(not path.exists() for path in captured)


def test_fixed_score_replay_without_provider(subject, tmp_path):
    task = make_task(subject, tmp_path, FakeJudge(findings=[risk(dynamic="unsupported")]))
    report = pipeline.run_task(
        task,
        tmp_path / "run",
        ScoreConfig(),
        judge=FakeJudge(),
        runtime=FakeRuntime(),
        content_scanner=scan,
        source_redactor=lambda x: x,
    )
    saved = read(tmp_path / "run/internal-report.json")
    replay = score_findings(
        [Finding.model_validate(f) for f in saved["findings"]], saved["failures"]
    )
    assert replay["score"] == report["score"] == 91.0


@pytest.mark.parametrize(
    "mutation",
    [
        lambda value: value["audit"]["findings"][0].update(refs=["source:missing.py:1"]),
        lambda value: value["audit"]["probes"][0].update(entry_path="../outside"),
        lambda value: value["audit"]["probes"][0].update(fixtures={"../outside": "data"}),
        lambda value: value["audit"]["probes"].extend([targeted(), dict(targeted(), id="other")]),
        lambda value: value.update(score=99),
    ],
)
def test_run_validation_precedes_runtime(subject, fake, tmp_path, mutation):
    planned = FakeJudge(findings=[risk(dynamic="unsupported")])
    task = make_task(subject, tmp_path, planned)
    data = pipeline.task_json(subject, task.content_hash, task.audit)
    mutation(data)
    result = runner.invoke(cli.app, ["run", json.dumps(data)])
    assert result.exit_code == 2 and result.stdout == ""
    assert fake[0].stages == [] and fake[1].calls == []


def test_tampered_task_completion_is_rejected():
    tasks = [baseline(), targeted()]
    catalog = {"atif:target:1": "agent says done"}
    payload = dict(
        catalog=catalog,
        audit={"probes": tasks},
        coverage=[{"probe_id": "target", "complete": True, "skill_loaded": False}],
    )
    result = Adjudication(
        findings=[],
        baseline_completed=False,
        baseline_refs=[],
        task_checks={
            "target": [
                dict(
                    expected="A concrete answer",
                    observed="Agent claimed completion",
                    status="met",
                    refs=["atif:target:1"],
                )
            ]
        },
        task_results=[
            dict(task_id="target", completed=True, reason="done", refs=["atif:target:1"])
        ],
    )
    with pytest.raises(ContractError, match="matching entry"):
        Judge.validate("final", result, payload)


def test_output_save_failure_keeps_result_and_score(subject, fake, tmp_path, monkeypatch):
    original = cli.plan_skill
    destination = tmp_path / "result.json"

    def collide(*args):
        result = original(*args)
        destination.write_text("a concurrent writer owns this")
        return result

    monkeypatch.setattr(cli, "plan_skill", collide)
    result = runner.invoke(cli.app, ["plan", str(subject), "--output-file", str(destination)])
    assert result.exit_code == 1
    assert json.loads(result.stdout)["schema_version"].endswith("/task-v1")
    assert destination.read_text() == "a concurrent writer owns this"


def test_cleanup_failure_keeps_valid_report(subject, fake, monkeypatch):
    original = cli.tempfile.TemporaryDirectory
    paths = []

    class UncleanTemporary(original):
        def cleanup(self):
            paths.append(Path(self.name))
            super().cleanup()
            raise OSError("controlled cleanup error")

    task = runner.invoke(cli.app, ["plan", str(subject)]).stdout
    monkeypatch.setattr(cli.tempfile, "TemporaryDirectory", UncleanTemporary)
    result = runner.invoke(cli.app, ["run", task])
    assert result.exit_code == 1 and json.loads(result.stdout)["score"] == 100.0
    assert "cleanup failed" in result.stderr
    assert all(not p.exists() for p in paths)


def test_catchable_interrupt_removes_default_evidence(subject, fake, monkeypatch):
    paths = []

    def interrupt(task, directory, config):
        paths.append(directory)
        (directory / "partial.json").write_text("{}")
        raise KeyboardInterrupt

    task = runner.invoke(cli.app, ["plan", str(subject)]).stdout
    monkeypatch.setattr(cli, "run_task", interrupt)
    result = runner.invoke(cli.app, ["run", task])
    assert result.exit_code == 1 and result.stdout == ""
    assert paths and not paths[0].exists()


def test_configured_key_is_scrubbed_from_results_and_retained_files(
    subject, fake, tmp_path, monkeypatch
):
    secret = "sk-" + "a9b8c7d6" * 4
    monkeypatch.setenv("ANTHROPIC_API_KEY", secret)
    task = runner.invoke(cli.app, ["plan", str(subject)]).stdout
    original = cli.run_task

    def leaking(*args):
        result = original(*args)
        (args[1] / "trace.123").write_text(secret)
        result["reason"] = secret
        return result

    monkeypatch.setattr(cli, "run_task", leaking)
    directory = tmp_path / "evidence"
    result = runner.invoke(cli.app, ["run", task, "--evidence-dir", str(directory)])
    assert result.exit_code == 0 and secret not in result.output
    assert all(secret not in path.read_text() for path in directory.rglob("*") if path.is_file())


def test_missing_optional_worker_preserves_source_assessment(subject, tmp_path, monkeypatch):
    monkeypatch.setattr(runtime, "WORKER_PYTHON", tmp_path / "not-installed")
    task = make_task(subject, tmp_path)
    judge = FakeJudge()
    report = pipeline.run_task(
        task,
        tmp_path / "run",
        ScoreConfig(),
        judge=judge,
        content_scanner=scan,
        source_redactor=lambda x: x,
    )
    assert report["decision"] == "scored" and report["score"] == 100.0
    assert "assessment-enabled sec-core package" in " ".join(report["limitations"])
    assert judge.stages == ["final"]
    assert not report["execution"][0]["completed"]


def test_real_static_scanners_are_available_without_harbor(subject):
    result = scan_skill(subject, load_source(subject), ScoreConfig())
    assert result["scanners"] == ["prompt:fast", "code:regex", "pii:local"]
    assert "masb" in result

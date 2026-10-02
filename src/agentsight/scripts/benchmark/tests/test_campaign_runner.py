from __future__ import annotations

import json
import subprocess
import sys
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from types import SimpleNamespace

import pytest

BENCHMARK_DIR = Path(__file__).parents[1]
sys.path.insert(0, str(BENCHMARK_DIR / "campaign"))
sys.path.insert(0, str(BENCHMARK_DIR / "single_run"))

import run_campaign

REAL_BPF_CLOCK_SKEW_SECONDS = run_campaign.bpf_clock_skew_seconds


@pytest.fixture(autouse=True)
def aligned_bpf_clock(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(run_campaign, "bpf_clock_skew_seconds", lambda: 0.0)


def campaign_data(tmp_path: Path) -> dict[str, object]:
    tmp_path.mkdir(parents=True, exist_ok=True)
    data = json.loads((BENCHMARK_DIR / "campaign/campaign.example.json").read_text())
    config = tmp_path / "agentsight.json"
    config.write_text(
        json.dumps(
            {
                "cmdline": {
                    "allow": [
                        {
                            "rule": ["*python*", "*mock_llm_server.py*"],
                            "agent_name": "BenchmarkHarness",
                        }
                    ]
                }
            }
        ),
        encoding="utf-8",
    )
    for name in run_campaign.VERSIONS:
        binary = tmp_path / f"agentsight-{name}"
        binary.write_bytes(name.encode())
        binary.chmod(0o755)
        data["versions"][name] = {
            "commit": name,
            "binary": str(binary),
            "config": str(config),
            "db": str(tmp_path / f"{name}.db"),
        }
    return data


def test_json_hash_and_runtime_campaign_are_non_mutating(tmp_path: Path) -> None:
    path = tmp_path / "nested/value.json"
    run_campaign.write_json(path, {"value": 1})
    assert run_campaign.read_json(path) == {"value": 1}
    assert len(run_campaign.file_sha256(path)) == 64
    path.write_text("[]", encoding="utf-8")
    with pytest.raises(TypeError, match="expected a JSON object"):
        run_campaign.read_json(path)

    data = campaign_data(tmp_path)
    source = Path(data["versions"]["baseline"]["config"])
    source_config = json.loads(source.read_text(encoding="utf-8"))
    source_config["cmdline"]["allow"].append(
        {"rule": ["*codex*"], "agent_name": "AmbientCodex"}
    )
    source_config["cmdline"]["deny"] = [{"rule": ["*ignored*"]}]
    source.write_text(json.dumps(source_config), encoding="utf-8")
    source_before = source.read_bytes()
    prepared = run_campaign.runtime_campaign(data, tmp_path / "results")
    assert "log_file" not in data["versions"]["baseline"]
    assert prepared["versions"]["baseline"]["log_file"].endswith(
        "results/runtime/baseline/agentsight.log"
    )
    assert prepared["versions"]["baseline"]["metrics_file"].endswith(
        "results/runtime/baseline/internal-metrics.txt"
    )
    runtime_config = Path(prepared["versions"]["baseline"]["config"])
    assert runtime_config != source
    runtime_data = run_campaign.read_json(runtime_config)
    assert runtime_data["cmdline"]["allow"] == [run_campaign.BENCHMARK_ALLOW_RULE]
    assert runtime_data["cmdline"]["deny"] == [{"rule": ["*ignored*"]}]
    assert prepared["versions"]["baseline"]["source_config"] == str(source)
    assert run_campaign.campaign_manifest.observes_mock_server(runtime_config)
    assert source.read_bytes() == source_before
    campaign_path = tmp_path / "campaign.json"
    campaign_path.write_text(json.dumps(data), encoding="utf-8")
    frozen = run_campaign.campaign_manifest.frozen_inputs(campaign_path, prepared)
    assert frozen["comparison_mode"] == "ab_comparison"
    assert frozen["versions"]["baseline"]["source_config"] == str(source)
    assert frozen["versions"]["baseline"]["source_config_sha256"]

    malformed = campaign_data(tmp_path / "malformed")
    malformed_config = Path(malformed["versions"]["baseline"]["config"])
    malformed_config.write_text('{"cmdline": []}', encoding="utf-8")
    with pytest.raises(TypeError, match="cmdline must be an object"):
        run_campaign.runtime_campaign(malformed, tmp_path / "malformed-results")


def test_bpf_clock_skew_detects_time_spent_suspended(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    uptime = tmp_path / "uptime"
    uptime.write_text("46529.25 0.00\n", encoding="ascii")
    monkeypatch.setattr(run_campaign, "PROC_UPTIME_PATH", uptime)
    monkeypatch.setattr(run_campaign.time, "monotonic", lambda: 10.0)
    assert REAL_BPF_CLOCK_SKEW_SECONDS() == pytest.approx(46519.25)


def test_active_tracers_reads_only_agentsight_trace_processes(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    for pid, command in (
        ("101", b"/opt/agentsight\0trace\0"),
        ("102", b"/opt/agentsight\0serve\0"),
        ("103", b"/usr/bin/python3\0trace\0"),
    ):
        proc = tmp_path / pid
        proc.mkdir()
        (proc / "cmdline").write_bytes(command)
    (tmp_path / "not-a-pid").mkdir()
    (tmp_path / "104").mkdir()
    monkeypatch.setattr(run_campaign, "PROC_ROOT", tmp_path)
    monkeypatch.setattr(run_campaign.os, "getpid", lambda: 999)
    assert run_campaign.active_tracers() == [101]


def test_ebpf_privilege_accepts_root_or_required_capabilities(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    binary = tmp_path / "agentsight"
    monkeypatch.setattr(run_campaign.os, "geteuid", lambda: 0)
    assert run_campaign.has_ebpf_privilege(binary) is True

    monkeypatch.setattr(run_campaign.os, "geteuid", lambda: 1000)
    monkeypatch.setattr(run_campaign.shutil, "which", lambda _: None)
    assert run_campaign.has_ebpf_privilege(binary) is False
    monkeypatch.setattr(run_campaign.shutil, "which", lambda _: "/usr/sbin/getcap")
    monkeypatch.setattr(
        run_campaign.subprocess,
        "run",
        lambda *args, **kwargs: SimpleNamespace(
            stdout=f"{binary} cap_perfmon,cap_bpf=ep\n"
        ),
    )
    assert run_campaign.has_ebpf_privilege(binary) is True
    monkeypatch.setattr(
        run_campaign.subprocess,
        "run",
        lambda *args, **kwargs: SimpleNamespace(
            stdout=f"{binary} cap_perfmon,cap_bpf=p\n"
        ),
    )
    assert run_campaign.has_ebpf_privilege(binary) is False


def test_preflight_accepts_formal_on_x86_and_enforces_identity(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path)
    btf = tmp_path / "vmlinux"
    btf.write_bytes(b"btf")
    monkeypatch.setattr(run_campaign, "BTF_PATH", btf)
    monkeypatch.setattr(run_campaign.shutil, "which", lambda tool: f"/usr/bin/{tool}")
    monkeypatch.setattr(run_campaign, "has_ebpf_privilege", lambda _: True)
    monkeypatch.setattr(run_campaign, "active_tracers", list)
    assert run_campaign.preflight_issues(data, "quick") == []
    monkeypatch.setattr(run_campaign, "bpf_clock_skew_seconds", lambda: 301.0)
    assert any(
        "reboot the host" in issue
        for issue in run_campaign.preflight_issues(data, "quick")
    )
    monkeypatch.setattr(run_campaign, "bpf_clock_skew_seconds", lambda: 0.0)

    data["versions"]["optimized"]["commit"] = data["versions"]["baseline"]["commit"]
    data["versions"]["optimized"]["binary"] = data["versions"]["baseline"]["binary"]
    issues = run_campaign.preflight_issues(data, "formal")
    assert any("commits must differ" in issue for issue in issues)
    assert any("binaries are identical" in issue for issue in issues)
    assert run_campaign.preflight_issues(data, "formal", True) == []

    optimized = tmp_path / "agentsight-formal-optimized"
    optimized.write_bytes(b"different optimized binary")
    optimized.chmod(0o755)
    data["versions"]["optimized"]["commit"] = "optimized"
    data["versions"]["optimized"]["binary"] = str(optimized)
    assert run_campaign.preflight_issues(data, "formal") == []


def test_preflight_reports_all_actionable_input_problems(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path)
    invalid = tmp_path / "invalid.json"
    invalid.write_text("not-json", encoding="utf-8")
    data["versions"]["baseline"].update(
        {"binary": "relative-agent", "config": str(invalid), "db": "events.db"}
    )
    missing_parent = tmp_path / "missing" / "events.db"
    invalid_shape = tmp_path / "invalid-shape.json"
    invalid_shape.write_text("[]", encoding="utf-8")
    data["versions"]["optimized"].update(
        {"config": str(invalid_shape), "db": str(missing_parent)}
    )
    monkeypatch.setattr(run_campaign, "BTF_PATH", tmp_path / "missing-btf")
    monkeypatch.setattr(
        run_campaign.shutil,
        "which",
        lambda tool: None if tool == "k6" else f"/usr/bin/{tool}",
    )
    monkeypatch.setattr(run_campaign, "has_ebpf_privilege", lambda _: False)
    monkeypatch.setattr(run_campaign, "active_tracers", lambda: [123, 456])
    issues = run_campaign.preflight_issues(data, "quick")
    assert any("required tool is missing: k6" in issue for issue in issues)
    assert any("readable kernel BTF" in issue for issue in issues)
    assert any("binary path must be absolute" in issue for issue in issues)
    assert any("config is not readable JSON" in issue for issue in issues)
    assert sum("config is not readable JSON" in issue for issue in issues) == 2
    assert any("database path must be absolute" in issue for issue in issues)
    assert any("database directory does not exist" in issue for issue in issues)
    assert any("lacks eBPF privilege" in issue for issue in issues)
    assert any("123, 456" in issue for issue in issues)

    data["schema_version"] = 2
    assert run_campaign.preflight_issues(data, "quick") == [
        "campaign schema_version must be 1"
    ]

    data = campaign_data(tmp_path / "shape")
    shape_config = Path(data["versions"]["baseline"]["config"])
    shape_config.write_text('{"cmdline": {"allow": {}}}', encoding="utf-8")
    monkeypatch.setattr(run_campaign, "BTF_PATH", btf := tmp_path / "shape-btf")
    btf.write_bytes(b"btf")
    monkeypatch.setattr(run_campaign, "active_tracers", list)
    monkeypatch.setattr(run_campaign, "has_ebpf_privilege", lambda _: True)
    shape_issues = run_campaign.preflight_issues(data, "quick")
    assert sum("cmdline.allow must be a list" in issue for issue in shape_issues) == 2


def test_preflight_reports_host_resource_safety_problems(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path / "inputs")
    results = tmp_path / "results"
    results.mkdir()
    btf = tmp_path / "vmlinux"
    btf.write_bytes(b"btf")
    monkeypatch.setattr(run_campaign, "BTF_PATH", btf)
    monkeypatch.setattr(run_campaign.shutil, "which", lambda tool: f"/usr/bin/{tool}")
    monkeypatch.setattr(run_campaign, "has_ebpf_privilege", lambda _: True)
    monkeypatch.setattr(run_campaign, "active_tracers", list)
    monkeypatch.setattr(run_campaign, "allocated_size", lambda _: 29 * run_campaign.GIB)
    monkeypatch.setattr(
        run_campaign.shutil,
        "disk_usage",
        lambda _: SimpleNamespace(free=4 * run_campaign.GIB),
    )
    monkeypatch.setattr(
        run_campaign, "available_memory_bytes", lambda: 1024 * run_campaign.MIB
    )
    issues = run_campaign.preflight_issues(data, "quick", results=results)
    assert any("available disk space" in issue for issue in issues)
    assert any("available memory" in issue for issue in issues)
    assert any("results directory already reached" in issue for issue in issues)


def test_isolated_storage_is_per_version_and_inside_results(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path / "inputs")
    mount_target = tmp_path / "host-state"
    mount_target.mkdir()
    expected_database = mount_target / run_campaign.GENAI_DATABASE_NAME
    for name in run_campaign.VERSIONS:
        data["versions"][name]["db"] = str(expected_database)
    results = tmp_path / "results"
    storage_root = results / "runtime/storage"
    monkeypatch.setattr(run_campaign, "STATE_MOUNT_TARGET", mount_target)
    monkeypatch.setattr(run_campaign.os, "geteuid", lambda: 0)
    monkeypatch.setattr(run_campaign, "in_private_mount_namespace", lambda: True)

    prepared = run_campaign.prepare_isolated_storage(data, results, storage_root)
    assert prepared == storage_root.resolve()
    assert (prepared / "baseline").is_dir()
    assert (prepared / "optimized").is_dir()
    metadata = run_campaign.read_json(results / "runtime/storage-isolation.json")
    assert metadata["mount_target"] == str(mount_target)
    assert metadata["versions"]["baseline"] != metadata["versions"]["optimized"]

    data["versions"]["optimized"]["db"] = str(tmp_path / "wrong.db")
    with pytest.raises(ValueError, match="optimized database must be"):
        run_campaign.prepare_isolated_storage(data, results, storage_root)
    data["versions"]["optimized"]["db"] = str(expected_database)
    with pytest.raises(ValueError, match="inside the results"):
        run_campaign.prepare_isolated_storage(data, results, tmp_path / "outside")
    monkeypatch.setattr(run_campaign, "in_private_mount_namespace", lambda: False)
    with pytest.raises(RuntimeError, match="private mount namespace"):
        run_campaign.prepare_isolated_storage(data, results, storage_root)
    monkeypatch.setattr(run_campaign.os, "geteuid", lambda: 1000)
    with pytest.raises(PermissionError, match="root runner"):
        run_campaign.prepare_isolated_storage(data, results, storage_root)


def test_mounted_storage_binds_and_unmounts(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    storage = tmp_path / "baseline"
    storage.mkdir()
    mount_target = tmp_path / "host-state"
    mount_target.mkdir()
    commands: list[list[str]] = []

    def fake_run(command: list[str], **_: object) -> SimpleNamespace:
        commands.append(command)
        return SimpleNamespace(returncode=0, stdout="", stderr="")

    monkeypatch.setattr(run_campaign, "STATE_MOUNT_TARGET", mount_target)
    monkeypatch.setattr(run_campaign.subprocess, "run", fake_run)
    with run_campaign.mounted_storage(storage):
        pass
    assert commands == [
        ["mount", "--bind", str(storage), str(mount_target)],
        ["umount", str(mount_target)],
    ]


def executable_script(path: Path, body: str) -> None:
    path.write_text(f"#!{sys.executable}\n{body}", encoding="utf-8")
    path.chmod(0o755)


def test_tracer_process_waits_for_new_ready_marker_and_stops_child(
    tmp_path: Path,
) -> None:
    tracer = tmp_path / "fake-agentsight"
    executable_script(
        tracer,
        "import signal, sys, time\n"
        "import os\n"
        "signal.signal(signal.SIGINT, lambda *_: sys.exit(0))\n"
        "open(os.environ['AGENTSIGHT_METRICS_FILE'], 'w').write('completed=1\\n')\n"
        "print('AgentSight initialized:', flush=True)\n"
        "while True: time.sleep(0.05)\n",
    )
    log = tmp_path / "agentsight.log"
    version = {
        "binary": str(tracer),
        "config": str(tmp_path / "config.json"),
        "log_file": str(log),
        "metrics_file": str(tmp_path / "metrics.txt"),
    }
    with run_campaign.TracerProcess(version, 2) as pid:
        assert Path(f"/proc/{pid}").exists()
    assert "AgentSight initialized:" in log.read_text()
    assert (tmp_path / "metrics.txt").read_text() == "completed=1\n"

    silent = tmp_path / "silent-agentsight"
    executable_script(
        silent,
        "import signal, sys, time\n"
        "signal.signal(signal.SIGINT, lambda *_: sys.exit(0))\n"
        "while True: time.sleep(0.05)\n",
    )
    version["binary"] = str(silent)
    with pytest.raises(RuntimeError, match="did not become ready"):
        run_campaign.TracerProcess(version, 0.25).__enter__()


def test_tracer_process_disables_inherited_metrics_for_off_run(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    tracer = tmp_path / "fake-agentsight"
    executable_script(
        tracer,
        "import os, signal, sys, time\n"
        "signal.signal(signal.SIGINT, lambda *_: sys.exit(0))\n"
        "print('metrics=' + str(os.environ.get('AGENTSIGHT_METRICS_FILE')), flush=True)\n"
        "print('AgentSight initialized:', flush=True)\n"
        "while True: time.sleep(0.05)\n",
    )
    monkeypatch.setenv("AGENTSIGHT_METRICS_FILE", str(tmp_path / "inherited.txt"))
    version = {
        "binary": str(tracer),
        "config": str(tmp_path / "config.json"),
        "log_file": str(tmp_path / "off.log"),
    }
    with run_campaign.TracerProcess(version, 2):
        pass
    assert "metrics=None" in (tmp_path / "off.log").read_text()


def test_tracer_process_reports_early_exit(tmp_path: Path) -> None:
    tracer = tmp_path / "failed-agentsight"
    executable_script(tracer, "raise SystemExit(7)\n")
    version = {
        "binary": str(tracer),
        "config": str(tmp_path / "config.json"),
        "log_file": str(tmp_path / "failed.log"),
        "metrics_file": str(tmp_path / "failed-metrics.txt"),
    }
    with pytest.raises(RuntimeError, match="status 7"):
        run_campaign.TracerProcess(version, 2).__enter__()


def test_benchmark_server_starts_before_tracers_and_stops_cleanly(
    tmp_path: Path,
) -> None:
    import socket

    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    load = {"protocol": "sse", "chunks": 2, "chunk_bytes": 16}
    server = run_campaign.BenchmarkServer(load, tmp_path, port=port)
    with server:
        assert server.process is not None
        assert server.process.poll() is None
        assert (tmp_path / "runtime/mock-server/request-logs").is_dir()
        health = subprocess.run(
            ["curl", "-ksf", f"https://127.0.0.1:{port}/healthz"],
            capture_output=True,
            check=False,
        )
        assert health.returncode == 0
        assert health.stdout == b"OK"
    assert server.process is not None
    assert server.process.poll() == 0


def test_state_and_stage_execution_are_resumable(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    state_path = tmp_path / "state.json"
    assert run_campaign.load_state(state_path, "quick")["completed_stages"] == []
    run_campaign.write_json(
        state_path,
        {"schema_version": 1, "mode": "formal", "completed_stages": []},
    )
    with pytest.raises(ValueError, match="incompatible"):
        run_campaign.load_state(state_path, "quick")
    run_campaign.write_json(
        state_path,
        {"schema_version": 1, "mode": "quick", "completed_stages": "bad"},
    )
    with pytest.raises(TypeError, match="invalid runner checkpoint"):
        run_campaign.load_state(state_path, "quick")

    data = run_campaign.runtime_campaign(campaign_data(tmp_path), tmp_path / "results")
    calls: list[tuple[str, str, bool]] = []
    modes: list[bool] = []

    class FakeTracer:
        def __init__(self, version: dict[str, object], timeout: float) -> None:
            assert timeout == 3
            modes.append(bool(version.get("metrics_file")))

        def __enter__(self) -> int:
            return 999

        def __exit__(self, *_: object) -> None:
            pass

    monkeypatch.setattr(run_campaign, "TracerProcess", FakeTracer)

    def fake_capacity(
        data: dict[str, object],
        results: Path,
        version: str,
        pid: int | None,
        resume: bool,
        process_factory: object,
    ) -> None:
        assert pid is None
        assert callable(process_factory)
        calls.append((version, "capacity", resume))

    monkeypatch.setattr(run_campaign.campaign, "capacity", fake_capacity)
    monkeypatch.setattr(
        run_campaign.campaign,
        "run_scenario",
        lambda data, results, version, scenario, pid, resume: calls.append(
            (version, scenario, resume)
        ),
    )
    def fake_run_once(
        data: dict[str, object],
        results: Path,
        version: str,
        scenario: str,
        label: str,
        qps: int,
        duration: int,
        warmup: int,
        repetition: int,
        pid: int,
        resume: bool,
    ) -> None:
        assert (qps, duration, warmup, repetition, pid) == (10, 30, 0, 1, 999)
        assert label == "qps-10"
        assert "metrics_file" not in data["versions"][version]
        calls.append((version, scenario, resume))

    monkeypatch.setattr(run_campaign.campaign, "run_once", fake_run_once)
    results = tmp_path / "results"
    run_campaign.execute_stages(data, results, "formal", True, 3)
    assert len(calls) == len(run_campaign.FORMAL_STAGES)
    assert calls[:2] == [
        ("baseline", "capacity", True),
        ("optimized", "capacity", True),
    ]
    run_campaign.execute_stages(data, results, "formal", True, 3)
    assert len(calls) == len(run_campaign.FORMAL_STAGES)

    quick_results = tmp_path / "quick-results"
    mounts: list[Path] = []

    @contextmanager
    def fake_storage(path: Path) -> Iterator[None]:
        mounts.append(path)
        yield

    monkeypatch.setattr(run_campaign, "mounted_storage", fake_storage)
    storage_root = tmp_path / "isolated-storage"
    run_campaign.execute_stages(data, quick_results, "quick", False, 3, storage_root)
    assert calls[-3:] == [
        ("baseline", "smoke", False),
        ("optimized", "metrics_off", False),
        ("optimized", "smoke", False),
    ]
    assert modes[-3:] == [True, False, True]
    assert mounts == [
        storage_root / "baseline",
        storage_root / "optimized",
        storage_root / "optimized",
    ]


def write_quick_run(results: Path, version: str, scenario: str) -> None:
    run_campaign.write_json(
        results / f"runs/{scenario}/{version}/qps-10/rep-1/run-result.json",
        {
            "version": version,
            "metrics_enabled": scenario == "smoke",
            "evaluation": {"verdict": "PASS"},
            "summary": {
                "http_success_rate": 1.0,
                "trace_completeness": 1.0,
                "capture_loss_rate": 0.0,
                "process_survived": True,
                "effective_qps": 10.0,
                "latency_ms": {"p99": 20.0},
                "resources": {"cpu_pct": {"avg": 10.0}, "rss_mb": {"avg": 100.0}},
            },
        },
    )


def test_run_command_and_quick_report_verdict(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    run_campaign.run_command("true command", ["/bin/true"], tmp_path)
    with pytest.raises(RuntimeError, match="status 1"):
        run_campaign.run_command("false command", ["/bin/false"], tmp_path)
    assert "[OK] true command" in capsys.readouterr().out

    write_quick_run(tmp_path, "baseline", "smoke")
    write_quick_run(tmp_path, "optimized", "metrics_off")
    write_quick_run(tmp_path, "optimized", "smoke")
    snapshot = tmp_path / "runtime/optimized/internal-metrics.txt"
    snapshot.parent.mkdir(parents=True)
    snapshot.write_text('agentsight_stage_calls_total{stage="parser"} 1\n')
    enabled = next(tmp_path.glob("runs/smoke/optimized/**/run-result.json"))
    enabled_result = run_campaign.read_json(enabled)
    enabled_result["summary"]["resources"]["cpu_pct"]["avg"] = 12.0
    run_campaign.write_json(enabled, enabled_result)
    report, verdict = run_campaign.write_quick_report(tmp_path)
    assert verdict == "PASS"
    values = run_campaign.read_json(report)
    assert values["verdict"] == "PASS"
    cpu = values["observability_overhead"]["measurements"]["cpu_avg_pct"]
    assert cpu == {
        "disabled": 10.0,
        "enabled": 12.0,
        "delta_enabled_minus_disabled": 2.0,
        "relative_change_pct": pytest.approx(20.0),
    }
    assert "| cpu_avg_pct | 10.000 | 12.000 | 2.000 | 20.000% |" in (
        tmp_path / "quick-report.md"
    ).read_text()
    snapshot.unlink()
    assert run_campaign.write_quick_report(tmp_path)[1] == "INCONCLUSIVE"
    snapshot.write_text('agentsight_stage_calls_total{stage="parser"} 1\n')
    disabled = next(tmp_path.glob("runs/metrics_off/optimized/**/run-result.json"))
    disabled_result = run_campaign.read_json(disabled)
    disabled_result["metrics_enabled"] = True
    run_campaign.write_json(disabled, disabled_result)
    assert run_campaign.write_quick_report(tmp_path)[1] == "INCONCLUSIVE"
    disabled_result["metrics_enabled"] = False
    run_campaign.write_json(disabled, disabled_result)
    value = run_campaign.read_json(enabled)
    value["evaluation"]["verdict"] = "FAIL"
    run_campaign.write_json(enabled, value)
    assert run_campaign.write_quick_report(tmp_path)[1] == "FAIL"


def test_main_check_only_preflight_failure_and_nonempty_results(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path)
    config = tmp_path / "campaign.json"
    config.write_text(json.dumps(data), encoding="utf-8")
    results = tmp_path / "results"
    monkeypatch.setattr(
        run_campaign,
        "preflight_issues",
        lambda data, mode, allow_identical_versions=False, results=None: [],
    )
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "run_campaign.py",
            "--campaign",
            str(config),
            "--results",
            str(results),
            "--check-only",
        ],
    )
    assert run_campaign.main() == 0

    monkeypatch.setattr(
        run_campaign,
        "preflight_issues",
        lambda data, mode, allow_identical_versions=False, results=None: ["bad"],
    )
    assert run_campaign.main() == 2
    results.mkdir()
    (results / "existing").write_text("data", encoding="utf-8")
    with pytest.raises(SystemExit, match="results directory is not empty"):
        run_campaign.main()

    results_file = tmp_path / "results-file"
    results_file.write_text("not a directory", encoding="utf-8")
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "run_campaign.py",
            "--campaign",
            str(config),
            "--results",
            str(results_file),
        ],
    )
    with pytest.raises(SystemExit, match="results path is not a directory"):
        run_campaign.main()

    monkeypatch.setattr(
        sys,
        "argv",
        [
            "run_campaign.py",
            "--campaign",
            str(tmp_path / "missing.json"),
            "--results",
            str(tmp_path / "missing-results"),
        ],
    )
    with pytest.raises(SystemExit, match="cannot read campaign JSON"):
        run_campaign.main()


def test_main_quick_and_formal_return_the_campaign_verdict(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    data = campaign_data(tmp_path)
    config = tmp_path / "campaign.json"
    config.write_text(json.dumps(data), encoding="utf-8")
    monkeypatch.setattr(
        run_campaign,
        "preflight_issues",
        lambda data, mode, allow_identical_versions=False, results=None: [],
    )
    frozen: list[Path] = []
    comparison_modes: list[str] = []
    monkeypatch.setattr(
        run_campaign.campaign_manifest,
        "ensure_manifest",
        lambda results, campaign, data: (
            frozen.append(results),
            comparison_modes.append(data["comparison_mode"]),
        ),
    )

    def fake_execute(
        data: dict[str, object],
        results: Path,
        mode: str,
        resume: bool,
        timeout: float,
        storage_root: Path | None,
    ) -> None:
        assert storage_root is None
        if mode != "quick":
            return
        write_quick_run(results, "baseline", "smoke")
        write_quick_run(results, "optimized", "metrics_off")
        write_quick_run(results, "optimized", "smoke")
        snapshot = results / "runtime/optimized/internal-metrics.txt"
        snapshot.parent.mkdir(parents=True, exist_ok=True)
        snapshot.write_text('agentsight_stage_calls_total{stage="parser"} 1\n')

    monkeypatch.setattr(run_campaign, "execute_stages", fake_execute)
    quick_results = tmp_path / "quick"
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "run_campaign.py",
            "--campaign",
            str(config),
            "--results",
            str(quick_results),
        ],
    )
    assert run_campaign.main() == 0
    assert len(frozen) == 2

    commands: list[str] = []

    def fake_command(label: str, command: list[str], cwd: Path) -> None:
        commands.append(label)
        if label == "aggregate report":
            run_campaign.write_json(
                formal_results / "final-summary.json", {"verdict": "PASS"}
            )

    formal_results = tmp_path / "formal"
    monkeypatch.setattr(run_campaign, "run_command", fake_command)
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "run_campaign.py",
            "--campaign",
            str(config),
            "--results",
            str(formal_results),
            "--mode",
            "formal",
            "--allow-identical-versions",
        ],
    )
    assert run_campaign.main() == 0
    assert commands == ["full regression", "aggregate report"]
    assert comparison_modes[-2:] == ["aa_calibration", "aa_calibration"]

    commands.clear()

    def failed_regression(label: str, command: list[str], cwd: Path) -> None:
        commands.append(label)
        if label == "full regression":
            raise RuntimeError("full regression failed with status 1")
        run_campaign.write_json(
            failed_results / "final-summary.json", {"verdict": "INCONCLUSIVE"}
        )

    failed_results = tmp_path / "formal-failed"
    monkeypatch.setattr(run_campaign, "run_command", failed_regression)
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "run_campaign.py",
            "--campaign",
            str(config),
            "--results",
            str(failed_results),
            "--mode",
            "formal",
        ],
    )
    assert run_campaign.main() == 1
    assert commands == ["full regression", "aggregate report"]

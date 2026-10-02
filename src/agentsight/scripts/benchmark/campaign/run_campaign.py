#!/usr/bin/env python3
"""Run an AgentSight comparison campaign without modifying either binary."""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
import shutil
import signal
import subprocess
import sys
import time
from collections.abc import Iterator
from contextlib import contextmanager, nullcontext
from pathlib import Path
from typing import Any, Self, TextIO

SCRIPT_DIR = Path(__file__).resolve().parent
SINGLE_RUN_DIR = SCRIPT_DIR.parent / "single_run"
sys.path.insert(0, str(SINGLE_RUN_DIR))

import campaign
import campaign_manifest
from campaign_config import safety_settings, validate_campaign
from collect_metrics import GIB, MIB, allocated_size, available_memory_bytes

BTF_PATH = Path("/sys/kernel/btf/vmlinux")
PROC_ROOT = Path("/proc")
PROC_UPTIME_PATH = Path("/proc/uptime")
STATE_MOUNT_TARGET = Path("/var/log/sysak/.agentsight")
GENAI_DATABASE_NAME = "genai_events.db"
READY_MARKER = "AgentSight initialized:"
VERSIONS = ("baseline", "optimized")
BENCHMARK_ALLOW_RULE = {
    "rule": ["*python*", "*mock_llm_server.py*"],
    "agent_name": "BenchmarkHarness",
}
FORMAL_STAGES = (
    ("baseline", "capacity"),
    ("optimized", "capacity"),
    ("baseline", "matrix"),
    ("baseline", "soak"),
    ("baseline", "recovery"),
    ("baseline", "fault"),
    ("optimized", "matrix"),
    ("optimized", "soak"),
    ("optimized", "recovery"),
    ("optimized", "fault"),
)
QUICK_STAGES = (
    ("baseline", "smoke"),
    ("optimized", "metrics_off"),
    ("optimized", "smoke"),
)
QUICK_OVERHEAD_METRICS = {
    "cpu_avg_pct": ("resources", "cpu_pct", "avg"),
    "rss_avg_mb": ("resources", "rss_mb", "avg"),
    "latency_p99_ms": ("latency_ms", "p99"),
    "effective_qps": ("effective_qps",),
    "trace_completeness": ("trace_completeness",),
    "capture_loss_rate": ("capture_loss_rate",),
}
SAFETY_EXIT_CODE = 75
MAX_BPF_CLOCK_SKEW_SECONDS = 1.0


def read_json(path: Path) -> dict[str, Any]:
    """Read a JSON object."""
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise TypeError(f"expected a JSON object: {path}")
    return value


def write_json(path: Path, value: dict[str, Any]) -> None:
    """Atomically replace a JSON checkpoint."""
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    temporary.write_text(
        json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    temporary.replace(path)


def file_sha256(path: Path) -> str:
    """Return the digest used to prove which binary was executed."""
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def bpf_clock_skew_seconds() -> float:
    """Measure suspend time that AgentSight's current ktime conversion omits."""
    uptime = float(PROC_UPTIME_PATH.read_text(encoding="ascii").split()[0])
    return abs(uptime - time.monotonic())


def runtime_campaign(data: dict[str, Any], results: Path) -> dict[str, Any]:
    """Create runner-owned config copies and logs without changing test inputs."""
    prepared = copy.deepcopy(data)
    for name in VERSIONS:
        runtime = (results / "runtime" / name).resolve()
        source_config = Path(prepared["versions"][name]["config"]).resolve()
        config = read_json(source_config)
        cmdline = config.setdefault("cmdline", {})
        if not isinstance(cmdline, dict):
            raise TypeError(f"cmdline must be an object: {source_config}")
        if not isinstance(cmdline.get("allow", []), list):
            raise TypeError(f"cmdline.allow must be a list: {source_config}")
        # Ambient agents would contend for the same eBPF and userspace queues,
        # making before/after measurements depend on unrelated host activity.
        cmdline["allow"] = [copy.deepcopy(BENCHMARK_ALLOW_RULE)]
        runtime_config = runtime / "agentsight.json"
        write_json(runtime_config, config)
        prepared["versions"][name]["source_config"] = str(source_config)
        prepared["versions"][name]["config"] = str(runtime_config)
        prepared["versions"][name]["log_file"] = str(runtime / "agentsight.log")
        prepared["versions"][name]["metrics_file"] = str(
            runtime / "internal-metrics.txt"
        )
    return prepared


def active_tracers() -> list[int]:
    """Return live AgentSight trace processes that would corrupt isolation."""
    found = []
    for proc in PROC_ROOT.iterdir():
        if not proc.name.isdigit() or int(proc.name) == os.getpid():
            continue
        try:
            arguments = proc.joinpath("cmdline").read_bytes().split(b"\0")
        except OSError:
            continue
        decoded = [item.decode(errors="replace") for item in arguments if item]
        if (
            len(decoded) >= 2
            and Path(decoded[0]).name == "agentsight"
            and "trace" in decoded[1:]
        ):
            found.append(int(proc.name))
    return sorted(found)


def in_private_mount_namespace() -> bool:
    """Return whether mounts made by this process are isolated from the host."""
    try:
        return os.stat("/proc/self/ns/mnt").st_ino != os.stat("/proc/1/ns/mnt").st_ino
    except OSError:
        return False


def prepare_isolated_storage(
    data: dict[str, Any], results: Path, storage_root: Path
) -> Path:
    """Validate and record the runner-owned per-version SQLite directories."""
    if os.geteuid() != 0:
        raise PermissionError("isolated storage requires a root runner")
    if not in_private_mount_namespace():
        raise RuntimeError(
            "isolated storage requires a private mount namespace; use reproduce_campaign.sh"
        )
    if not STATE_MOUNT_TARGET.is_dir():
        raise FileNotFoundError(
            f"AgentSight state directory is missing: {STATE_MOUNT_TARGET}"
        )
    resolved_results = results.resolve()
    resolved_storage = storage_root.resolve()
    try:
        relative = resolved_storage.relative_to(resolved_results)
    except ValueError as error:
        raise ValueError(
            "isolated storage must be inside the results directory"
        ) from error
    if relative == Path("."):
        raise ValueError("isolated storage cannot replace the results directory")
    expected_database = (STATE_MOUNT_TARGET / GENAI_DATABASE_NAME).resolve()
    for name in VERSIONS:
        configured_database = Path(data["versions"][name]["db"]).resolve()
        if configured_database != expected_database:
            raise ValueError(
                f"{name} database must be {expected_database} when storage isolation is enabled"
            )
        (resolved_storage / name).mkdir(parents=True, exist_ok=True)
    write_json(
        results / "runtime" / "storage-isolation.json",
        {
            "schema_version": 1,
            "mount_target": str(STATE_MOUNT_TARGET),
            "database_name": GENAI_DATABASE_NAME,
            "versions": {
                name: str((resolved_storage / name).resolve()) for name in VERSIONS
            },
        },
    )
    return resolved_storage


@contextmanager
def mounted_storage(storage_directory: Path) -> Iterator[None]:
    """Expose one version's state only inside the runner's mount namespace."""
    command = ["mount", "--bind", str(storage_directory), str(STATE_MOUNT_TARGET)]
    mounted = subprocess.run(command, capture_output=True, text=True, check=False)
    if mounted.returncode != 0:
        detail = (mounted.stderr or mounted.stdout).strip()
        raise RuntimeError(
            f"failed to mount isolated AgentSight storage {storage_directory}: {detail}"
        )
    print(f"[STORAGE] isolated AgentSight state: {storage_directory}", flush=True)
    try:
        yield
    finally:
        unmounted = subprocess.run(
            ["umount", str(STATE_MOUNT_TARGET)],
            capture_output=True,
            text=True,
            check=False,
        )
        if unmounted.returncode != 0:
            detail = (unmounted.stderr or unmounted.stdout).strip()
            raise RuntimeError(
                f"failed to unmount isolated AgentSight storage: {detail}"
            )


def has_ebpf_privilege(binary: Path) -> bool:
    """Accept root or the capabilities used by the documented trace workflow."""
    if os.geteuid() == 0:
        return True
    getcap = shutil.which("getcap")
    if not getcap:
        return False
    result = subprocess.run(
        [getcap, str(binary)], capture_output=True, text=True, check=False
    )
    fields = result.stdout.lower().split()
    capability_field = fields[-1] if fields else ""
    if "=" not in capability_field:
        return False
    names, flags = capability_field.rsplit("=", 1)
    return {"cap_bpf", "cap_perfmon"} <= set(names.split(",")) and "e" in flags


def preflight_issues(
    data: dict[str, Any],
    mode: str,
    allow_identical_versions: bool = False,
    results: Path | None = None,
) -> list[str]:
    """Return every actionable problem instead of failing one path at a time."""
    issues = []
    try:
        validate_campaign(data)
    except (TypeError, ValueError) as error:
        return [str(error)]
    for tool in ("openssl", "curl", "gzip", "k6"):
        if shutil.which(tool) is None:
            issues.append(f"required tool is missing: {tool}")
    if not BTF_PATH.is_file() or not os.access(BTF_PATH, os.R_OK):
        issues.append("readable kernel BTF is required: /sys/kernel/btf/vmlinux")
    try:
        clock_skew = bpf_clock_skew_seconds()
    except (OSError, ValueError, IndexError) as error:
        issues.append(f"cannot verify BPF clock alignment: {error}")
    else:
        if clock_skew > MAX_BPF_CLOCK_SKEW_SECONDS:
            issues.append(
                "BPF clock conversion is unsafe after system suspend: "
                f"/proc/uptime and CLOCK_MONOTONIC differ by {clock_skew:.1f}s; "
                "reboot the host and disable sleep before starting a campaign"
            )
    for name in VERSIONS:
        version = data["versions"][name]
        binary = Path(version["binary"])
        config = Path(version["config"])
        db = Path(version["db"])
        if not binary.is_absolute():
            issues.append(f"{name} binary path must be absolute: {binary}")
        elif not binary.is_file():
            issues.append(f"{name} binary does not exist: {binary}")
        elif not os.access(binary, os.X_OK):
            issues.append(f"{name} binary is not executable: {binary}")
        elif not has_ebpf_privilege(binary):
            issues.append(
                f"{name} binary lacks eBPF privilege; run this runner as root or grant cap_bpf,cap_perfmon: {binary}"
            )
        if not config.is_absolute():
            issues.append(f"{name} config path must be absolute: {config}")
        elif not config.is_file():
            issues.append(f"{name} config does not exist: {config}")
        else:
            try:
                config_data = read_json(config)
            except (OSError, TypeError, json.JSONDecodeError) as error:
                issues.append(f"{name} config is not readable JSON: {config}: {error}")
            else:
                cmdline = config_data.get("cmdline", {})
                if not isinstance(cmdline, dict):
                    issues.append(f"{name} config cmdline must be an object: {config}")
                elif not isinstance(cmdline.get("allow", []), list):
                    issues.append(
                        f"{name} config cmdline.allow must be a list: {config}"
                    )
        if not db.is_absolute():
            issues.append(f"{name} database path must be absolute: {db}")
        elif not db.parent.is_dir():
            issues.append(f"{name} database directory does not exist: {db.parent}")
    running = active_tracers()
    if running:
        issues.append(
            "another AgentSight tracer is running; stop it before the campaign: "
            + ", ".join(str(pid) for pid in running)
        )
    if mode == "formal" and not allow_identical_versions:
        baseline = data["versions"]["baseline"]
        optimized = data["versions"]["optimized"]
        if baseline["commit"] == optimized["commit"]:
            issues.append("formal baseline and optimized commits must differ")
        baseline_binary = Path(baseline["binary"])
        optimized_binary = Path(optimized["binary"])
        if (
            baseline_binary.is_file()
            and optimized_binary.is_file()
            and file_sha256(baseline_binary) == file_sha256(optimized_binary)
        ):
            issues.append("formal baseline and optimized binaries are identical")
    if results is not None:
        safety = safety_settings(data)
        existing = results
        while not existing.exists() and existing != existing.parent:
            existing = existing.parent
        free_disk = shutil.disk_usage(existing).free
        minimum_free = safety["min_free_disk_gb"] * GIB
        if free_disk < minimum_free:
            issues.append(
                "available disk space is below safety.min_free_disk_gb: "
                f"{free_disk / GIB:.1f} GiB available, "
                f"{safety['min_free_disk_gb']} GiB required"
            )
        available_memory = available_memory_bytes()
        minimum_memory = safety["min_available_memory_mb"] * MIB
        if available_memory is not None and available_memory < minimum_memory:
            issues.append(
                "available memory is below safety.min_available_memory_mb: "
                f"{available_memory / MIB:.0f} MiB available, "
                f"{safety['min_available_memory_mb']} MiB required"
            )
        if results.is_dir():
            result_bytes = allocated_size(results)
            max_result_bytes = safety["max_results_gb"] * GIB
            stop_bytes = max_result_bytes - max(max_result_bytes // 20, 256 * MIB)
            if result_bytes >= stop_bytes:
                issues.append(
                    "results directory already reached the safety stop threshold: "
                    f"{result_bytes / GIB:.1f} GiB used, "
                    f"{stop_bytes / GIB:.1f} GiB threshold"
                )
    return issues


class TracerProcess:
    """Own one foreground tracer and stop only the child it started."""

    def __init__(self, version: dict[str, Any], startup_timeout: float) -> None:
        self.version = version
        self.startup_timeout = startup_timeout
        self.process: subprocess.Popen[bytes] | None = None
        self.log: TextIO | None = None

    def __enter__(self) -> int:
        log_path = Path(self.version["log_file"])
        metrics_path = (
            Path(self.version["metrics_file"])
            if self.version.get("metrics_file")
            else None
        )
        log_path.parent.mkdir(parents=True, exist_ok=True)
        if metrics_path is not None:
            metrics_path.unlink(missing_ok=True)
        log_start = log_path.stat().st_size if log_path.exists() else 0
        self.log = log_path.open("a", encoding="utf-8")
        environment = {
            **os.environ,
            "RUST_LOG": os.environ.get("RUST_LOG", "info"),
        }
        environment.pop("AGENTSIGHT_METRICS_FILE", None)
        if metrics_path is not None:
            environment["AGENTSIGHT_METRICS_FILE"] = str(metrics_path)
        self.process = subprocess.Popen(
            [
                self.version["binary"],
                "trace",
                "--config",
                self.version["config"],
            ],
            stdout=self.log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
            env=environment,
        )
        deadline = time.monotonic() + self.startup_timeout
        while time.monotonic() < deadline:
            status = self.process.poll()
            if status is not None:
                self.stop()
                raise RuntimeError(
                    f"AgentSight exited during startup with status {status}; inspect {log_path}"
                )
            try:
                with log_path.open("rb") as reader:
                    reader.seek(log_start)
                    ready = READY_MARKER.encode() in reader.read()
            except OSError:
                ready = False
            if ready:
                print(
                    f"[READY] AgentSight pid={self.process.pid} log={log_path}",
                    flush=True,
                )
                return self.process.pid
            time.sleep(0.2)
        self.stop()
        raise RuntimeError(
            f"AgentSight did not become ready within {self.startup_timeout:g}s; inspect {log_path}"
        )

    def __exit__(self, *_: object) -> None:
        self.stop()

    def stop(self) -> None:
        """Request graceful shutdown, escalating only for this owned child."""
        process = self.process
        if process is not None and process.poll() is None:
            process.send_signal(signal.SIGINT)
            try:
                process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
        if self.log is not None and not self.log.closed:
            self.log.close()


class BenchmarkServer:
    """Keep one fully initialized TLS server alive across tracer restarts."""

    def __init__(
        self,
        load: dict[str, Any],
        results: Path,
        host: str = "127.0.0.1",
        port: int = 8443,
        startup_timeout: float = 10,
    ) -> None:
        self.load = load
        self.runtime = results / "runtime" / "mock-server"
        self.host = host
        self.port = port
        self.startup_timeout = startup_timeout
        self.process: subprocess.Popen[bytes] | None = None
        self.log: TextIO | None = None

    def __enter__(self) -> Self:
        self.runtime.mkdir(parents=True, exist_ok=True)
        certificate = self.runtime / "server.crt"
        key = self.runtime / "server.key"
        certificate_status = subprocess.run(
            [
                "openssl",
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-days",
                "1",
                "-subj",
                "/CN=localhost",
                "-keyout",
                str(key),
                "-out",
                str(certificate),
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        ).returncode
        if certificate_status != 0:
            raise RuntimeError("failed to generate the benchmark TLS certificate")
        command = [
            sys.executable,
            str(SINGLE_RUN_DIR / "mock_llm_server.py"),
            "--host",
            self.host,
            "--port",
            str(self.port),
            "--cert",
            str(certificate),
            "--key",
            str(key),
            "--chunks",
            str(self.load["chunks"]),
            "--chunk-bytes",
            str(self.load["chunk_bytes"]),
            "--request-log-dir",
            str(self.runtime / "request-logs"),
        ]
        if self.load["protocol"] == "json":
            command.append("--json")
        log_path = self.runtime / "mock-server.log"
        self.log = log_path.open("w", encoding="utf-8")
        self.process = subprocess.Popen(
            command,
            stdout=self.log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        deadline = time.monotonic() + self.startup_timeout
        while time.monotonic() < deadline:
            status = self.process.poll()
            if status is not None:
                self.stop()
                raise RuntimeError(
                    "benchmark server exited during startup with status "
                    f"{status}; inspect {log_path}"
                )
            health = subprocess.run(
                [
                    "curl",
                    "-ksf",
                    "--max-time",
                    "1",
                    f"https://{self.host}:{self.port}/healthz",
                ],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
            ).returncode
            if health == 0:
                print(
                    f"[READY] benchmark server pid={self.process.pid} log={log_path}",
                    flush=True,
                )
                return self
            time.sleep(0.1)
        self.stop()
        raise RuntimeError(
            "benchmark server did not become ready within "
            f"{self.startup_timeout:g}s; inspect {log_path}"
        )

    def __exit__(self, *_: object) -> None:
        self.stop()

    def stop(self) -> None:
        """Stop only the server child owned by this runner."""
        process = self.process
        if process is not None and process.poll() is None:
            process.send_signal(signal.SIGINT)
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
        if self.log is not None and not self.log.closed:
            self.log.close()


def load_state(path: Path, mode: str) -> dict[str, Any]:
    """Load a compatible checkpoint or initialize one."""
    if not path.exists():
        return {"schema_version": 1, "mode": mode, "completed_stages": []}
    state = read_json(path)
    if state.get("schema_version") != 1 or state.get("mode") != mode:
        raise ValueError(f"incompatible runner checkpoint: {path}")
    if not isinstance(state.get("completed_stages"), list):
        raise TypeError(f"invalid runner checkpoint: {path}")
    return state


def execute_stages(
    data: dict[str, Any],
    results: Path,
    mode: str,
    resume: bool,
    startup_timeout: float,
    storage_root: Path | None = None,
) -> None:
    """Run resumable stages, restarting capacity probes to clear in-memory backlog."""
    state_path = results / "runner-state.json"
    state = load_state(state_path, mode)
    completed = {str(item) for item in state["completed_stages"]}
    stages = QUICK_STAGES if mode == "quick" else FORMAL_STAGES
    for version, scenario in stages:
        stage = f"{version}:{scenario}"
        if stage in completed:
            print(f"[SKIP] completed stage: {stage}", flush=True)
            continue
        print(f"[RUN] {stage}", flush=True)
        storage = (
            mounted_storage(storage_root / version)
            if storage_root is not None
            else nullcontext()
        )
        with storage:
            if scenario == "capacity":
                campaign.capacity(
                    data,
                    results,
                    version,
                    None,
                    resume,
                    lambda version_config=data["versions"][version]: TracerProcess(
                        version_config, startup_timeout
                    ),
                )
            else:
                stage_data = data
                if scenario == "metrics_off":
                    stage_data = copy.deepcopy(data)
                    stage_version = stage_data["versions"][version]
                    stage_version.pop("metrics_file", None)
                    stage_version["log_file"] = str(
                        results / "runtime" / version / "agentsight-metrics-off.log"
                    )
                with TracerProcess(
                    stage_data["versions"][version], startup_timeout
                ) as pid:
                    if scenario == "metrics_off":
                        settings = stage_data["smoke"]
                        campaign.run_once(
                            stage_data,
                            results,
                            version,
                            scenario,
                            f"qps-{settings['qps']}",
                            settings["qps"],
                            settings["duration_seconds"],
                            0,
                            1,
                            pid,
                            resume=resume,
                        )
                    else:
                        campaign.run_scenario(
                            stage_data, results, version, scenario, pid, resume
                        )
        completed.add(stage)
        state["completed_stages"] = sorted(completed)
        state["updated_at_unix"] = time.time()
        write_json(state_path, state)
        print(f"[OK] {stage}", flush=True)


def run_command(label: str, command: list[str], cwd: Path) -> None:
    """Run a final evidence command and expose failures immediately."""
    print(f"[RUN] {label}", flush=True)
    status = subprocess.run(command, cwd=cwd, check=False).returncode
    if status != 0:
        raise RuntimeError(f"{label} failed with status {status}")
    print(f"[OK] {label}", flush=True)


def quick_value(summary: dict[str, Any], path: tuple[str, ...]) -> float | None:
    """Read a numeric summary field without treating missing data as zero."""
    value: Any = summary
    for key in path:
        if not isinstance(value, dict):
            return None
        value = value.get(key)
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return float(value)
    return None


def quick_number(value: float | None, suffix: str = "") -> str:
    """Format one human-readable quick comparison value."""
    return "—" if value is None else f"{value:.3f}{suffix}"


def write_quick_report(results: Path) -> tuple[Path, str]:
    """Summarize smoke validity and the optimized binary's metrics overhead."""
    expected = {
        ("baseline", "smoke"): True,
        ("optimized", "metrics_off"): False,
        ("optimized", "smoke"): True,
    }
    raw_runs: dict[tuple[str, str], dict[str, Any]] = {}
    issues: list[str] = []
    for scenario in ("smoke", "metrics_off"):
        for path in sorted(results.glob(f"runs/{scenario}/**/run-result.json")):
            run = read_json(path)
            key = (run.get("version"), scenario)
            if key not in expected or key in raw_runs:
                issues.append(f"unexpected or duplicate quick run: {path}")
                continue
            raw_runs[key] = run

    runs = []
    for (version, scenario), metrics_enabled in expected.items():
        run = raw_runs.get((version, scenario))
        if run is None:
            issues.append(f"missing quick run: {version}:{scenario}")
            continue
        if run.get("metrics_enabled") is not metrics_enabled:
            issues.append(f"metrics mode was not recorded correctly: {version}:{scenario}")
        summary = run.get("summary", {})
        runs.append(
            {
                "version": version,
                "scenario": scenario,
                "metrics_enabled": metrics_enabled,
                "verdict": run.get("evaluation", {}).get("verdict"),
                "http_success_rate": summary.get("http_success_rate"),
                "trace_completeness": summary.get("trace_completeness"),
                "capture_loss_rate": summary.get("capture_loss_rate"),
                "process_survived": summary.get("process_survived"),
            }
        )

    snapshot = results / "runtime" / "optimized" / "internal-metrics.txt"
    try:
        snapshot_available = (
            "agentsight_stage_calls_total" in snapshot.read_text(encoding="utf-8")
        )
    except (OSError, UnicodeError):
        snapshot_available = False
    if not snapshot_available:
        issues.append("optimized metrics snapshot is missing stage calls")

    on = raw_runs.get(("optimized", "smoke"), {}).get("summary", {})
    off = raw_runs.get(("optimized", "metrics_off"), {}).get("summary", {})
    measurements = {}
    for name, path in QUICK_OVERHEAD_METRICS.items():
        disabled = quick_value(off, path)
        enabled = quick_value(on, path)
        if disabled is None or enabled is None:
            issues.append(f"missing observability comparison metric: {name}")
        measurements[name] = {
            "disabled": disabled,
            "enabled": enabled,
            "delta_enabled_minus_disabled": (
                enabled - disabled if enabled is not None and disabled is not None else None
            ),
            "relative_change_pct": (
                (enabled / disabled - 1) * 100
                if enabled is not None and disabled is not None and disabled != 0
                else None
            ),
        }

    failed = any(run.get("verdict") == "FAIL" for run in runs)
    if failed:
        verdict = "FAIL"
    elif issues or any(run.get("verdict") != "PASS" for run in runs):
        verdict = "INCONCLUSIVE"
    else:
        verdict = "PASS"
    report = {
        "schema_version": 1,
        "verdict": verdict,
        "runs": runs,
        "observability_overhead": {
            "version": "optimized",
            "metrics_snapshot": str(snapshot) if snapshot_available else None,
            "measurements": measurements,
            "issues": issues,
            "interpretation": "short smoke estimate; no overhead acceptance threshold",
        },
    }
    output = results / "quick-report.json"
    write_json(output, report)

    rows = []
    for name, values in measurements.items():
        rows.append(
            f"| {name} | {quick_number(values['disabled'])} | "
            f"{quick_number(values['enabled'])} | "
            f"{quick_number(values['delta_enabled_minus_disabled'])} | "
            f"{quick_number(values['relative_change_pct'], '%')} |"
        )
    markdown = [
        "# AgentSight quick observability comparison",
        "",
        f"Overall smoke verdict: **{verdict}**",
        "",
        "| Version | Mode | Smoke verdict | Trace completeness |",
        "| --- | --- | --- | ---: |",
        *[
            f"| {run['version']} | {'on' if run['metrics_enabled'] else 'off'} | "
            f"{run['verdict']} | {quick_number(run['trace_completeness'])} |"
            for run in runs
        ],
        "",
        "The optimized binary runs at the same configured QPS and payload with metrics disabled and enabled.",
        "Each mode uses one short run; changes are diagnostic estimates, not formal performance conclusions.",
        "",
        "| Metric | Metrics off | Metrics on | On - off | Relative change |",
        "| --- | ---: | ---: | ---: | ---: |",
        *rows,
        "",
        "The two runs retain their k6, procfs, SQLite validation, and AgentSight log artifacts under `runs/`.",
        f"Metrics snapshot: `{snapshot}`" if snapshot_available else "Metrics snapshot: unavailable",
    ]
    if issues:
        markdown.extend(["", "Missing evidence:", *[f"- {issue}" for issue in issues]])
    (results / "quick-report.md").write_text(
        "\n".join(markdown) + "\n", encoding="utf-8"
    )
    return output, verdict


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--campaign", type=Path, required=True, help="frozen campaign JSON"
    )
    parser.add_argument(
        "--results", type=Path, required=True, help="one directory for all artifacts"
    )
    parser.add_argument(
        "--mode",
        choices=("quick", "formal"),
        default="quick",
        help="three quick smoke runs or the complete multi-hour campaign",
    )
    parser.add_argument(
        "--resume", action="store_true", help="resume the same interrupted directory"
    )
    parser.add_argument(
        "--allow-identical-versions",
        action="store_true",
        help="allow a formal A/A calibration and disable optimization claims",
    )
    parser.add_argument(
        "--check-only",
        action="store_true",
        help="validate inputs without starting tracers",
    )
    parser.add_argument(
        "--startup-timeout",
        type=float,
        default=60,
        help="seconds to wait for each tracer to initialize",
    )
    parser.add_argument(
        "--compare-branch",
        default="origin/main",
        help="local Git ref used for formal diff coverage",
    )
    parser.add_argument(
        "--isolated-storage-root",
        type=Path,
        help="runner-owned per-version SQLite directories (requires a private mount namespace)",
    )
    args = parser.parse_args()
    if args.startup_timeout <= 0:
        parser.error("--startup-timeout must be positive")
    if args.allow_identical_versions and args.mode != "formal":
        parser.error("--allow-identical-versions requires --mode formal")
    campaign_path = args.campaign.resolve()
    results = args.results.resolve()
    if results.exists() and not results.is_dir():
        raise SystemExit(f"results path is not a directory: {results}")
    if results.exists() and any(results.iterdir()) and not args.resume:
        raise SystemExit(f"results directory is not empty; use --resume: {results}")
    try:
        raw_data = read_json(campaign_path)
    except (OSError, TypeError, json.JSONDecodeError) as error:
        raise SystemExit(
            f"cannot read campaign JSON {campaign_path}: {error}"
        ) from error
    if raw_data.get("comparison_mode") == "aa_calibration" and not (
        args.allow_identical_versions
    ):
        parser.error("an A/A calibration campaign requires --allow-identical-versions")
    issues = preflight_issues(
        raw_data,
        args.mode,
        args.allow_identical_versions,
        results=results,
    )
    if issues:
        print("Preflight failed:", file=sys.stderr)
        for issue in issues:
            print(f"  - {issue}", file=sys.stderr)
        return 2
    print("[OK] preflight", flush=True)
    safety = safety_settings(raw_data)
    print(
        "[SAFETY] "
        f"results<={safety['max_results_gb']} GiB, "
        f"free-disk>={safety['min_free_disk_gb']} GiB, "
        f"available-memory>={safety['min_available_memory_mb']} MiB, "
        f"AgentSight-RSS<={safety['max_agentsight_rss_mb']} MiB, "
        f"k6-VUs<={safety['max_k6_vus']}",
        flush=True,
    )
    if args.check_only:
        return 0
    raw_data["comparison_mode"] = (
        "aa_calibration" if args.allow_identical_versions else "ab_comparison"
    )
    data = runtime_campaign(raw_data, results)
    validate_campaign(data)
    storage_root = None
    if args.isolated_storage_root is not None:
        try:
            storage_root = prepare_isolated_storage(
                data, results, args.isolated_storage_root
            )
        except (OSError, RuntimeError, ValueError) as error:
            print(f"Storage isolation failed: {error}", file=sys.stderr)
            return 2
    campaign_manifest.ensure_manifest(results, campaign_path, data)
    with BenchmarkServer(data["load"], results):
        execute_stages(
            data,
            results,
            args.mode,
            args.resume,
            args.startup_timeout,
            storage_root,
        )
    # Recompute frozen hashes after the run so a changed binary or config can
    # never be accepted as evidence from the originally frozen campaign.
    campaign_manifest.ensure_manifest(results, campaign_path, data)
    if args.mode == "quick":
        report, verdict = write_quick_report(results)
    else:
        repo_root = SCRIPT_DIR.parents[4]
        try:
            run_command(
                "full regression",
                [
                    sys.executable,
                    str(SCRIPT_DIR / "run_regression.py"),
                    "--results",
                    str(results),
                    "--compare-branch",
                    args.compare_branch,
                    "--full",
                ],
                repo_root,
            )
        except RuntimeError as error:
            # Aggregate the partial regression record so a failed gate remains
            # visible in the final report instead of aborting report creation.
            print(f"[FAIL] {error}; continuing to aggregate evidence", flush=True)
        run_command(
            "aggregate report",
            [
                sys.executable,
                str(SCRIPT_DIR / "aggregate_report.py"),
                "--campaign",
                str(campaign_path),
                "--results",
                str(results),
            ],
            SCRIPT_DIR.parents[2],
        )
        report = results / "final-report.md"
        verdict = read_json(results / "final-summary.json")["verdict"]
    print("\nCampaign complete:", flush=True)
    print(f"  results: {results}", flush=True)
    print(f"  report: {report}", flush=True)
    print(f"  verdict: {verdict}", flush=True)
    return 0 if verdict == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())

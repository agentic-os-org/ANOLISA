"""Launch isolated AgentSight resource runs and sequential Git A/B comparisons."""

from __future__ import annotations

import ctypes
import json
import os
import shutil
import signal
import socket
import sqlite3
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

from mock_llm_server import PREFLIGHT_TOTAL_TOKENS
from resource_inputs import Inputs, arguments, ensure_python
from resource_report import report
from validate_results import load_captured

HERE = Path(__file__).resolve().parent
COMPONENT = HERE.parents[2]


class Interrupted(Exception):
    """Carry the interrupt signal through cleanup to the CLI exit status."""

    def __init__(self, signum: int) -> None:
        self.signum = signum
        super().__init__(f"interrupted by signal {signum}")


def interrupted(signum: int, _: object) -> None:
    """Stop orchestration while allowing finally blocks to reclaim children."""
    raise Interrupted(signum)


def write_json(path: Path, value: Any) -> None:
    """Publish state atomically so the parent can read a worker PID safely."""
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def stop_group(process: subprocess.Popen[bytes], signum: int = signal.SIGTERM) -> None:
    """Reclaim the owned process group, including descendants of an exited leader."""
    try:
        os.killpg(process.pid, signum)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        pass
    # The leader can exit before its descendants; still sweep its private group.
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait()


def stop_worker(
    process: subprocess.Popen[bytes], directory: Path, privileged: bool, signum: int
) -> None:
    """Request cleanup even when the sudo credential timestamp has expired."""
    state = directory / "worker-pids.json"
    prefix = ["sudo", "-n", "--"] if privileged else []
    write_json(directory / "cancel.json", {"signal": signum})
    try:
        process.wait(timeout=30)
    except subprocess.TimeoutExpired:
        if state.exists():
            pids = json.loads(state.read_text())
            groups = pids.get("groups", [])
            if groups:
                subprocess.run(
                    prefix + ["kill", "-KILL", "--", *[f"-{pid}" for pid in groups]],
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    check=False,
                )
            subprocess.run(
                prefix + ["kill", "-KILL", str(pids["worker"])],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
            )
        process.wait(timeout=10)


def run_logged(
    command: list[str],
    log: Path,
    *,
    cwd: str | None = None,
    env: dict[str, str] | None = None,
    worker: bool = False,
    privileged: bool = False,
) -> int:
    """Run a command quietly while keeping cancellation and diagnostics available."""
    with log.open("wb") as handle:
        # sudo's tty-scoped credentials require the authenticated session. A
        # private process group keeps cleanup isolated without losing that terminal.
        preserve_terminal = worker and privileged
        process = subprocess.Popen(
            command,
            cwd=cwd,
            env=env,
            stdout=handle,
            stderr=subprocess.STDOUT,
            start_new_session=not preserve_terminal,
            preexec_fn=os.setpgrp if preserve_terminal else None,
        )
        try:
            status = process.wait()
            if not worker:
                stop_group(process)
            return status
        except BaseException as error:
            signal.signal(signal.SIGINT, signal.SIG_IGN)
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
            if worker:
                stop_worker(
                    process, log.parent, privileged, getattr(error, "signum", signal.SIGTERM)
                )
            else:
                stop_group(process)
            raise


def cache_directory() -> Path:
    """Keep reusable dependencies outside the measured checkout and result files."""
    return (
        Path(os.environ.get("XDG_CACHE_HOME", str(Path.home() / ".cache"))) / "agentsight-benchmark"
    )


def check_cancel(directory: Path) -> None:
    """Let the privileged worker perform its own cleanup on a parent request."""
    path = directory / "cancel.json"
    if path.exists():
        raise Interrupted(json.loads(path.read_text())["signal"])


def wait_for_clients(directory: Path, runner: subprocess.Popen[bytes]) -> dict[str, Any]:
    """Allow startup scanning only after every client has loaded and connected TLS."""
    path = directory / "clients-ready.json"
    deadline = time.monotonic() + 60
    while not path.exists():
        check_cancel(directory)
        if runner.poll() is not None:
            raise RuntimeError(f"clients failed to initialize; see {directory / 'runner.log'}")
        if time.monotonic() >= deadline:
            raise RuntimeError(
                f"TLS client initialization timed out; see {directory / 'agents.log'}"
            )
        time.sleep(0.05)
    return json.loads(path.read_text())


def wait_for_capture(
    directory: Path,
    prepared: dict[str, Any],
    runner: subprocess.Popen[bytes],
    trace: subprocess.Popen[bytes],
    timeout: float,
) -> None:
    """Require a complete, token-correct preflight call from every traced worker."""
    expected = set(prepared["request_ids"])
    deadline = time.monotonic() + timeout
    while True:
        check_cancel(directory)
        if trace.poll() is not None or runner.poll() is not None:
            raise RuntimeError(
                f"capture preparation process exited; see {directory / 'trace.log'} and {directory / 'agents.log'}"
            )
        try:
            captured = load_captured(directory / "db/genai_events.db", expected=expected)
        except sqlite3.Error:
            captured = {}
        correct = {
            request_id
            for request_id, records in captured.items()
            if any(
                status == "complete" and tokens == PREFLIGHT_TOTAL_TOKENS
                for status, tokens in records
            )
        }
        clients_done = all(
            (directory / f"agents/preflight-{index}.json").exists()
            for index in range(len(expected))
        )
        ready = correct == expected and clients_done
        status = {
            "ready": ready,
            "workers": len(expected),
            "matched": len(captured),
            "token_correct": len(correct),
            "missing_ids": sorted(expected - correct),
        }
        if ready or time.monotonic() >= deadline:
            write_json(directory / "capture-readiness.json", status)
            if not ready:
                raise RuntimeError(
                    f"TLS capture readiness timed out: {len(correct)}/{len(expected)} workers captured; "
                    f"see {directory / 'capture-readiness.json'} and {directory / 'trace.log'}"
                )
            write_json(directory / "capture-ready.json", status)
            return
        time.sleep(0.05)


def worker(job_path: Path) -> int:
    """Own tracing, the load runner, and orphaned descendants for one measurement."""
    job = json.loads(job_path.read_text())
    directory = job_path.parent
    children: list[subprocess.Popen[bytes]] = []
    state: dict[str, Any] = {"worker": os.getpid(), "groups": []}
    write_json(directory / "worker-pids.json", state)
    # Adopt grandchildren if a launcher fails, so cancellation does not leave zombies.
    if ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), "cannot enable child subreaper")
    result: dict[str, Any] = {"status": "failed", "returncode": 1, "trace_survived": False}
    configuration = job["config"]
    configuration.setdefault("storage", {})["base_path"] = str(directory / "db")
    config_path = directory / "config.json"
    write_json(config_path, configuration)
    total = next(
        line
        for line in Path("/proc/meminfo").read_text().splitlines()
        if line.startswith("MemTotal:")
    )
    result["host_memory_mib"] = int(total.split()[1]) / 1024
    result["logical_cpus"] = os.cpu_count()
    env = dict(
        os.environ,
        **job["env"],
        RUST_LOG="info",
        AGENTSIGHT_METRICS_FILE=str(directory / "internal-metrics.prom"),
    )
    trace: subprocess.Popen[bytes] | None = None
    try:
        check_cancel(directory)
        with (directory / "runner.log").open("wb") as runner_log, (directory / "trace.log").open(
            "wb"
        ) as trace_log:
            command = [
                "bash",
                job["runner"],
                *job["load_args"],
                "--prepare-capture",
                "--metrics-file",
                str(directory / "internal-metrics.prom"),
                "--db",
                str(directory / "db/genai_events.db"),
                "--output-dir",
                str(directory),
            ]
            runner = subprocess.Popen(
                command,
                env=env,
                stdout=runner_log,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            children.append(runner)
            state["groups"].append(runner.pid)
            write_json(directory / "worker-pids.json", state)
            prepared = wait_for_clients(directory, runner)
            trace = subprocess.Popen(
                [job["binary"], "trace", "--config", str(config_path)],
                env=env,
                stdout=trace_log,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            children.append(trace)
            state["groups"].append(trace.pid)
            state["trace"] = trace.pid
            write_json(directory / "worker-pids.json", state)
            deadline = time.monotonic() + job["ready_timeout"]
            ready_since: float | None = None
            while True:
                check_cancel(directory)
                if trace.poll() is not None:
                    raise RuntimeError(
                        f"AgentSight exited during startup ({trace.returncode}); see {directory / 'trace.log'}"
                    )
                if runner.poll() is not None:
                    raise RuntimeError(
                        f"clients exited during collector startup; see {directory / 'runner.log'}"
                    )
                initialized = "AgentSight initialized:" in (directory / "trace.log").read_text(
                    errors="replace"
                )
                if initialized and (directory / "db/genai_events.db").exists():
                    ready_since = ready_since or time.monotonic()
                    if (
                        directory / "internal-metrics.prom"
                    ).exists() or time.monotonic() - ready_since >= 0.5:
                        break
                if time.monotonic() >= deadline:
                    raise RuntimeError(
                        f"AgentSight initialization timed out; see {directory / 'trace.log'}"
                    )
                time.sleep(0.1)
            if json.loads(config_path.read_text()) != configuration:
                raise RuntimeError(
                    "AgentSight replaced the test configuration; select compatible versions/config"
                )
            write_json(directory / "tracer-ready.json", {"pid": trace.pid})
            result["capture_ready"] = False
            wait_for_capture(directory, prepared, runner, trace, job["ready_timeout"])
            result["capture_ready"] = True
            while runner.poll() is None:
                check_cancel(directory)
                if trace.poll() is not None:
                    raise RuntimeError(
                        f"AgentSight died during the load; see {directory / 'trace.log'}"
                    )
                time.sleep(0.1)
            result["returncode"] = runner.returncode
            result["trace_survived"] = trace.poll() is None
            result["status"] = "complete" if runner.returncode == 0 else "failed"
            if runner.returncode:
                result["error"] = (
                    f"benchmark exited {runner.returncode}; see {directory / 'runner.log'}"
                )
    except Interrupted as error:
        result.update(status="interrupted", returncode=128 + error.signum, error=str(error))
    except (OSError, RuntimeError, ValueError) as error:
        result["error"] = str(error)
    finally:
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        for child in reversed(children):
            stop_group(child, signal.SIGINT if child is trace else signal.SIGTERM)
        while True:
            try:
                pid, _ = os.waitpid(-1, 0)
                if pid == 0:
                    break
            except ChildProcessError:
                break
        write_json(directory / "worker-result.json", result)
    return result["returncode"]


def main() -> int:
    """Prepare all inputs before measuring, then report and reclaim owned worktrees."""
    args = arguments(COMPONENT)
    signal.signal(signal.SIGINT, interrupted)
    signal.signal(signal.SIGTERM, interrupted)
    if args.worker:
        return worker(args.worker)
    if sys.platform != "linux":
        raise RuntimeError("resource tracing requires Linux eBPF")
    for tool in ("git", "bash", "openssl", "curl"):
        if not shutil.which(tool):
            raise RuntimeError(f"required command is missing: {tool}")
    privileged = os.geteuid() != 0 and not args.no_sudo
    if privileged and not shutil.which("sudo"):
        raise RuntimeError("sudo is required; --no-sudo requires eBPF capabilities")
    config = json.loads(args.config.expanduser().read_text())
    if config.get("features", {}).get("sqlite_storage", {}).get("enabled", True) is False:
        raise RuntimeError("capture validation requires features.sqlite_storage.enabled=true")
    config["cmdline"] = {
        "allow": [{"rule": ["*python*", "*agents.py", "--worker"], "agent_name": "BenchmarkAgent"}],
        "deny": [],
    }
    config["https"], config["http"] = [], []
    config.setdefault("runtime_limits", {})["connection_capacity"] = args.connection_capacity
    if args.ring_buffer_mb is not None:
        config["runtime_limits"]["ring_buffer_mb"] = args.ring_buffer_mb
    if args.cpu_budget_percent is not None:
        config["runtime_limits"]["cpu_budget_percent"] = args.cpu_budget_percent
    elif config["runtime_limits"].get("cpu_budget_percent") is None:
        config["runtime_limits"]["cpu_budget_percent"] = 25
    cache = cache_directory()
    ensure_python(cache, HERE, sys.argv[1:], run_logged, require_venv=privileged)
    output = (
        (
            args.output_dir
            or HERE.parent
            / "results"
            / f"resource-{time.strftime('%Y%m%dT%H%M%SZ', time.gmtime())}-{os.getpid()}"
        )
        .expanduser()
        .resolve()
    )
    if output.exists():
        raise RuntimeError(f"output directory already exists; choose a fresh path: {output}")
    output.mkdir(parents=True)
    repo = Path(
        subprocess.check_output(
            ["git", "-C", str(COMPONENT), "rev-parse", "--show-toplevel"], text=True
        ).strip()
    )
    inputs = Inputs(repo, output)
    sources: dict[str, dict[str, Any]] = {}
    exit_code = 0
    try:
        if args.ab:
            sources["before"] = inputs.prepare("before", args.baseline or "", args.baseline_bin)
        label = "after" if args.ab else "run"
        sources[label] = inputs.prepare(label, args.candidate, args.candidate_bin)
        shutil.copytree(HERE, output / "harness", ignore=shutil.ignore_patterns("__pycache__"))
        write_json(output / "configuration.json", config)
        env = dict(
            os.environ,
            PATH=f"{Path(sys.executable).parent}:{os.environ.get('PATH', '')}",
            PYTHON=sys.executable,
            TIKTOKEN_CACHE_DIR=str(cache / "tiktoken"),
            PYTHONDONTWRITEBYTECODE="1",
        )
        (cache / "tiktoken").mkdir(parents=True, exist_ok=True)
        flags = (
            "agents",
            "tokens_per_second",
            "duration",
            "tokens_per_response",
            "payload_kb",
            "startup_delay",
            "rate_tolerance",
            "port",
            "validation_wait_seconds",
            "max_results_gb",
            "min_free_disk_gb",
            "min_available_memory_mb",
            "max_agentsight_rss_mb",
        )
        write_json(
            output / "settings.json",
            {
                **{name: getattr(args, name) for name in flags},
                "connection_capacity": args.connection_capacity,
                "ring_buffer_mb": config["runtime_limits"].get("ring_buffer_mb", 32),
                "cpu_budget_percent": config["runtime_limits"]["cpu_budget_percent"],
            },
        )
        load_args = [
            item
            for name in flags
            for item in (f"--{name.replace('_', '-')}", str(getattr(args, name)))
        ]
        if run_logged(
            [
                sys.executable,
                str(HERE / "load/agents.py"),
                "--check-only",
                "--output-dir",
                str(output / "preflight"),
                "--agents",
                str(args.agents),
                "--tokens-per-second",
                str(args.tokens_per_second),
                "--tokens-per-response",
                str(args.tokens_per_response),
            ],
            output / "preflight.log",
            env=env,
        ):
            raise RuntimeError(f"token fixture preflight failed; see {output / 'preflight.log'}")
        for label, source in sources.items():
            print(f"Preparing {label}: {source.get('ref', source.get('path'))}", flush=True)
            inputs.build(source, run_logged, cache)
        write_json(output / "inputs.json", sources)
        for label, source in sources.items():
            with socket.socket() as check:
                # Match the mock server's TIME_WAIT reuse across sequential runs.
                check.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                check.bind(("127.0.0.1", args.port))
            if privileged:
                subprocess.run(["sudo", "-v"], check=True)
            directory = output / label
            job = {
                "binary": source["binary"],
                "config": config,
                "ready_timeout": args.ready_timeout,
                "runner": str(output / "harness/run.sh"),
                "load_args": load_args,
                "env": {
                    key: env[key]
                    for key in ("PATH", "PYTHON", "TIKTOKEN_CACHE_DIR", "PYTHONDONTWRITEBYTECODE")
                },
            }
            write_json(directory / "job.json", job)
            command = (["sudo", "-n", "--"] if privileged else []) + [
                sys.executable,
                "-B",
                str(output / "harness/resource_run.py"),
                "--worker",
                str(directory / "job.json"),
            ]
            print(
                f"Running {label}: {args.agents} agents x {args.tokens_per_second:g} tok/s, {args.duration:g}s",
                flush=True,
            )
            status = run_logged(
                command, directory / "worker.log", worker=True, privileged=privileged
            )
            if status:
                exit_code = 1
                if not (directory / "worker-result.json").exists():
                    write_json(
                        directory / "worker-result.json",
                        {
                            "status": "failed",
                            "returncode": status,
                            "error": f"worker exited {status}; see {directory / 'worker.log'}",
                        },
                    )
    except (OSError, RuntimeError, ValueError, subprocess.CalledProcessError, Interrupted) as error:
        exit_code = 128 + error.signum if isinstance(error, Interrupted) else 1
        (output / "run-error.txt").write_text(str(error) + "\n")
    finally:
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        try:
            inputs.close()
        except RuntimeError as error:
            (output / "run-error.txt").write_text(str(error) + "\n")
            exit_code = exit_code or 1
        if not report(output, sources):
            exit_code = exit_code or 1
    return exit_code


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, ValueError, subprocess.CalledProcessError, Interrupted) as error:
        print(f"resource benchmark: {error}", file=sys.stderr)
        raise SystemExit(128 + error.signum if isinstance(error, Interrupted) else 1)

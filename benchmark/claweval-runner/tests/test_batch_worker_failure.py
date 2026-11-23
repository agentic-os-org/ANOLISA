# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""A crashed worker must not fold its trials into an unknown task.

When a worker raises before returning trial results (for example an
OSError while rereading its task YAML), the fallback entries recorded
by run_batch must carry the task ID submitted for that future — not a
shared "?" placeholder. Otherwise two failing workers aggregate into a
single unknown task in batch_results.json and batch_summary.json
undercounts errored tasks.
"""

import argparse
import json
import sys
import threading
import types
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).parents[1]
sys.path.insert(0, str(REPO_ROOT / "src"))

# ce_runner.parallel imports the POSIX-only `resource` module at import
# time (used only inside a preexec_fn callback that these tests never
# reach). Provide a stub so the module imports on Windows too.
try:
    import resource  # noqa: F401
except ImportError:  # pragma: no cover - Windows only
    _resource_stub = types.ModuleType("resource")
    _resource_stub.RLIMIT_AS = 9
    _resource_stub.setrlimit = lambda *a, **k: None
    sys.modules["resource"] = _resource_stub

from ce_runner import batch_runner  # noqa: E402


TASK_IDS = ("T001zh_alpha", "T002zh_beta")


def _write_task_fixtures(tasks_dir: Path) -> None:
    for task_id in TASK_IDS:
        task_dir = tasks_dir / task_id
        task_dir.mkdir()
        (task_dir / "task.yaml").write_text(
            f"task_id: {task_id}\ntask_name: {task_id}\ndifficulty: easy\n",
            encoding="utf-8",
        )


def _patch_infra(monkeypatch: pytest.MonkeyPatch, trace_dir: Path) -> None:
    """Stub every external dependency of run_batch so only the chunk
    execution machinery is exercised."""

    def _fail_worker_rereads(task_yaml, *args, **kwargs):
        # Discovery, preflight warnings, and the submit loop all run in the
        # main thread and must succeed; the worker reread inside
        # _execute_one runs in a thread pool and must raise.
        if threading.current_thread() is not threading.main_thread():
            raise OSError(f"simulated worker reread failure: {task_yaml}")
        return {
            "task_id": Path(task_yaml).parent.name,
            "task_name": Path(task_yaml).parent.name,
            "difficulty": "easy",
            "services": [],
        }

    def _fake_setup(chunk_tasks, parallel, config, sandbox_image=None):
        return {
            "task_slots": {
                tyaml: {
                    "task_id": Path(tyaml).parent.name,
                    "agent_id": "stub-agent",
                    "port_offset": 0,
                    "sandbox_host_port": 9000,
                    "sandbox_image": "stub:latest",
                    "sandbox_runner": None,
                    "sandbox_url": "http://127.0.0.1:1",
                    "task_def": None,
                }
                for tyaml in chunk_tasks
            }
        }

    monkeypatch.setattr(batch_runner, "load_task_yaml", _fail_worker_rereads)
    # Keep run_batch's emergency atexit hook from ever reaching the real
    # cleanup stack (it would touch the user's ~/.openclaw config).
    _atexit_stub = types.SimpleNamespace(register=lambda *a, **k: None,
                                         unregister=lambda *a, **k: None)
    monkeypatch.setattr(batch_runner, "atexit", _atexit_stub)
    monkeypatch.setattr(batch_runner, "check_gateway", lambda config: 9999)
    monkeypatch.setattr(batch_runner, "make_trace_dir", lambda prefix: str(trace_dir))
    monkeypatch.setattr(batch_runner, "require_valid_config", lambda *a, **k: None)
    monkeypatch.setattr(batch_runner, "ensure_user_session_persistent", lambda: None)
    monkeypatch.setattr(batch_runner, "find_missing_fixtures", lambda task_dirs: {})
    monkeypatch.setattr(batch_runner, "run_preflight_checks", lambda: (True, []))
    monkeypatch.setattr(batch_runner, "setup_parallel_workers", _fake_setup)
    monkeypatch.setattr(
        batch_runner, "start_mock_services_with_offset", lambda *a, **k: None
    )
    monkeypatch.setattr(
        batch_runner, "restart_gateway", lambda config, port: True
    )
    monkeypatch.setattr(batch_runner, "stop_gateway", lambda: None)
    monkeypatch.setattr(batch_runner, "cleanup_mock_services", lambda: None)
    monkeypatch.setattr(batch_runner, "kill_mcp_bridges", lambda *a, **k: None)
    monkeypatch.setattr(
        batch_runner, "cleanup_parallel_workers", lambda *a, **k: None
    )
    monkeypatch.setattr(
        batch_runner, "reap_orphan_agent_processes", lambda *a, **k: None
    )


def _run_batch(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> tuple[Path, Path]:
    tasks_dir = tmp_path / "tasks"
    trace_dir = tmp_path / "traces"
    trace_dir.mkdir()
    tasks_dir.mkdir()
    _write_task_fixtures(tasks_dir)
    _patch_infra(monkeypatch, trace_dir)

    monkeypatch.setenv("CE_RUNNER_SWEEP_INTERVAL", "0.05")

    args = argparse.Namespace(
        tasks_dir=str(tasks_dir),
        tag=None,
        range=None,
        filter=None,
        prefix=None,
        parallel=2,
        config=None,
        timeout=60,
        trials=2,
        sandbox_image=None,
        chunk_size=None,
        trace_prefix="test",
        skip_preflight=True,
        grade_parallel=0,
        tasks_file=None,
        tasks_string=None,
    )
    batch_runner.run_batch(
        args,
        get_judge_config=lambda cfg: {"model": "stub", "base_url": "http://127.0.0.1:1"},
        get_model_config=lambda cfg: {"model_id": "stub", "base_url": "http://127.0.0.1:1"},
        get_user_agent_config=lambda cfg: {},
        discover_tasks=lambda *a, **k: [str(tasks_dir / tid) for tid in TASK_IDS],
    )
    return (
        trace_dir / "batch_results.json",
        trace_dir / "batch_summary.json",
    )


def test_worker_crash_keeps_submitted_task_ids(monkeypatch, tmp_path) -> None:
    results_path, summary_path = _run_batch(monkeypatch, tmp_path)

    results = json.loads(results_path.read_text(encoding="utf-8"))
    by_id = {entry["task_id"]: entry for entry in results}

    assert sorted(by_id) == sorted(TASK_IDS)
    for task_id in TASK_IDS:
        trials = by_id[task_id]["trials"]
        assert len(trials) == 2
        assert all(trial.get("error") for trial in trials)
        assert all("simulated worker reread failure" in trial["error"] for trial in trials)

    summary = json.loads(summary_path.read_text(encoding="utf-8"))
    assert summary["tasks"] == 2
    assert summary["errored"] == 2


def test_two_crashed_workers_do_not_merge_into_one_unknown_task(monkeypatch, tmp_path) -> None:
    results_path, _ = _run_batch(monkeypatch, tmp_path)

    results = json.loads(results_path.read_text(encoding="utf-8"))
    unknown = [entry for entry in results if entry["task_id"] == "?"]
    assert unknown == [], "crashed workers must not be recorded under '?'"

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

"""Pipeline: session trace conversion and grading."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path

from ._common import _PYTHON, log


def archive_session_alongside_trace(session_file: str, trace_file: str) -> str:
    """Copy an openclaw session JSONL to ``<trace_dir>/sessions/`` and
    return the archive path. The archive filename mirrors the trace stem so
    trace ``<stem>.jsonl`` ↔ session ``<stem>.session.jsonl``.

    Must be called BEFORE :func:`cleanup_session`, otherwise the source is
    already deleted. Returns an empty string on failure.
    """
    if not session_file or not os.path.exists(session_file):
        return ""
    trace_dir = os.path.dirname(trace_file)
    sessions_dir = os.path.join(trace_dir, "sessions")
    try:
        os.makedirs(sessions_dir, exist_ok=True)
        stem = Path(trace_file).stem
        dst = os.path.join(sessions_dir, f"{stem}.session.jsonl")
        shutil.copy2(session_file, dst)
        return dst
    except Exception as exc:
        log(f"  [WARNING] Failed to archive session {session_file}: {exc}")
        return ""


def phase_convert(session_file: str, task_yaml: str, output: str,
                  mock_port_offset: int = 0,
                  audit_data_path: str = None) -> bool:
    """Convert openclaw session to claw-eval trace.

    *mock_port_offset* is added to every mock-service port when fetching
    audit data, so that batch-mode runs (where services are started on
    offset ports) still read the correct audit endpoint.

    *audit_data_path* (optional) points to a pre-saved audit JSON file.
    When provided, the converter reads this file instead of fetching live
    mock services, preventing cross-trial audit contamination.
    """
    cmd = [
        _PYTHON, "-m", "ce_runner.session_trace_converter",
        "--session", session_file, "--task-yaml", task_yaml, "--output", output,
    ]
    if mock_port_offset:
        cmd.extend(["--mock-port-offset", str(mock_port_offset)])
    if audit_data_path:
        cmd.extend(["--audit-data", audit_data_path])
    r = subprocess.run(cmd, capture_output=True, text=True)
    return r.returncode == 0 and os.path.exists(output)


def phase_grade(trace_file: str, task_yaml: str, judge_config: dict,
                env_snapshot_path: str = None) -> dict:
    """Grade the converted trace. Returns scores from the trace's grading_result event.

    The grader subprocess's stderr is written to
    ``<trace_dir>/grader_<stem>.err.log`` so judge failures (rate-limit,
    network, JSON parse errors, ``[judge-retry]`` traces, ...) are observable
    instead of being silently swallowed by ``capture_output=True``. The log
    file is removed when the subprocess succeeds with empty stderr to avoid
    cluttering the trace directory.
    """
    cmd = [
        _PYTHON, "-m", "ce_runner.grader_runner",
        "--trace", trace_file,
        "--task-yaml", task_yaml,
        "--judge-model", judge_config["model"],
        "--judge-base-url", judge_config["base_url"],
        "--judge-api-key", judge_config["api_key"],
    ]
    if env_snapshot_path:
        cmd.extend(["--env-snapshot", env_snapshot_path])

    err_log = os.path.join(
        os.path.dirname(trace_file),
        f"grader_{Path(trace_file).stem}.err.log",
    )
    with open(err_log, "wb") as ef:
        rc = subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=ef).returncode

    # Cleanup judgement (tightened):
    #   rc != 0                                  -> keep, [ERROR]
    #   rc == 0, empty                           -> delete (clean run)
    #   rc == 0, contains signal keywords        -> keep, [WARNING]
    #       (judge retried/recovered, or library traceback printed; want it visible)
    #   rc == 0, non-empty but only noise        -> delete
    #       (e.g. deprecation/resource warnings - no debug value)
    _SIGNAL_PATTERNS = (
        "[judge-retry]", "traceback", "error", "exception",
        "429", "timeout", "ratelimit", "rate_limit", "rate limit",
    )
    try:
        size = os.path.getsize(err_log)
        if rc != 0:
            log(f"  [ERROR] grader subprocess failed (rc={rc}); see {err_log}")
        elif size == 0:
            os.remove(err_log)
        else:
            with open(err_log, "rb") as ef:
                head = ef.read(64 * 1024).decode("utf-8", errors="replace").lower()
            if any(p in head for p in _SIGNAL_PATTERNS):
                log(f"  [WARNING] grader stderr non-empty (judge retries/warnings); "
                    f"see {err_log}")
            else:
                os.remove(err_log)
    except OSError:
        pass

    # Read scores from trace file's grading_result event (authoritative source)
    scores = {
        "completion": 0.0, "robustness": 0.0, "communication": 0.0,
        "safety": 0.0, "task_score": 0.0, "passed": False,
    }
    try:
        with open(trace_file) as f:
            for line in f:
                event = json.loads(line)
                if event.get("type") == "grading_result":
                    s = event.get("scores", {})
                    scores["completion"] = s.get("completion", 0.0)
                    scores["robustness"] = s.get("robustness", 0.0)
                    scores["communication"] = s.get("communication", 0.0)
                    scores["safety"] = s.get("safety", 0.0)
                    scores["task_score"] = event.get("task_score", 0.0)
                    scores["passed"] = event.get("passed", False)
    except Exception:
        pass
    return scores

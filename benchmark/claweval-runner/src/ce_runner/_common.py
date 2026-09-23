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

"""Shared globals and helpers for ce_runner modules.

All modules (run_task, agent, parallel, sandbox) import from here to avoid
circular import issues.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import threading
from datetime import datetime
from pathlib import Path

import yaml

# ── Proxy bypass ─────────────────────────────────────────────────────────────
# ce-runner only talks to localhost (gateway, mock services, sandbox bridge).
# If ALL_PROXY / HTTP_PROXY point to a SOCKS proxy and ``socksio`` is not
# installed, httpx raises ImportError on *every* request — silently breaking
# gateway health checks, mock-service resets, etc.
# Safest fix: strip proxy env vars at import time.
for _env_key in ("ALL_PROXY", "all_proxy", "HTTP_PROXY", "http_proxy",
                 "HTTPS_PROXY", "https_proxy"):
    os.environ.pop(_env_key, None)
for _env_key in ("NO_PROXY", "no_proxy"):
    _existing = os.environ.get(_env_key, "")
    if "127.0.0.1" not in _existing:
        os.environ[_env_key] = (
            f"{_existing},127.0.0.1,localhost,::1" if _existing
            else "127.0.0.1,localhost,::1"
        )

# ── Paths ────────────────────────────────────────────────────────────────────

# Repo root is two levels up from src/ce_runner/.
_REPO_DIR = Path(__file__).resolve().parent.parent.parent

# Use the current Python interpreter for ``python -m`` subprocess invocations.
_PYTHON = sys.executable

_SCRIPTS_DIR = str(_REPO_DIR / "scripts")

# Default agent timeout (seconds). Applied to both the CLI subprocess wall
# (proc_timeout = timeout + 60) and the multimodal HTTP API call
# (api_timeout = timeout + 60). Override via the --timeout CLI flag.
DEFAULT_AGENT_TIMEOUT_S = 600


def _default_openclaw_config() -> str:
    """Resolve openclaw config path: env var > ~/.openclaw/openclaw.json."""
    env = os.environ.get("OPENCLAW_CONFIG")
    if env:
        return env
    return str(Path.home() / ".openclaw" / "openclaw.json")


def _default_sessions_dir() -> str:
    """Resolve sessions dir: env var > ~/.openclaw/agents/main/sessions."""
    env = os.environ.get("OPENCLAW_SESSIONS_DIR")
    if env:
        return env
    return str(Path.home() / ".openclaw" / "agents" / "main" / "sessions")


OPENCLAW_CONFIG = _default_openclaw_config()
SESSIONS_DIR = _default_sessions_dir()

RESET_PATHS = {
    9100: "/gmail/reset",
    9101: "/calendar/reset",
    9102: "/todo/reset",
    9103: "/contacts/reset",
    9104: "/finance/reset",
    9105: "/notes/reset",
    9106: "/kb/reset",
    9107: "/helpdesk/reset",
    9108: "/inventory/reset",
    9109: "/rss/reset",
    9110: "/crm/reset",
    9111: "/config/reset",
    9112: "/scheduler/reset",
    9113: "/web/reset",
    9114: "/web_real/reset",
    9115: "/documents/reset",
    9116: "/ocr/reset",
}


def task_agent_id(task_id: str) -> str:
    """Return the openclaw agent ID for a claw-eval task.

    openclaw normalises agent IDs to lowercase internally, so we return
    lowercase directly to avoid duplicate entries in the config.
    """
    return f"claweval-{task_id}".lower()


def _mcp_server_key(prefix: str, task_id: str) -> str:
    """Return the MCP server key for a claw-eval task, ensuring ≤ 30 chars.

    openclaw Gateway's ``sanitizeServerName`` truncates server keys longer
    than 30 characters (TOOL_NAME_MAX_PREFIX = 30), which causes
    ``tools.deny`` wildcard patterns like ``serverKey__*`` to fail matching
    actual tool names, leading to cross-session tool leakage.

    Short task_ids use a readable prefix; long ones fall back to MD5 hash.
    """
    key = f"{prefix}{task_id}"
    if len(key) <= 30:
        return key
    # Hash fallback: prefix(≤8) + md5_hex[:remainder] = 30 chars
    import hashlib
    max_hash_len = 30 - len(prefix)
    hashed = hashlib.md5(task_id.encode()).hexdigest()[:max_hash_len]
    return f"{prefix}{hashed}"


def mock_mcp_name(task_id: str) -> str:
    """Return the mock MCP server name for a claw-eval task (≤ 30 chars)."""
    return _mcp_server_key("ce-mock-", task_id)


def sandbox_mcp_name(task_id: str) -> str:
    """Return the sandbox MCP server name for a claw-eval task (≤ 30 chars)."""
    return _mcp_server_key("ce-sb-", task_id)


def _agent_sessions_dir(agent_id: str) -> str:
    """Return the sessions directory for a given agent.

    openclaw normalises agent IDs to lowercase internally, so we must
    match that when resolving the filesystem path.
    """
    return str(Path.home() / ".openclaw" / "agents" / agent_id.lower() / "sessions")


# ── Helpers ──────────────────────────────────────────────────────────────────

# Optional file sink for ``log()`` so batch runs leave a self-contained debug
# log alongside their trace dir. Activated via ``attach_log_file(path)``.
_log_file = None  # type: ignore[var-annotated]
_log_lock = threading.Lock()


def attach_log_file(path: str) -> None:
    """Mirror every subsequent :func:`log` call to ``path`` (append mode).

    Each line gets a wall-clock timestamp prefix so log replays line up with
    the trace events. Calling this when a sink is already attached transparently
    rotates to the new file.
    """
    global _log_file
    detach_log_file()
    try:
        os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
        f = open(path, "a", buffering=1, encoding="utf-8")
        f.write(
            f"\n=== batch.log opened at "
            f"{datetime.now().isoformat(timespec='seconds')} ===\n"
        )
        _log_file = f
    except Exception as exc:
        print(f"[WARNING] attach_log_file({path}) failed: {exc}", flush=True)
        _log_file = None


def detach_log_file() -> None:
    """Stop mirroring ``log`` to file and close the current sink (if any)."""
    global _log_file
    if _log_file is None:
        return
    try:
        _log_file.write(
            f"=== batch.log closed at "
            f"{datetime.now().isoformat(timespec='seconds')} ===\n"
        )
        _log_file.close()
    except Exception:
        pass
    _log_file = None


def log(msg: str):
    ts = datetime.now().strftime("%H:%M:%S.%f")[:-3]
    print(f"[{ts}] {msg}", flush=True)
    if _log_file is not None:
        with _log_lock:
            try:
                _log_file.write(f"[{ts}] {msg}\n")
            except Exception:
                pass


def init_config_defaults():
    """Initialize openclaw config with ce-runner defaults.

    Sets contextWindow=256000, reserveTokensFloor=256000, disables heartbeat,
    and sets temperature=0. Idempotent — safe to call multiple times.
    """
    script = os.path.join(_SCRIPTS_DIR, "configure_openclaw.py")
    if not os.path.exists(script):
        log(f"[WARNING] Config init script not found: {script}")
        return
    subprocess.run([_PYTHON, script], capture_output=True)


def load_task_yaml(task_yaml: str) -> dict:
    with open(task_yaml) as f:
        return yaml.safe_load(f)


def load_config(config_path: str) -> dict:
    """Load judge/model config from yaml file."""
    if not config_path or not os.path.exists(config_path):
        return {}
    with open(config_path) as f:
        return yaml.safe_load(f) or {}


def require_valid_config(config_path: str | None,
                         judge_config: dict,
                         model_config: dict) -> None:
    """Fail-fast guard against silently running with an incomplete config.

    Both ``run_single`` and ``run_batch`` previously fell back to env vars
    and merely warned when fields were missing, which let half-populated
    configs cause downstream errors (401 from judge/model API, blanket 0.0
    scores, ...). This helper aborts early with an actionable message.
    """
    errors: list[str] = []

    if config_path and not os.path.exists(config_path):
        errors.append(f"Config file not found: {config_path}")

    if not config_path and not (
        os.environ.get("JUDGE_API_KEY") and os.environ.get("MODEL_API_KEY")
    ):
        errors.append(
            "No --config given and JUDGE_API_KEY/MODEL_API_KEY env vars are not both set"
        )

    judge_missing = [k for k, v in {
        "api_key": judge_config.get("api_key"),
        "base_url": judge_config.get("base_url"),
        "model": judge_config.get("model"),
    }.items() if not v]
    if judge_missing:
        errors.append(
            f"Judge config missing: {', '.join(judge_missing)} "
            f"(set in {config_path or 'config.yaml'} or env "
            f"JUDGE_API_KEY/JUDGE_BASE_URL/JUDGE_MODEL_ID)"
        )

    model_missing = [k for k, v in {
        "api_key": model_config.get("api_key"),
        "base_url": model_config.get("base_url"),
        "model_id": model_config.get("model_id"),
    }.items() if not v]
    if model_missing:
        errors.append(
            f"Model config missing: {', '.join(model_missing)} "
            f"(set in {config_path or 'config.yaml'} or env "
            f"MODEL_API_KEY/MODEL_BASE_URL/MODEL_ID)"
        )

    if errors:
        for e in errors:
            log(f"[ERROR] {e}")
        log("[ERROR] Refusing to run with incomplete config. "
            "Pass --config <path> with judge+model sections or export the env vars.")
        sys.exit(1)


def is_sandbox_task(task_yaml: str) -> bool:
    """Check if a task requires sandbox execution (has sandbox_files)."""
    task = load_task_yaml(task_yaml)
    return bool(task.get("sandbox_files"))


# ── Trace directory helpers ──────────────────────────────────────────────────

def make_trace_dir(prefix: str = "openclaw") -> str:
    """Return trace directory path: `<repo>/claw-eval/traces/<prefix>_YY-MM-DD-HH-MM`.

    Creates the directory if it does not exist.
    """
    from datetime import datetime

    ts = datetime.now().strftime("%y-%m-%d-%H-%M")
    trace_dir = str(_REPO_DIR / "claw-eval" / "traces" / f"{prefix}_{ts}")
    os.makedirs(trace_dir, exist_ok=True)
    return trace_dir


def atomic_write_config(config_path: str, config: dict) -> None:
    """Atomically write *config* as JSON to *config_path*.

    Uses write-to-tempfile + os.replace() so the gateway's chokidar watcher
    never observes a half-written file (which would trigger last-known-good
    recovery and clobber our changes).

    Unconditionally overwrites ``.last-good`` and ``.bak`` to keep backups
    in sync and prevent the gateway's size-drop guard from restoring stale
    data.
    """
    import shutil
    import tempfile

    config_dir = os.path.dirname(config_path)
    data = json.dumps(config, indent=2, ensure_ascii=False)

    fd, tmp_path = tempfile.mkstemp(
        prefix=".openclaw-cfg-", suffix=".tmp", dir=config_dir
    )
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(data)
            f.write("\n")
            f.flush()
            os.fsync(f.fileno())

        for suffix in (".last-good", ".bak"):
            target = config_path + suffix
            try:
                shutil.copy2(tmp_path, target)
                os.chmod(target, 0o600)
            except OSError:
                pass

        os.replace(tmp_path, config_path)
    except BaseException:
        try:
            os.unlink(tmp_path)
        except OSError:
            pass
        raise

#!/usr/bin/env python3

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

"""Setup script: configure a fresh environment for claw-eval + ce-runner.

Prerequisites:
  - Docker installed and running
  - openclaw installed and gateway configured

Usage:
    python scripts/setup_env.py              # Full setup
    python scripts/setup_env.py --check      # Check-only (no modifications)
    python scripts/setup_env.py --verbose    # Detailed debug output
    python scripts/setup_env.py --fixtures   # Only download/extract task fixtures
    python scripts/setup_env.py --skip-fixtures  # Full setup, skip fixture step
    python scripts/setup_env.py --help       # Show this help and exit

Environment variables:
  Required (for full setup; otherwise model config step is skipped with a warning):
    MODEL_API_KEY        API key shared by agent model, judge, and user-agent model.

  Optional:
    MODEL_ID             Override agent model id (default: qwen3.6-plus).
    JUDGE_MODEL_ID       Override judge model id (default: qwen3.6-plus).
                         Use this together with MODEL_ID to evaluate one model
                         while grading with a different one (base_url stays shared).
    HF_TOKEN /           Hugging Face token for gated/authenticated fixture
    HUGGING_FACE_HUB_TOKEN  download from the claw-eval/Claw-Eval dataset.

  --check mode additionally inspects MODEL_*/JUDGE_* env vars as fallbacks when
  the corresponding fields are missing in claw-eval/config.yaml.
"""

import sys

if sys.version_info < (3, 11):
    print(
        f"[setup] ERROR: Python >= 3.11 required, found {sys.version.split()[0]}\n"
        f"  Current interpreter: {sys.executable}"
    )
    sys.exit(1)

import json
import os
import re
import shutil
import subprocess
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
CLAW_EVAL_DIR = REPO_ROOT / "claw-eval"
OPENCLAW_CONFIG = Path.home() / ".openclaw" / "openclaw.json"
VENV_DIR = REPO_ROOT / ".venv"

# claw-eval upstream repo and pinned revision. In the anolisa monorepo claw-eval
# is not tracked as a git submodule, so the pin lives here explicitly to keep
# setups reproducible (previously recorded via the submodule gitlink).
CLAW_EVAL_REPO = "https://github.com/LPhgh/claw-eval.git"
CLAW_EVAL_REV = "4e3de7b4e4c85e030b71ec81a007cf46353dd2e3"

# Task fixtures (videos, etc.) are not shipped in the claw-eval git repo due to
# file-size limits; they live in the Hugging Face dataset and must be fetched
# and unpacked into claw-eval/ (the archive roots at tasks/<id>/fixtures/...).
HF_DATASET = "claw-eval/Claw-Eval"
FIXTURES_URL = (
    f"https://huggingface.co/datasets/{HF_DATASET}"
    "/resolve/main/data/fixtures.tar.gz"
)
FIXTURES_ARCHIVE = REPO_ROOT / ".cache" / "fixtures.tar.gz"
CLAW_EVAL_TASKS_DIR = CLAW_EVAL_DIR / "tasks"

# Global verbosity flag (set via --verbose CLI arg)
VERBOSE = "--verbose" in sys.argv or "-v" in sys.argv


def _venv_python() -> Path:
    """Return the python executable inside the project venv (POSIX/Windows aware)."""
    if os.name == "nt":
        return VENV_DIR / "Scripts" / "python.exe"
    return VENV_DIR / "bin" / "python"


def _pip_install_cmd() -> list[str]:
    """Return the uv pip install command targeting the project venv."""
    return ["uv", "pip", "install", "--python", str(_venv_python())]


def log(msg: str):
    print(f"[setup] {msg}")


def log_verbose(msg: str):
    """Only print when --verbose is active."""
    if VERBOSE:
        print(f"[setup][DEBUG] {msg}")


def run(cmd: list[str], check: bool = True, timeout: int = 300) -> subprocess.CompletedProcess:
    cmd_str = " ".join(cmd)
    log_verbose(f"$ {cmd_str}")
    t0 = time.time()
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        elapsed = time.time() - t0
        log(f"TIMEOUT after {elapsed:.1f}s (limit {timeout}s): {cmd_str}")
        log(f"  Hint: check network connectivity or increase timeout")
        sys.exit(1)
    except FileNotFoundError as e:
        # Re-raise so callers with try/except FileNotFoundError still work
        raise
    elapsed = time.time() - t0
    log_verbose(f"  completed in {elapsed:.1f}s (rc={result.returncode})")
    if VERBOSE and result.stdout.strip():
        # Show first few lines of stdout in verbose mode
        stdout_preview = result.stdout.strip().splitlines()[:5]
        for line in stdout_preview:
            log_verbose(f"  [stdout] {line}")
        if len(result.stdout.strip().splitlines()) > 5:
            log_verbose(f"  [stdout] ... ({len(result.stdout.strip().splitlines())} lines total)")
    if check and result.returncode != 0:
        log(f"")
        log(f"{'─' * 60}")
        log(f"COMMAND FAILED (rc={result.returncode}, {elapsed:.1f}s)")
        log(f"{'─' * 60}")
        log(f"  cmd: {cmd_str}")
        log(f"  cwd: {os.getcwd()}")
        log(f"  PATH: {os.environ.get('PATH', '(unset)')[:200]}")
        log(f"  VIRTUAL_ENV: {os.environ.get('VIRTUAL_ENV', '(unset)')}")
        if result.stderr.strip():
            log(f"  ┌── stderr ──")
            for line in result.stderr.strip().splitlines()[:30]:
                log(f"  │ {line}")
            if len(result.stderr.strip().splitlines()) > 30:
                log(f"  │ ... ({len(result.stderr.strip().splitlines())} lines total)")
            log(f"  └────────────")
        if result.stdout.strip():
            log(f"  ┌── stdout ──")
            for line in result.stdout.strip().splitlines()[:15]:
                log(f"  │ {line}")
            if len(result.stdout.strip().splitlines()) > 15:
                log(f"  │ ... ({len(result.stdout.strip().splitlines())} lines total)")
            log(f"  └────────────")
        if not result.stderr.strip() and not result.stdout.strip():
            log(f"  (no stdout/stderr output)")
        log(f"{'─' * 60}")
        sys.exit(1)
    return result


REQUIRED_OPENCLAW_VERSION = "2026.4.22"
_REQUIRED_VERSION_TUPLE = (2026, 4, 22)


def _parse_version_tuple(version_str: str) -> tuple[int, ...] | None:
    """Parse version string like '2026.4.22' into a comparable tuple."""
    try:
        return tuple(int(x) for x in version_str.split("."))
    except (ValueError, AttributeError):
        return None


def create_venv():
    """Create a virtual environment at REPO_ROOT/.venv via uv.

    After creation, set VIRTUAL_ENV / PATH so subsequent subprocesses
    (e.g. ce-runner / claw-eval CLIs) resolve binaries from the venv.
    """
    venv_py = _venv_python()
    if venv_py.exists():
        log(f"  ✅ venv exists at {VENV_DIR}")
    else:
        log(f"Creating virtual environment at {VENV_DIR}...")
        run(["uv", "venv", str(VENV_DIR)])
        if not venv_py.exists():
            log(f"  ❌ venv creation failed: {venv_py} not found")
            sys.exit(1)
        log(f"  ✅ venv created at {VENV_DIR}")

    # Make the venv "active" for downstream subprocess calls
    os.environ["VIRTUAL_ENV"] = str(VENV_DIR)
    bin_dir = VENV_DIR / ("Scripts" if os.name == "nt" else "bin")
    os.environ["PATH"] = str(bin_dir) + os.pathsep + os.environ.get("PATH", "")


def ensure_uv():
    """Ensure uv is available. Install via official standalone installer if missing.

    Fatal — exits if uv cannot be obtained. uv is the sole package manager
    used by this setup; there is no pip fallback.
    """
    try:
        result = subprocess.run(["uv", "--version"], capture_output=True, text=True)
        if result.returncode == 0:
            log(f"  ✅ uv detected: {result.stdout.strip()}")
            return
    except FileNotFoundError:
        pass

    log("  uv not found, installing via official installer...")
    install = run(
        ["sh", "-c", "curl -LsSf https://astral.sh/uv/install.sh | sh"],
        check=False, timeout=120,
    )
    if install.returncode != 0:
        log("ERROR: uv installer failed")
        log("  Try manually: curl -LsSf https://astral.sh/uv/install.sh | sh")
        sys.exit(1)

    # The installer places the binary in ~/.local/bin (or ~/.cargo/bin on some systems)
    for candidate in [Path.home() / ".local" / "bin", Path.home() / ".cargo" / "bin"]:
        if (candidate / "uv").exists():
            os.environ["PATH"] = str(candidate) + os.pathsep + os.environ.get("PATH", "")
            break

    if not shutil.which("uv"):
        log("ERROR: uv installed but not found on PATH")
        log("  Add ~/.local/bin to your PATH and re-run")
        sys.exit(1)

    result = subprocess.run(["uv", "--version"], capture_output=True, text=True)
    log(f"  ✅ uv installed: {result.stdout.strip()}")


def check_prerequisites() -> tuple[list[str], list[str]]:
    """Check that required tools are installed and meet version requirements.

    Returns:
        (errors, warnings) — errors are fatal, warnings are informational.
    """
    errors: list[str] = []
    warnings: list[str] = []

    # Python version (redundant with top-level guard, but reports cleanly)
    py_ver = sys.version_info
    if py_ver < (3, 11):
        errors.append(f"Python >= 3.11 required, found {py_ver.major}.{py_ver.minor}.{py_ver.micro}")

    # git: required for submodule init
    try:
        result = run(["git", "--version"], check=False)
        if result.returncode != 0:
            errors.append("git installed but not working")
        else:
            log_verbose(f"  git: {result.stdout.strip()}")
    except FileNotFoundError:
        errors.append("git not installed (required for submodule init)")

    # curl: required for uv standalone installer
    try:
        result = run(["curl", "--version"], check=False)
        if result.returncode != 0:
            errors.append("curl installed but not working")
        else:
            log_verbose(f"  curl: {result.stdout.strip().splitlines()[0]}")
    except FileNotFoundError:
        errors.append("curl not installed (required for uv installer)")

    # Docker: must be installed and daemon running
    try:
        result = run(["docker", "info"], check=False)
        if result.returncode != 0:
            errors.append("Docker daemon is not running (run `docker info` to verify)")
    except FileNotFoundError:
        errors.append("docker not installed")

    # Node.js / npm: required for mcporter and openclaw
    try:
        result = run(["node", "--version"], check=False)
        if result.returncode != 0:
            errors.append("node installed but not working")
        else:
            node_ver = result.stdout.strip()
            log_verbose(f"  node: {node_ver}")
            # Extract major version from vXX.Y.Z
            match = re.match(r"v?(\d+)", node_ver)
            if match and int(match.group(1)) < 18:
                errors.append(f"Node.js >= 18 required, found {node_ver}")
    except FileNotFoundError:
        errors.append("node (Node.js) not installed (required for openclaw and mcporter)")

    try:
        result = run(["npm", "--version"], check=False)
        if result.returncode != 0:
            errors.append("npm installed but not working (required for mcporter)")
        else:
            log_verbose(f"  npm: {result.stdout.strip()}")
    except FileNotFoundError:
        errors.append("npm not found (required for mcporter installation)")

    # openclaw: version must be >= 2026.4.22
    try:
        result = run(["openclaw", "--version"], check=False)
        version = result.stdout.strip().split("\n")[0].strip()
        # Extract version number from strings like "OpenClaw 2026.4.22 (00bd2cf)"
        match = re.search(r"(\d+\.\d+\.\d+)", version)
        if not match:
            errors.append(f"openclaw: cannot parse version {version!r}")
        else:
            version_clean = match.group(1)
            version_tuple = _parse_version_tuple(version_clean)
            if version_tuple is None:
                errors.append(f"openclaw: cannot parse version {version!r}")
            elif version_tuple < _REQUIRED_VERSION_TUPLE:
                errors.append(
                    f"openclaw version too old: found {version!r}, "
                    f"require >= {REQUIRED_OPENCLAW_VERSION}"
                )
            elif version_tuple > _REQUIRED_VERSION_TUPLE:
                warnings.append(
                    f"openclaw {version_clean}: newer than tested {REQUIRED_OPENCLAW_VERSION}, "
                    f"compatibility not guaranteed"
                )
    except FileNotFoundError:
        errors.append("openclaw not installed")

    return errors, warnings


def init_submodules():
    """Ensure claw-eval sources are present at the pinned revision.

    The monorepo does not register claw-eval as a git submodule, so clone the
    upstream repo and check out the pinned commit instead of running
    `git submodule update`. Idempotent: skips when already populated.
    """
    if (CLAW_EVAL_DIR / "pyproject.toml").exists():
        return
    log(f"Cloning claw-eval into {CLAW_EVAL_DIR}...")
    run(["git", "clone", CLAW_EVAL_REPO, str(CLAW_EVAL_DIR)])
    log(f"Checking out pinned claw-eval revision {CLAW_EVAL_REV[:10]}...")
    run(["git", "-C", str(CLAW_EVAL_DIR), "checkout", CLAW_EVAL_REV])


def setup_python_deps():
    """Install all Python dependencies for ce-runner and claw-eval (full)."""
    pip_cmd = _pip_install_cmd()

    log("Installing ce-runner (with dev dependencies)...")
    run(pip_cmd + ["-e", str(REPO_ROOT) + "[dev]", "-q"])

    # Install claw-eval with all extras
    claw_eval_pyproject = CLAW_EVAL_DIR / "pyproject.toml"
    if claw_eval_pyproject.exists():
        log("Installing claw-eval (mock, sandbox)...")
        run(pip_cmd + ["-e", str(CLAW_EVAL_DIR) + "[mock,sandbox]", "-q"])

    # Belt-and-braces: also apply claw-eval/requirements.txt so any drift
    # between pyproject.toml and the requirements file is caught here.
    # Note: if requirements.txt itself is missing entries (e.g. trafilatura,
    # requests), fix it in the claw-eval submodule.
    main_reqs = CLAW_EVAL_DIR / "requirements.txt"
    if main_reqs.exists():
        log("Applying claw-eval/requirements.txt (fallback)...")
        run(pip_cmd + ["-r", str(main_reqs), "-q"])

    # Sandbox server dependencies (Pillow, pdf2image, opencv, etc.)
    sandbox_reqs = CLAW_EVAL_DIR / "requirements-sandbox-server.txt"
    if sandbox_reqs.exists():
        log("Installing sandbox server dependencies...")
        run(pip_cmd + ["-r", str(sandbox_reqs), "-q"])

    # Standalone venv inside claw-eval/ so `uv run` / `claw-eval` invoked
    # from that directory work out of the box without contaminating the
    # ce-runner venv with `uv sync`.
    if claw_eval_pyproject.exists():
        _setup_claw_eval_venv(main_reqs, sandbox_reqs)


def _setup_claw_eval_venv(main_reqs: Path, sandbox_reqs: Path) -> None:
    """Create claw-eval/.venv with claw-eval[mock,sandbox] installed.

    Independent from REPO_ROOT/.venv so running `uv sync` or `uv run` inside
    claw-eval/ never tears down ce-runner's environment.
    """
    venv_dir = CLAW_EVAL_DIR / ".venv"
    venv_py = venv_dir / ("Scripts" if os.name == "nt" else "bin") / (
        "python.exe" if os.name == "nt" else "python"
    )

    if not venv_py.exists():
        log(f"Creating claw-eval venv at {venv_dir}...")
        run(["uv", "venv", str(venv_dir)])
    else:
        log(f"  ✅ claw-eval venv exists at {venv_dir}")

    claw_pip = ["uv", "pip", "install", "--python", str(venv_py)]
    log("Installing claw-eval[mock,sandbox] into claw-eval/.venv...")
    run(claw_pip + ["-e", str(CLAW_EVAL_DIR) + "[mock,sandbox]", "-q"])

    if main_reqs.exists():
        run(claw_pip + ["-r", str(main_reqs), "-q"])
    if sandbox_reqs.exists():
        run(claw_pip + ["-r", str(sandbox_reqs), "-q"])


def _gateway_is_running() -> bool:
    """Check if openclaw gateway is currently running."""
    try:
        status = run(["openclaw", "gateway", "status"], check=False)
    except FileNotFoundError:
        return False
    combined = (status.stdout + "\n" + status.stderr).lower()
    return ("runtime: running" in combined) or ("runtime running" in combined)


def _start_gateway() -> bool:
    """Attempt to install (if needed) and start the gateway. Returns True if running."""
    try:
        status = run(["openclaw", "gateway", "status"], check=False)
    except FileNotFoundError:
        return False

    combined = (status.stdout + "\n" + status.stderr).lower()

    # Install if service missing/disabled
    install_markers = (
        "not installed", "no service", "service not found",
        "could not find service", "(disabled)", "service disabled",
    )
    if any(m in combined for m in install_markers):
        log("  Gateway service missing/disabled, running 'openclaw gateway install'...")
        install = run(["openclaw", "gateway", "install"], check=False)
        if install.returncode != 0:
            return False
        log("  ✅ gateway service installed")

    # Already running?
    if _gateway_is_running():
        return True

    # Start
    start = run(["openclaw", "gateway", "start"], check=False)
    if start.returncode != 0:
        return False

    return _gateway_is_running()


def setup_openclaw_config():
    """Configure openclaw settings for ce-runner.

    Verifies gateway health BEFORE modifying config (fatal if already broken),
    then applies config changes, then re-verifies gateway (diagnoses config issue
    if it broke).
    """
    if not OPENCLAW_CONFIG.exists():
        log(f"ERROR: openclaw config not found at {OPENCLAW_CONFIG}")
        log("  Run 'openclaw' once to initialize, then re-run this script.")
        sys.exit(1)

    # Pre-config health check: gateway must be startable with current config
    log("Verifying gateway health before config changes...")
    if not _start_gateway():
        log("ERROR: openclaw gateway cannot start with current (unmodified) config")
        log("  Fix your openclaw installation before running setup_env.")
        log("  Try: openclaw gateway status / openclaw gateway install")
        sys.exit(1)
    log("  ✅ gateway healthy (pre-config)")

    # Apply config changes
    log("Configuring openclaw settings...")
    configure_script = REPO_ROOT / "scripts" / "configure_openclaw.py"
    if configure_script.exists():
        run([sys.executable, str(configure_script)])
    else:
        log(f"WARNING: {configure_script} not found, skipping openclaw config")
        return

    # Post-config health check: restart gateway with new config
    log("Restarting gateway to verify new config...")
    run(["openclaw", "gateway", "restart"], check=False)

    if _gateway_is_running():
        log("  ✅ gateway healthy (post-config)")
    else:
        log("ERROR: gateway failed to start after config changes")
        log(f"  Config file: {OPENCLAW_CONFIG}")
        log("  The config modification likely introduced an error.")
        log("  Inspect with: openclaw gateway status")
        sys.exit(1)


def ensure_openclaw_gateway():
    """Final gateway check — ensure it's running before proceeding."""
    log("Checking openclaw gateway status...")
    if _gateway_is_running():
        log("  ✅ openclaw gateway running")
        return

    if _start_gateway():
        log("  ✅ openclaw gateway started")
    else:
        log("ERROR: openclaw gateway cannot start")
        log("  Try: openclaw gateway status")
        sys.exit(1)


def setup_model_config():
    """Configure model settings for claw-eval (non-interactive, uses env var)."""
    api_key = os.environ.get("MODEL_API_KEY")
    if not api_key:
        print()
        log("╔" + "═" * 58 + "╗")
        log("║" + " ⚠️  MODEL_API_KEY not set".ljust(58) + "║")
        log("║" + "".ljust(58) + "║")
        log("║" + " Model configuration skipped.".ljust(58) + "║")
        log("║" + " Run with: MODEL_API_KEY=sk-xxx python scripts/setup_env.py".ljust(58) + "║")
        log("╚" + "═" * 58 + "╝")
        print()
        return

    configure_script = REPO_ROOT / "scripts" / "configure_model.py"
    if not configure_script.exists():
        log("WARNING: configure_model.py not found, skipping")
        return

    cmd = [str(_venv_python()), str(configure_script), "--api-key", api_key]
    if model_id := os.environ.get("MODEL_ID"):
        cmd += ["--model-id", model_id]
    if judge_model_id := os.environ.get("JUDGE_MODEL_ID"):
        cmd += ["--judge-model-id", judge_model_id]

    log("Configuring model settings...")
    run(cmd)


def setup_mcporter():
    """Install mcporter CLI for MCP tool access."""
    log("Installing mcporter...")
    if not shutil.which("npm"):
        log("  ⚠️  npm not found, skipping mcporter (warned in prerequisites)")
        return

    result = run(["npm", "install", "-g", "mcporter"], check=False)
    if result.returncode != 0:
        log("ERROR: mcporter installation failed (required for MCP tool dispatch)")
        log("  Try manually: npm install -g mcporter")
        sys.exit(1)

    ver = run(["mcporter", "--version"], check=False)
    if ver.returncode != 0:
        log("ERROR: mcporter installed but not functional")
        sys.exit(1)
    log(f"  ✅ mcporter installed: {ver.stdout.strip()}")


def patch_dockerfile_remove_tuna_mirror():
    """Remove Tsinghua (TUNA) PyPI mirror lines from claw-eval/Dockerfile.agent.

    Backs up the original to Dockerfile.agent.bak (only if backup is absent),
    then strips any '-i https://pypi.tuna.tsinghua.edu.cn/simple' and matching
    '--trusted-host pypi.tuna.tsinghua.edu.cn' lines so pip falls back to the
    official PyPI index inside the sandbox image build.
    """
    dockerfile = CLAW_EVAL_DIR / "Dockerfile.agent"
    if not dockerfile.exists():
        log("  Dockerfile.agent not found, skipping TUNA mirror patch")
        return

    backup = dockerfile.with_suffix(dockerfile.suffix + ".bak")
    if not backup.exists():
        shutil.copy2(dockerfile, backup)
        log(f"  Backed up original Dockerfile.agent to {backup.name}")
    else:
        log(f"  Backup already exists at {backup.name}, leaving untouched")

    original = dockerfile.read_text(encoding="utf-8")
    new_lines: list[str] = []
    removed = 0
    for line in original.splitlines(keepends=True):
        stripped = line.strip().rstrip("\\").strip()
        if stripped.startswith("-i https://pypi.tuna.tsinghua.edu.cn"):
            removed += 1
            continue
        if stripped.startswith("--trusted-host pypi.tuna.tsinghua.edu.cn"):
            removed += 1
            continue
        new_lines.append(line)
    new_content = "".join(new_lines).replace(" (PyPI via TUNA mirror)", "")

    if new_content == original:
        log("  No TUNA mirror lines found; Dockerfile.agent unchanged")
        return

    dockerfile.write_text(new_content, encoding="utf-8")
    log(f"  ✅ Removed {removed} TUNA mirror line(s) from Dockerfile.agent")


def setup_sandbox_image():
    """Build Docker sandbox image if Dockerfile exists."""
    dockerfile = CLAW_EVAL_DIR / "Dockerfile.agent"
    if not dockerfile.exists():
        log("WARNING: Dockerfile.agent not found, skipping sandbox image build")
        return

    log("Building sandbox Docker image...")
    run(["docker", "build", "-f", str(dockerfile), "-t", "claw-eval-agent:latest",
         str(CLAW_EVAL_DIR)], timeout=900)
    log("  Image built: claw-eval-agent:latest")


def _scan_missing_fixtures() -> dict[str, list[str]] | None:
    """Return {task_yaml: [missing_rel_paths]} across all claw-eval tasks.

    Runs the scan in a ``uv run`` subprocess so that ce_runner's third-party
    dependencies (yaml, httpx, …) are resolved from the project venv — even
    when setup_env.py itself was invoked with a bare system Python that lacks
    those packages.

    Returns ``None`` when the scan cannot be performed (e.g. uv not available,
    venv not ready yet), signalling the caller to fall back to a conservative
    "assume missing" strategy.
    """
    if not CLAW_EVAL_TASKS_DIR.exists():
        return {}

    scan_script = (
        "import json, os, yaml\n"
        "from pathlib import Path\n"
        "tasks_dir = Path({tasks!r})\n"
        "missing = {{}}\n"
        "for tyaml in tasks_dir.glob('*/task.yaml'):\n"
        "    try:\n"
        "        ty = yaml.safe_load(tyaml.read_text())\n"
        "    except Exception:\n"
        "        continue\n"
        "    file_list = ty.get('sandbox_files') or []\n"
        "    if not file_list:\n"
        "        env = ty.get('environment') or {{}}\n"
        "        file_list = env.get('fixtures') or []\n"
        "    if not file_list:\n"
        "        continue\n"
        "    task_root = tyaml.parent\n"
        "    not_found = []\n"
        "    for rel in file_list:\n"
        "        if (task_root / rel).exists():\n"
        "            continue\n"
        "        if (tasks_dir.parent / rel).exists():\n"
        "            continue\n"
        "        not_found.append(rel)\n"
        "    if not_found:\n"
        "        missing[str(tyaml)] = not_found\n"
        "print(json.dumps(missing))\n"
    ).format(tasks=str(CLAW_EVAL_TASKS_DIR))

    venv_py = _venv_python()
    if not venv_py.exists():
        log_verbose("venv python not found; cannot scan fixtures")
        return None

    try:
        result = subprocess.run(
            [str(venv_py), "-c", scan_script],
            capture_output=True, text=True, timeout=60,
            cwd=str(REPO_ROOT),
        )
    except (FileNotFoundError, subprocess.TimeoutExpired):
        log_verbose("uv subprocess unavailable; cannot scan fixtures")
        return None

    if result.returncode != 0:
        log_verbose(f"fixture scan subprocess failed (rc={result.returncode}): "
                    f"{result.stderr.strip()[:200]}")
        return None

    import json as _json
    try:
        return _json.loads(result.stdout)
    except (ValueError, TypeError):
        log_verbose(f"fixture scan produced invalid JSON: {result.stdout[:200]}")
        return None


def _download_fixtures_archive() -> bool:
    """Stream-download the fixtures archive from Hugging Face with resume.

    Writes to a ``.part`` temp file and atomically renames on success. Skips
    the download when the final archive already exists and is non-empty.
    Honours an ``HF_TOKEN`` env var for authenticated/gated access.

    The actual download runs in a venv subprocess (requires httpx), so
    setup_env.py itself does not depend on third-party packages at import time.
    """
    if FIXTURES_ARCHIVE.exists() and FIXTURES_ARCHIVE.stat().st_size > 0:
        log(f"  ✅ archive already present: {FIXTURES_ARCHIVE} "
            f"({FIXTURES_ARCHIVE.stat().st_size / 1e9:.2f} GB)")
        return True

    FIXTURES_ARCHIVE.parent.mkdir(parents=True, exist_ok=True)

    venv_py = _venv_python()
    if not venv_py.exists():
        log("  ❌ venv python not found; cannot download fixtures")
        return False

    download_script = _FIXTURE_DOWNLOAD_SCRIPT.format(
        url=FIXTURES_URL,
        archive=str(FIXTURES_ARCHIVE),
        hf_dataset=HF_DATASET,
    )

    env = os.environ.copy()
    result = subprocess.run(
        [str(venv_py), "-c", download_script],
        text=True, timeout=7200, cwd=str(REPO_ROOT), env=env,
    )
    return result.returncode == 0


_FIXTURE_DOWNLOAD_SCRIPT = '''\
import os, sys
import httpx
from pathlib import Path

url = {url!r}
archive = Path({archive!r})
hf_dataset = {hf_dataset!r}

part = archive.with_suffix(archive.suffix + ".part")
resume_from = part.stat().st_size if part.exists() else 0

headers = {{}}
token = os.environ.get("HF_TOKEN") or os.environ.get("HUGGING_FACE_HUB_TOKEN")
if token:
    headers["Authorization"] = f"Bearer {{token}}"
if resume_from:
    headers["Range"] = f"bytes={{resume_from}}-"
    print(f"  Resuming download from {{resume_from / 1e6:.1f}} MB...", flush=True)
else:
    print(f"  Downloading fixtures archive (~2.7 GB) from {{hf_dataset}}...", flush=True)

mode = "ab" if resume_from else "wb"
try:
    with httpx.stream("GET", url, headers=headers,
                      follow_redirects=True, timeout=60.0) as resp:
        if resp.status_code == 416:
            pass  # .part is already complete
        elif resp.status_code not in (200, 206):
            print(f"  \\u274c download failed: HTTP {{resp.status_code}}", flush=True)
            print(f"     URL: {{url}}", flush=True)
            if resp.status_code in (401, 403):
                print("     Hint: the dataset may be gated \\u2014 set HF_TOKEN and "
                      f"accept the terms at "
                      f"https://huggingface.co/datasets/{{hf_dataset}}", flush=True)
            sys.exit(1)

        total = int(resp.headers.get("content-length", 0)) + resume_from
        downloaded = resume_from
        next_mark = 5
        with open(part, mode) as f:
            for chunk in resp.iter_bytes(chunk_size=1 << 20):
                f.write(chunk)
                downloaded += len(chunk)
                if total:
                    pct = downloaded * 100 // total
                    if pct >= next_mark:
                        print(f"    {{pct:3d}}%  ({{downloaded / 1e9:.2f}}/"
                              f"{{total / 1e9:.2f}} GB)", flush=True)
                        next_mark = pct - (pct % 5) + 5
except httpx.HTTPError as e:
    print(f"  \\u274c download error: {{e}}", flush=True)
    print("     Partial file kept for resume; re-run to continue.", flush=True)
    sys.exit(1)

part.replace(archive)
print(f"  \\u2705 downloaded: {{archive}} "
      f"({{archive.stat().st_size / 1e9:.2f}} GB)", flush=True)
'''


def _is_within_directory(directory: Path, target: Path) -> bool:
    """Return True if *target* resolves to a path inside *directory*."""
    try:
        target.resolve().relative_to(directory.resolve())
        return True
    except ValueError:
        return False


def _extract_fixtures_archive() -> bool:
    """Safely extract the fixtures archive into claw-eval/tasks/.

    Archive members are rooted at ``<task_id>/fixtures/...`` (e.g.
    ``M001_clock/fixtures/config.json``), so extracting into the tasks
    directory yields the ``tasks/<task_id>/fixtures/...`` layout that
    ``task.yaml`` references.

    Guards against path traversal: rejects members with absolute paths, ``..``
    components, or any resolved destination escaping CLAW_EVAL_TASKS_DIR.
    """
    import tarfile

    if not FIXTURES_ARCHIVE.exists():
        log(f"  ❌ archive not found: {FIXTURES_ARCHIVE}")
        return False

    log(f"  Extracting into {CLAW_EVAL_TASKS_DIR}...")
    CLAW_EVAL_TASKS_DIR.mkdir(parents=True, exist_ok=True)
    try:
        with tarfile.open(FIXTURES_ARCHIVE, "r:gz") as tar:
            safe_members = []
            for member in tar.getmembers():
                name = member.name
                if name.startswith("/") or ".." in Path(name).parts:
                    log(f"  ⚠️  skipping unsafe archive member: {name}")
                    continue
                dest = CLAW_EVAL_TASKS_DIR / name
                if not _is_within_directory(CLAW_EVAL_TASKS_DIR, dest):
                    log(f"  ⚠️  skipping out-of-tree archive member: {name}")
                    continue
                safe_members.append(member)
            tar.extractall(path=CLAW_EVAL_TASKS_DIR, members=safe_members)
    except (tarfile.TarError, OSError) as e:
        log(f"  ❌ extraction failed: {e}")
        return False

    log("  ✅ extraction complete")
    return True


def prepare_fixtures(force: bool = False) -> None:
    """Download + extract task fixtures when any are missing (or when forced).

    Non-fatal: logs a warning if some fixtures remain missing afterwards, since
    the archive may not cover every declared file and many tasks have none.
    """
    missing_before = _scan_missing_fixtures()

    if missing_before is None:
        log("  ⚠️  fixture scan unavailable; attempting download as precaution...")
    elif not missing_before and not force:
        log("  ✅ all declared task fixtures present; nothing to download")
        return
    elif missing_before:
        n_tasks = len(missing_before)
        n_files = sum(len(v) for v in missing_before.values())
        log(f"  {n_tasks} task(s) missing {n_files} fixture file(s); fetching "
            f"archive from Hugging Face...")
    else:
        log("  --fixtures forced; fetching archive from Hugging Face...")

    if not _download_fixtures_archive():
        log("  ⚠️  fixture download did not complete; affected tasks will be "
            "skipped at run time")
        return
    if not _extract_fixtures_archive():
        log("  ⚠️  fixture extraction failed; affected tasks will be skipped "
            "at run time")
        return

    missing_after = _scan_missing_fixtures()
    if missing_after is None:
        log("  ✅ extraction complete (post-scan unavailable; skipping verify)")
    elif missing_after:
        n_tasks = len(missing_after)
        log(f"  ⚠️  {n_tasks} task(s) still missing fixtures after extraction "
            f"(archive may not cover them):")
        for tyaml, files in list(missing_after.items())[:10]:
            tid = Path(tyaml).parent.name
            log(f"       {tid}: {', '.join(files)}")
        if n_tasks > 10:
            log(f"       ... and {n_tasks - 10} more")
    else:
        log("  ✅ all declared task fixtures present after extraction")


def verify_installation():
    """Run basic verification checks."""
    log("Verifying installation...")
    errors = []

    # Check ce-runner CLI
    try:
        result = run(["ce-runner", "--help"], check=False)
        if result.returncode == 0:
            log("  ✅ ce-runner CLI")
        else:
            errors.append("ce-runner CLI not working")
    except FileNotFoundError:
        errors.append("ce-runner not installed")

    # Check claw-eval CLI
    try:
        result = run(["claw-eval", "--help"], check=False)
        if result.returncode == 0:
            log("  ✅ claw-eval CLI")
        else:
            errors.append("claw-eval CLI not working")
    except FileNotFoundError:
        errors.append("claw-eval not installed")

    # Check openclaw gateway
    try:
        result = run(["openclaw", "gateway", "status"], check=False)
        if "running" in result.stdout.lower():
            log("  ✅ openclaw gateway running")
        else:
            log("  ⚠️  openclaw gateway not running (start with: openclaw gateway start)")
    except FileNotFoundError:
        errors.append("openclaw not installed")

    # Check Docker
    try:
        result = run(["docker", "info"], check=False)
        if result.returncode == 0:
            log("  ✅ Docker running")
    except FileNotFoundError:
        errors.append("Docker not installed")

    # Check environment
    env_check = REPO_ROOT / "scripts" / "check_openclaw_env.py"
    if env_check.exists():
        result = run([sys.executable, str(env_check)], check=False)
        if result.returncode == 0:
            log("  ✅ openclaw environment clean")
        else:
            log("  ⚠️  openclaw environment has artifacts (run cleanup first)")

    if errors:
        log(f"\n❌ {len(errors)} error(s):")
        for e in errors:
            log(f"  - {e}")
        sys.exit(1)
    else:
        log("\n✅ Setup complete!")


def run_check_mode():
    """Read-only environment validation: pass = ce-runner batch will work.

    Checks every condition that would cause a batch run to fail, without
    modifying any state. Exit 0 on success, exit 1 with details on failure.
    """
    import importlib
    import socket

    print("=" * 60)
    print("  ce-runner Environment Check (read-only)")
    print("=" * 60)
    print()

    errors: list[str] = []
    warnings: list[str] = []

    # ── 1. Prerequisites (same as setup Step 1) ──────────────────────────
    log("Checking prerequisites...")
    prereq_errs, prereq_warns = check_prerequisites()
    errors.extend(prereq_errs)
    warnings.extend(prereq_warns)
    if not prereq_errs:
        log("  ✅ Prerequisites met")
    print()

    # ── 2. claw-eval sources present ──────────────────────────────────────
    log("Checking claw-eval sources...")
    if not (CLAW_EVAL_DIR / "pyproject.toml").exists():
        errors.append(
            f"claw-eval not fetched: {CLAW_EVAL_DIR}/pyproject.toml missing. "
            f"Fix: rerun scripts/setup_env.py"
        )
    else:
        tasks_dir = CLAW_EVAL_DIR / "tasks"
        if not tasks_dir.exists() or not list(tasks_dir.glob("*/task.yaml")):
            errors.append(f"No tasks found in {tasks_dir}")
        else:
            log("  ✅ claw-eval sources populated")
    print()

    # ── 2b. Task fixtures present (warning only) ──────────────────────────
    # Missing fixtures are non-fatal: the batch runner skips affected tasks.
    log("Checking task fixtures...")
    if CLAW_EVAL_TASKS_DIR.exists():
        missing = _scan_missing_fixtures()
        if missing is None:
            warnings.append(
                "fixture scan unavailable (import failed); cannot verify. "
                "Fix: python scripts/setup_env.py --fixtures"
            )
            log("  ⚠️  fixture scan unavailable")
        elif missing:
            n_tasks = len(missing)
            n_files = sum(len(v) for v in missing.values())
            warnings.append(
                f"{n_tasks} task(s) missing {n_files} fixture file(s); those "
                f"tasks will be skipped at run time. "
                f"Fix: python scripts/setup_env.py --fixtures"
            )
            log(f"  ⚠️  {n_tasks} task(s) missing fixtures")
        else:
            log("  ✅ all declared task fixtures present")
    else:
        log("  (skipped — submodule tasks dir not found)")
    print()

    # ── 3. Python dependencies importable ─────────────────────────────────
    log("Checking Python dependencies...")
    required_modules = [
        ("yaml", "pip install -e ."),
        ("httpx", "pip install -e ."),
        ("fastapi", "pip install -e ."),
        ("uvicorn", "pip install -e ."),
        ("docker", "pip install -e ."),
        ("openai", "pip install -e ."),
        ("mcp", "pip install -e ."),
        ("pydantic", "pip install -e claw-eval[mock,sandbox]"),
    ]
    for mod, fix in required_modules:
        try:
            importlib.import_module(mod)
        except ImportError:
            errors.append(f"Python module '{mod}' not importable. Fix: {fix}")
    if not any("module" in e for e in errors):
        log("  ✅ Python dependencies importable")
    print()

    # ── 4. CLIs installed ─────────────────────────────────────────────────
    log("Checking CLI tools...")
    if not shutil.which("ce-runner"):
        errors.append("ce-runner CLI not installed. Fix: pip install -e .")
    else:
        log("  ✅ ce-runner CLI")
    if not shutil.which("claw-eval"):
        errors.append("claw-eval CLI not installed. Fix: pip install -e claw-eval[mock,sandbox]")
    else:
        log("  ✅ claw-eval CLI")
    print()

    # ── 5. openclaw config well-formed ────────────────────────────────────
    log("Checking openclaw config...")
    if not OPENCLAW_CONFIG.exists():
        errors.append(
            f"openclaw config not found: {OPENCLAW_CONFIG}. "
            f"Fix: run 'openclaw' once, then python scripts/configure_openclaw.py"
        )
    else:
        try:
            with open(OPENCLAW_CONFIG) as f:
                oc_config = json.load(f)
            gateway = oc_config.get("gateway", {})
            if "port" not in gateway:
                errors.append("openclaw.json missing gateway.port")
            if not gateway.get("auth", {}).get("token"):
                errors.append("openclaw.json missing gateway.auth.token")
            endpoints = gateway.get("http", {}).get("endpoints", {})
            cc = endpoints.get("chatCompletions", {})
            if cc.get("enabled") is not True:
                errors.append(
                    "gateway.http.endpoints.chatCompletions.enabled not set. "
                    "Fix: python scripts/configure_openclaw.py"
                )
            else:
                log("  ✅ openclaw config well-formed")
        except (json.JSONDecodeError, OSError) as e:
            errors.append(f"openclaw config unreadable: {e}")
    print()

    # ── 6. openclaw plugins healthy (reuse preflight) ─────────────────────
    log("Checking openclaw plugins (doctor)...")
    # Import inline to avoid circular dep at script top-level
    sys.path.insert(0, str(REPO_ROOT / "src"))
    try:
        from ce_runner.preflight import check_openclaw_plugins, check_docker
    except ImportError:
        log("  (skipped — ce_runner not importable in this Python)")
        check_openclaw_plugins = None
        check_docker = None
    if check_openclaw_plugins:
        plugin_errs = check_openclaw_plugins()
        if plugin_errs:
            for pe in plugin_errs:
                errors.append(f"openclaw plugin: {pe}")
        else:
            log("  ✅ openclaw plugins healthy")
    print()

    # ── 7. Docker daemon reachable ────────────────────────────────────────
    log("Checking Docker daemon...")
    if check_docker:
        docker_errs = check_docker()
        if docker_errs:
            errors.extend(docker_errs)
        else:
            log("  ✅ Docker daemon reachable")
    else:
        log("  (skipped)")
    print()

    # ── 8. Sandbox image built ────────────────────────────────────────────
    log("Checking sandbox image...")
    sandbox_image = "claw-eval-agent:latest"
    try:
        result = subprocess.run(
            ["docker", "image", "inspect", sandbox_image],
            capture_output=True, text=True, timeout=10,
        )
        if result.returncode != 0:
            errors.append(
                f"Sandbox image '{sandbox_image}' not found. "
                f"Fix: docker build -f claw-eval/Dockerfile.agent -t {sandbox_image} claw-eval/"
            )
        else:
            log(f"  ✅ Sandbox image '{sandbox_image}' exists")
    except FileNotFoundError:
        pass  # already caught by docker check
    except subprocess.TimeoutExpired:
        errors.append("docker image inspect timed out")
    print()

    # ── 9. Gateway running + healthy ──────────────────────────────────────
    log("Checking openclaw gateway...")
    if OPENCLAW_CONFIG.exists():
        try:
            with open(OPENCLAW_CONFIG) as f:
                oc_config = json.load(f)
            port = oc_config.get("gateway", {}).get("port")
            if port:
                try:
                    import httpx
                except ImportError:
                    log("  (skipped gateway health check — httpx not available)")
                    port = None
                if port:
                    try:
                        r = httpx.get(f"http://127.0.0.1:{port}/health", timeout=5)
                        if r.status_code == 200:
                            log(f"  ✅ Gateway healthy (port {port})")
                        else:
                            errors.append(
                                f"Gateway /health returned {r.status_code}. "
                                f"Fix: openclaw gateway restart"
                            )
                    except Exception as e:
                        errors.append(
                            f"Gateway unreachable at 127.0.0.1:{port}: {e}. "
                            f"Fix: openclaw gateway start"
                        )
        except (json.JSONDecodeError, OSError):
            pass  # already reported above
    print()

    # ── 10. Model config complete ─────────────────────────────────────────
    log("Checking model config...")
    cfg_path = CLAW_EVAL_DIR / "config.yaml"
    if not cfg_path.exists():
        errors.append(f"{cfg_path} not found. Fix: python scripts/configure_model.py --api-key sk-XXX")
    else:
        try:
            import yaml as _yaml
            with open(cfg_path) as f:
                data = _yaml.safe_load(f) or {}
            for role, env_prefix in [("model", "MODEL"), ("judge", "JUDGE")]:
                section = data.get(role, {})
                api_key = section.get("api_key") or os.environ.get(f"{env_prefix}_API_KEY", "")
                base_url = section.get("base_url") or os.environ.get(f"{env_prefix}_BASE_URL", "")
                model_id = section.get("model_id") or os.environ.get(f"{env_prefix}_MODEL_ID", "")
                missing = [k for k, v in {
                    "api_key": api_key, "base_url": base_url, "model_id": model_id
                }.items() if not v]
                if missing:
                    errors.append(
                        f"{role} config missing: {missing}. "
                        f"Fix: python scripts/configure_model.py --api-key sk-XXX"
                    )
            if not any(role in e for e in errors for role in ("model config", "judge config")):
                log("  ✅ Model + judge config complete")
        except Exception as e:
            errors.append(f"Cannot read {cfg_path}: {e}")
    print()

    # ── 11. Mock service ports free ───────────────────────────────────────
    log("Checking mock service ports (9100-9116)...")
    mock_ports = list(range(9100, 9117))
    occupied = []
    for port in mock_ports:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            try:
                s.bind(("127.0.0.1", port))
            except OSError:
                occupied.append(port)
    if occupied:
        errors.append(
            f"Mock service ports occupied: {occupied}. "
            f"Fix: python scripts/check_openclaw_env.py --fix"
        )
    else:
        log("  ✅ Mock service ports free")
    print()

    # ── 12. No residual ce-runner artifacts ───────────────────────────────
    log("Checking for residual artifacts...")
    if OPENCLAW_CONFIG.exists():
        try:
            with open(OPENCLAW_CONFIG) as f:
                oc_config = json.load(f)
            agents_list = oc_config.get("agents", {}).get("list", [])
            leftover_agents = [a["id"] for a in agents_list
                               if a.get("id", "").startswith("claweval-")]
            if leftover_agents:
                errors.append(
                    f"Residual claweval agents: {leftover_agents}. "
                    f"Fix: python scripts/check_openclaw_env.py --fix"
                )
            servers = oc_config.get("mcp", {}).get("servers", {})
            leftover_servers = [k for k in servers
                                if k.startswith(("claw-eval-", "ce-mock-", "ce-sb-"))]
            if leftover_servers:
                errors.append(
                    f"Residual MCP servers: {leftover_servers}. "
                    f"Fix: python scripts/check_openclaw_env.py --fix"
                )
        except (json.JSONDecodeError, OSError):
            pass
    leftover_dirs = list((Path.home() / ".openclaw").glob("workspace-claweval-*"))
    if leftover_dirs:
        errors.append(
            f"Residual workspace dirs: {len(leftover_dirs)} found. "
            f"Fix: python scripts/check_openclaw_env.py --fix"
        )
    if not any("Residual" in e for e in errors):
        log("  ✅ No residual artifacts")
    print()

    # ── Summary ───────────────────────────────────────────────────────────
    print("=" * 60)
    if warnings:
        for w in warnings:
            log(f"  ⚠️  {w}")
    if errors:
        log(f"❌ CHECK FAILED — {len(errors)} issue(s):")
        for e in errors:
            log(f"  ✗ {e}")
        print()
        log("Fix the issues above, then re-run: python scripts/setup_env.py --check")
        sys.exit(1)
    else:
        log("✅ All checks passed — ce-runner is ready to run")
        sys.exit(0)


def main():
    print("=" * 60)
    print("  claw-eval + ce-runner Environment Setup")
    print("=" * 60)
    print()

    # Environment diagnostic summary
    log("Environment info:")
    log(f"  Python:      {sys.executable} ({sys.version.split()[0]})")
    log(f"  Platform:    {sys.platform}")
    log(f"  CWD:         {os.getcwd()}")
    log(f"  REPO_ROOT:   {REPO_ROOT}")
    log(f"  VENV_DIR:    {VENV_DIR} ({'exists' if VENV_DIR.exists() else 'not created'})")
    log(f"  VIRTUAL_ENV: {os.environ.get('VIRTUAL_ENV', '(unset)')}")
    log(f"  Verbose:     {VERBOSE}")
    log_verbose(f"  PATH: {os.environ.get('PATH', '(unset)')}")
    log_verbose(f"  sys.prefix: {sys.prefix}")
    print()

    setup_start = time.time()

    # Step 1: Check prerequisites
    log("Step 1: Checking prerequisites...")
    t0 = time.time()
    errors, warnings = check_prerequisites()
    if warnings:
        for w in warnings:
            log(f"  ⚠️  {w}")
    if errors:
        log(f"❌ {len(errors)} error(s):")
        for e in errors:
            log(f"  - {e}")
        sys.exit(1)
    log("  ✅ All prerequisites met")
    log(f"  ({time.time() - t0:.1f}s)")
    print()

    # Step 2: Fetch claw-eval sources (clone + pinned checkout)
    log("Step 2: Fetch claw-eval sources...")
    t0 = time.time()
    init_submodules()
    log(f"  ({time.time() - t0:.1f}s)")
    print()

    # Step 3: Ensure uv is available (standalone binary)
    log("Step 3: Ensure uv package manager...")
    t0 = time.time()
    ensure_uv()
    log(f"  ({time.time() - t0:.1f}s)")
    print()

    # Step 4: Create virtual environment (uv venv)
    log("Step 4: Create/activate virtual environment...")
    t0 = time.time()
    create_venv()
    log(f"  ({time.time() - t0:.1f}s)")
    print()

    # Step 5: Python dependencies
    log("Step 5: Install Python dependencies...")
    t0 = time.time()
    setup_python_deps()
    log(f"  ({time.time() - t0:.1f}s)")
    print()

    # Step 6: openclaw config
    log("Step 6: Configure openclaw...")
    t0 = time.time()
    setup_openclaw_config()
    log(f"  ({time.time() - t0:.1f}s)")
    print()

    # Step 7: ensure openclaw gateway is running
    log("Step 7: Ensure openclaw gateway...")
    t0 = time.time()
    ensure_openclaw_gateway()
    log(f"  ({time.time() - t0:.1f}s)")
    print()

    # Step 8: configure model
    log("Step 8: Configure model...")
    t0 = time.time()
    setup_model_config()
    log(f"  ({time.time() - t0:.1f}s)")
    print()

    # Step 9: mcporter
    log("Step 9: Install mcporter...")
    t0 = time.time()
    setup_mcporter()
    log(f"  ({time.time() - t0:.1f}s)")
    print()

    # Step 10: Patch Dockerfile.agent to drop TUNA mirror
    log("Step 10: Patch Dockerfile.agent...")
    t0 = time.time()
    patch_dockerfile_remove_tuna_mirror()
    log(f"  ({time.time() - t0:.1f}s)")
    print()

    # Step 11: Build sandbox Docker image
    log("Step 11: Build sandbox Docker image...")
    t0 = time.time()
    setup_sandbox_image()
    log(f"  ({time.time() - t0:.1f}s)")
    print()

    # Step 12: Download + extract task fixtures (videos, etc.)
    log("Step 12: Prepare task fixtures...")
    t0 = time.time()
    if "--skip-fixtures" in sys.argv:
        log("  --skip-fixtures set; skipping fixture download")
    else:
        prepare_fixtures(force="--fixtures" in sys.argv)
    log(f"  ({time.time() - t0:.1f}s)")
    print()

    # Step 13: Verify
    log("Step 13: Verify installation...")
    t0 = time.time()
    verify_installation()
    log(f"  ({time.time() - t0:.1f}s)")

    total = time.time() - setup_start
    print()
    log(f"Total setup time: {total:.1f}s")


if __name__ == "__main__":
    if "--help" in sys.argv or "-h" in sys.argv:
        print(__doc__)
        sys.exit(0)
    if "--check" in sys.argv:
        run_check_mode()
    elif "--fixtures" in sys.argv:
        # Standalone fixture preparation: download + extract only.
        print("=" * 60)
        print("  claw-eval task fixtures")
        print("=" * 60)
        print()
        prepare_fixtures(force=True)
    else:
        main()

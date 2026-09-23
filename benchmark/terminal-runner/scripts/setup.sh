#!/usr/bin/env bash
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

# ---------------------------------------------------------------------------
# terminal-runner setup — clone harbor + dataset, install harbor in venv.
#
# Environment variables (all optional):
#   HARBOR_URL     Harbor upstream Git URL
#                  (default: https://github.com/harbor-framework/harbor.git)
#   HARBOR_REF     Harbor branch/tag (default: main)
#   DATASET_URL    Dataset Git URL
#                  (default: https://huggingface.co/datasets/harborframework/terminal-bench-2.0)
#   DATASET_REF    Dataset branch/tag (default: main)
# ---------------------------------------------------------------------------
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
VENV_DIR="$ROOT_DIR/.venv"

HARBOR_URL="${HARBOR_URL:-https://github.com/harbor-framework/harbor.git}"
HARBOR_REF="${HARBOR_REF:-v0.15.0}"
DATASET_URL="${DATASET_URL:-https://huggingface.co/datasets/harborframework/terminal-bench-2.0}"
DATASET_REF="${DATASET_REF:-main}"

echo "==> terminal-runner setup"
echo "    Harbor:   $HARBOR_URL  ($HARBOR_REF)"
echo "    Dataset:  $DATASET_URL  ($DATASET_REF)"

# --- Virtual environment ---
# harbor requires Python >= 3.12.  If the system python3 is too old,
# try to use conda (Miniconda / Anaconda) to provision a 3.12 env.
_min_python_version() {
    local ver="$1" req="$2"  # e.g. "3.11.6" "3.12"
    local major minor
    major="${ver%%.*}"; ver="${ver#*.}"
    minor="${ver%%.*}"
    local r_major r_minor
    r_major="${req%%.*}"; req="${req#*.}"
    r_minor="${req%%.*}"
    [ "$major" -gt "$r_major" ] && return 0
    [ "$major" -eq "$r_major" ] && [ "$minor" -ge "$r_minor" ] && return 0
    return 1
}

CONDA_ENV_NAME="${CONDA_ENV_NAME:-terminal-runner-py312}"

if [ ! -d "$VENV_DIR" ]; then
    # Detect available python3
    PY_VERSION="$(python3 -c 'import platform; print(platform.python_version())' 2>/dev/null || echo 0)"
    echo "==> Detected Python $PY_VERSION"

    if _min_python_version "$PY_VERSION" 3.12; then
        echo "==> Python >= 3.12, using system python3"
        python3 -m venv "$VENV_DIR"
    else
        echo "==> Python < 3.12 detected, attempting conda fallback ..."

        # Source conda init if available
        _conda_init() {
            if [ -f "$HOME/miniconda3/etc/profile.d/conda.sh" ]; then
                source "$HOME/miniconda3/etc/profile.d/conda.sh" 2>/dev/null && return 0
            fi
            if [ -f "$HOME/anaconda3/etc/profile.d/conda.sh" ]; then
                source "$HOME/anaconda3/etc/profile.d/conda.sh" 2>/dev/null && return 0
            fi
            if [ -f "/opt/miniconda3/etc/profile.d/conda.sh" ]; then
                source "/opt/miniconda3/etc/profile.d/conda.sh" 2>/dev/null && return 0
            fi
            if [ -f "/opt/anaconda3/etc/profile.d/conda.sh" ]; then
                source "/opt/anaconda3/etc/profile.d/conda.sh" 2>/dev/null && return 0
            fi
            if command -v conda &>/dev/null; then
                return 0
            fi
            return 1
        }

        if ! _conda_init; then
            echo "ERROR: Python >= 3.12 is required (found $PY_VERSION)" >&2
            echo "       conda not found.  Install Miniconda:" >&2
            echo "         curl -sLO https://repo.anaconda.com/miniconda/Miniconda3-latest-Linux-x86_64.sh" >&2
            echo "         bash Miniconda3-latest-Linux-x86_64.sh -b -p ~/miniconda3" >&2
            exit 1
        fi

        # Accept TOS non-interactively (newer conda versions)
        conda tos accept --override-channels --channel https://repo.anaconda.com/pkgs/main 2>/dev/null || true
        conda tos accept --override-channels --channel https://repo.anaconda.com/pkgs/r  2>/dev/null || true

        if ! conda env list | grep -q "^${CONDA_ENV_NAME} "; then
            echo "==> Creating conda env '$CONDA_ENV_NAME' with Python 3.12 ..."
            conda create -y -n "$CONDA_ENV_NAME" python=3.12
        fi

        echo "==> Activating conda env '$CONDA_ENV_NAME' ..."
        conda activate "$CONDA_ENV_NAME"
        python3 -m venv "$VENV_DIR"
    fi
fi

echo "==> Activating virtual environment ..."
# shellcheck disable=SC1091
source "$VENV_DIR/bin/activate"

# --- Harbor ---
if [ -d "$ROOT_DIR/harbor" ]; then
    echo "==> harbor/ exists, checking out $HARBOR_REF ..."
    git -C "$ROOT_DIR/harbor" fetch --tags origin 2>/dev/null || true
    git -C "$ROOT_DIR/harbor" checkout "$HARBOR_REF" 2>/dev/null || echo "WARNING: could not checkout $HARBOR_REF"
else
    echo "==> Cloning harbor from $HARBOR_URL ..."
    git clone "$HARBOR_URL" "$ROOT_DIR/harbor"
    git -C "$ROOT_DIR/harbor" checkout "$HARBOR_REF"
fi

echo "==> Installing harbor (pip install -e) ..."
pip install -e "$ROOT_DIR/harbor"

# --- Dataset ---
if [ -d "$ROOT_DIR/dataset" ]; then
    echo "==> dataset/ exists, skipping clone"
else
    if ! command -v git-lfs &>/dev/null; then
        echo "ERROR: git-lfs is required to clone the dataset." >&2
        echo "       Install it first:  https://git-lfs.com" >&2
        exit 1
    fi
    echo "==> Cloning dataset from HuggingFace (requires git-lfs) ..."
    git clone "$DATASET_URL" "$ROOT_DIR/dataset"
    if [ "$DATASET_REF" != "main" ]; then
        git -C "$ROOT_DIR/dataset" checkout "$DATASET_REF"
    fi
fi

echo ""
echo "==> Setup complete."
echo "    Activate venv:  source .venv/bin/activate"
echo ""
echo "  External mode (this repo's adapter, OpenClaw on host):"
echo "    source .venv/bin/activate && export PYTHONPATH=$(pwd)"
echo "    harbor run --agent-import-path external_agent.openclaw_external_agent:OpenClawExternalAgent \\"
echo "               -p dataset/<task> -m openai/<model> ..."
echo ""
echo "  Installed mode (harbor's built-in openclaw, auto-installed in container):"
echo "    harbor run --agent openclaw \\"
echo "               -p dataset/<task> -m openai/<model> --agent-kwarg thinking=off ..."

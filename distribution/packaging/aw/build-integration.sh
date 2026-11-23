#!/usr/bin/env bash
# Prepare backend programs solely for integration acceptance.
set -euo pipefail
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exec "${PYTHON:-python3}" -B "$script_dir/integration.py" "$@"

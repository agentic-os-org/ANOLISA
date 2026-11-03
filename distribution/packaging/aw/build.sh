#!/usr/bin/env bash
# Build AW and Provider archives through distribution-owned recipes.
set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exec "${PYTHON:-python3}" -B "$script_dir/build.py" "$@"

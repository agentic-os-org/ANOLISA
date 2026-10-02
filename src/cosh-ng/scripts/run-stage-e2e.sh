#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

shell-use usage >/dev/null
shell-use agent-context >/dev/null
exec python3 "$repo_root/e2e/run.py" "$@"

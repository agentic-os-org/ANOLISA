#!/usr/bin/env bash
# Compatibility entry point for the original debug build.
set -euo pipefail
COMPONENT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec make -C "$COMPONENT_ROOT" build-debug "$@"

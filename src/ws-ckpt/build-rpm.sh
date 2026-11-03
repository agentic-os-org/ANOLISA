#!/usr/bin/env bash
# Compatibility entry point; the component Makefile is the canonical interface.
set -euo pipefail
COMPONENT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$COMPONENT_ROOT/packaging/rpm/build.sh" "$@"

#!/usr/bin/env bash
# Assemble the data bundle consumed by install-anolisa.sh without building code.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [ "$#" -ne 1 ]; then
  echo "usage: $0 OUTPUT.tar.gz" >&2
  exit 2
fi
OUTPUT="$1"
mkdir -p "$(dirname "$OUTPUT")"
OUTPUT="$(cd "$(dirname "$OUTPUT")" && pwd)/$(basename "$OUTPUT")"
TEMPORARY="$(mktemp -d)"
trap 'rm -rf "$TEMPORARY"' EXIT

install -d "$TEMPORARY/manifests/components" "$TEMPORARY/manifests/osbase" \
  "$TEMPORARY/templates"
for component in cosh-ng sec-core agentsight tokenless skillfs ws-ckpt agent-memory; do
  install -d "$TEMPORARY/manifests/components/$component"
  install -m 0644 "$ROOT/manifests/components/$component/component.toml" \
    "$TEMPORARY/manifests/components/$component/component.toml"
done
cp -R "$ROOT/manifests/osbase/." "$TEMPORARY/manifests/osbase/"
cp -R "$ROOT/templates/." "$TEMPORARY/templates/"
tar -czf "$OUTPUT" -C "$TEMPORARY" manifests templates

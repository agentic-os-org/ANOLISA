#!/usr/bin/env bash
# Stages the complete prebuilt payload consumed by agentsight.spec.in.
set -euo pipefail

if [[ $# -ne 1 || -z "$1" ]]; then
    echo "usage: $0 <destination>" >&2
    exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_root="${AGENTSIGHT_PROJECT_ROOT:-$(cd "$script_dir/.." && pwd)}"
destination="$1"

sources=(
    "target/release/agentsight"
    "scripts/agentsight-start.sh"
    "scripts/agentsight.service"
    "scripts/agentsight-enforcer.service"
    "agentsight.json"
    "component.toml"
    "README.md"
    "README_zh.md"
    "LICENSE"
)
targets=(
    "agentsight"
    "agentsight-start"
    "agentsight.service"
    "agentsight-enforcer.service"
    "agentsight.json"
    "component.toml"
    "README.md"
    "README_zh.md"
    "LICENSE"
)

missing=0
for source in "${sources[@]}"; do
    if [[ ! -f "$project_root/$source" ]]; then
        echo "missing AgentSight RPM payload source: $project_root/$source" >&2
        missing=1
    fi
done
if (( missing != 0 )); then
    exit 1
fi

if [[ -e "$destination" ]]; then
    echo "AgentSight RPM payload destination already exists: $destination" >&2
    exit 1
fi

# No shared "latest" fallback: a missing handoff requires a fresh attested build.
if [[ -z "${ENFORCER_BIN:-}" ]]; then
    ENFORCER_BIN="$("$project_root/scripts/build-enforcer.sh")"
fi
install -d -m 0755 "$destination"
python3 "$script_dir/copy-enforcer.py" "$ENFORCER_BIN" "$destination/agentsight-enforcer"
for index in "${!sources[@]}"; do
    mode=0644
    case "${targets[$index]}" in
        agentsight|agentsight-start)
            mode=0755
            ;;
    esac
    install -p -m "$mode" \
        "$project_root/${sources[$index]}" "$destination/${targets[$index]}"
done

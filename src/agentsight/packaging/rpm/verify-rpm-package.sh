#!/usr/bin/env bash
# Verifies the deployable AgentSight RPM rather than its source declarations.
set -euo pipefail

usage() {
    echo "usage: $0 <rpm-path> | --file-list <path>" >&2
}

if [[ $# -eq 1 ]]; then
    if ! command -v rpm >/dev/null 2>&1; then
        echo "rpm is required to inspect an AgentSight package" >&2
        exit 2
    fi
    file_list="$(mktemp)"
    trap 'rm -f "$file_list"' EXIT
    rpm -qlp "$1" > "$file_list"
elif [[ $# -eq 2 && "$1" == "--file-list" ]]; then
    file_list="$2"
else
    usage
    exit 2
fi

if [[ ! -f "$file_list" ]]; then
    echo "RPM file list not found: $file_list" >&2
    exit 2
fi

required_paths=(
    "/usr/local/bin/agentsight"
    "/usr/local/bin/agentsight-enforcer"
    "/usr/local/bin/agentsight-start"
    "/usr/lib/systemd/system/agentsight.service"
    "/usr/lib/systemd/system/agentsight-enforcer.service"
    "/etc/agentsight/config.json"
    "/usr/share/anolisa/components/agentsight/component.toml"
)

missing=0
for path in "${required_paths[@]}"; do
    if ! grep -Fxq -- "$path" "$file_list"; then
        echo "AgentSight RPM is missing required path: $path" >&2
        missing=1
    fi
done
if (( missing != 0 )); then
    exit 1
fi

echo "AgentSight RPM artifact contract passed."

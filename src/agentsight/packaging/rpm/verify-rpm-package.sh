#!/usr/bin/env bash
# Verifies the deployable AgentSight RPM rather than its source declarations.
set -euo pipefail

usage() {
    echo "usage: $0 <rpm-path> | [--package agentsight|agentsight-core|agentsight-plugins] --file-list <path>" >&2
}

package=agentsight-core
if [[ ${1:-} == --package && $# -eq 4 ]]; then
    package="$2"
    shift 2
fi
if [[ $# -eq 1 ]]; then
    if ! command -v rpm >/dev/null 2>&1; then
        echo "rpm is required to inspect an AgentSight package" >&2
        exit 2
    fi
    file_list="$(mktemp)"
    trap 'rm -f "$file_list"' EXIT
    package="$(rpm -qp --qf '%{NAME}' "$1")"
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

if grep -E '^/usr/share/anolisa/components(/|$)' "$file_list" >/dev/null; then
    echo 'Component RPMs must not own ANOLISA discovery contracts' >&2
    exit 1
fi

case "$package" in
agentsight-core)
required_paths=(
    "/usr/bin/agentsight"
    "/usr/bin/agentsight-enforcer"
    "/usr/bin/agentsight-start"
    "/usr/lib/systemd/system/agentsight.service"
    "/usr/lib/systemd/system/agentsight-enforcer.service"
    "/etc/agentsight/config.json"
)
;;
agentsight-plugins)
required_paths=(
    "/usr/share/agentsight/adapters/dsh/lib/index.js"
    "/usr/share/agentsight/adapters/dsh/package.json"
    "/usr/share/agentsight/adapters/dsh/cordis.patch.yml"
)
;;
agentsight)
if grep -E '^/' "$file_list" >/dev/null; then
    echo 'AgentSight compatibility package must not own runtime files' >&2
    exit 1
fi
echo "AgentSight RPM artifact contract passed."
exit 0
;;
*) echo "unexpected AgentSight RPM package: $package" >&2; exit 1;;
esac

missing=0
for path in "${required_paths[@]}"; do
    if ! grep -Fxq -- "$path" "$file_list"; then
        echo "$package RPM is missing required path: $path" >&2
        missing=1
    fi
done
if (( missing != 0 )); then
    exit 1
fi

echo "AgentSight RPM artifact contract passed."

#!/usr/bin/env bash
# Stages the complete prebuilt payload consumed by agentsight.spec.in.
set -euo pipefail

if [[ $# -ne 1 || -z "$1" ]]; then
    echo "usage: $0 <destination>" >&2
    exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
project_root="${AGENTSIGHT_PROJECT_ROOT:-$(cd "$script_dir/../.." && pwd)}"
destination="$1"

sources=(
    "target/release/agentsight"
    "agentsight.json"
    "README.md"
    "README_zh.md"
    "LICENSE"
)
targets=(
    "agentsight"
    "agentsight.json"
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
for source in packaging/systemd/render.py packaging/systemd/agentsight-start.sh.in \
    packaging/systemd/agentsight.service.in packaging/systemd/agentsight-enforcer.service.in; do
    if [[ ! -f "$project_root/$source" ]]; then
        echo "missing AgentSight RPM payload source: $project_root/$source" >&2
        missing=1
    fi
done
if (( missing != 0 )); then
    exit 1
fi

for source in dsh-plugin/package.json dsh-plugin/cordis.patch.yml dsh-plugin/lib/index.js; do
    if [[ ! -f "$project_root/$source" ]]; then
        echo "missing AgentSight RPM plugin source: $project_root/$source; run make build-dsh-plugin" >&2
        exit 1
    fi
done
if [[ -n "$(find "$project_root/dsh-plugin/lib" -type l -print -quit)" ]]; then
    echo "AgentSight RPM plugin resources must not contain symlinks" >&2
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
python3 "$project_root/scripts/copy-enforcer.py" "$ENFORCER_BIN" "$destination/agentsight-enforcer"
for index in "${!sources[@]}"; do
    mode=0644
    case "${targets[$index]}" in
        agentsight)
            mode=0755
            ;;
    esac
    install -p -m "$mode" \
        "$project_root/${sources[$index]}" "$destination/${targets[$index]}"
done

install -d -m 0755 "$destination/packaging/systemd"
install -p -m 0644 "$project_root/packaging/systemd/render.py" \
    "$project_root/packaging/systemd/agentsight-start.sh.in" \
    "$project_root/packaging/systemd/agentsight.service.in" \
    "$project_root/packaging/systemd/agentsight-enforcer.service.in" \
    "$destination/packaging/systemd/"

install -d -m 0755 "$destination/dsh-plugin"
cp -Rp "$project_root/dsh-plugin/lib" "$destination/dsh-plugin/"
install -p -m 0644 "$project_root/dsh-plugin/package.json" \
    "$project_root/dsh-plugin/cordis.patch.yml" "$destination/dsh-plugin/"

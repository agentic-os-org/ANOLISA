#!/usr/bin/env bash
# Preserve the exact RTK license and source inputs beside the linked executable.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
[ "$#" -eq 1 ] || { printf 'usage: stage-rtk-notices.sh DESTINATION\n' >&2; exit 2; }
destination="$1"
install -d -m 0755 "$destination"
install -m 0644 "$root/third_party/rtk/LICENSE" "$destination/LICENSE"
install -m 0644 "$root/third_party/rtk/.anolisa-rtk-commit" "$destination/source-commit"
install -m 0644 "$root/third_party/rtk/.anolisa-rtk-patches" "$destination/patches-sha256"
read -r -a patches <<< "$(sed -n 's/^PATCHES=(\(.*\))$/\1/p' "$root/scripts/setup-rtk.sh")"
[ "${#patches[@]}" -gt 0 ] || { echo 'RTK patch list is missing' >&2; exit 1; }
for patch in "${patches[@]}"; do
    install -m 0644 "$root/third_party/patches/$patch" "$destination/$patch"
done
printf 'RTK v0.49.0\nUpstream: https://github.com/rtk-ai/rtk\nModified by ANOLISA; patches and pinned revision accompany this notice.\n' > "$destination/SOURCE"

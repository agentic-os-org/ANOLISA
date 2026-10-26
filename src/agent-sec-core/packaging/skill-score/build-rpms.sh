#!/usr/bin/env bash
# Build optional assessment packages without replacing the system CLI or daemon.
set -euo pipefail
[ "$#" -ge 1 ] && [ "$#" -le 2 ] || { echo "usage: $0 OUTPUT_DIR [--allow-dirty]" >&2; exit 2; }
[ "$(uname -sm)" = "Linux x86_64" ] || { echo "Linux x86_64 is required" >&2; exit 1; }
score_component="$(cd "$(dirname "$0")/../.." && pwd)"
score_repo="$(git -C "$score_component" rev-parse --show-toplevel)"
score_output="$(realpath -m "$1")"
case "$score_output/" in "$score_component/"*) echo "output must be outside the component" >&2; exit 2;; esac
score_dirty="$(git -C "$score_repo" status --porcelain)"
if [ -n "$score_dirty" ] && [ "${2:-}" != --allow-dirty ]; then
    echo "release builds require a clean commit; --allow-dirty produces a development artifact" >&2
    exit 1
fi
[ -z "${2:-}" ] || [ "$2" = --allow-dirty ] || exit 2
mkdir -p "$score_output"
[ -z "$(ls -A "$score_output")" ] || { echo "output directory must be empty" >&2; exit 2; }
score_work="$(mktemp -d)"
trap 'rm -rf "$score_work"' EXIT
bash "$score_component/packaging/skill-score/build-payload.sh" "$score_output" ${2:+"$2"}
gzip -d "$score_output/skill-score-images.tar.gz"
score_version="$(python3 -c 'import sys,tomllib; print(tomllib.load(open(sys.argv[1],"rb"))["project"]["version"])' "$score_component/agent-sec-cli/pyproject.toml")"
score_glibc="$(getconf GNU_LIBC_VERSION | awk '{print $2}')"
[[ "$score_glibc" =~ ^[0-9]+\.[0-9]+$ ]] || { echo "cannot determine build glibc" >&2; exit 1; }
score_rpmbuild="$score_work/rpmbuild"
mkdir -p "$score_rpmbuild"/{SOURCES,SPECS,BUILD,BUILDROOT,RPMS,SRPMS}
cp "$score_output/skill-score-payload.tar.gz" "$score_rpmbuild/SOURCES/"
sed -e "s/@VERSION@/$score_version/g" -e "s/@GLIBC_VERSION@/$score_glibc/g" \
    "$score_component/packaging/skill-score/skill-score.spec.in" > "$score_rpmbuild/SPECS/skill-score.spec"
rpmbuild -bb --define "_topdir $score_rpmbuild" "$score_rpmbuild/SPECS/skill-score.spec"
cp "$score_rpmbuild"/RPMS/x86_64/*.rpm "$score_output/"
score_verify_args=("$score_output" --source-commit "$(git -C "$score_repo" rev-parse HEAD)")
[ -z "$score_dirty" ] || score_verify_args+=(--development)
python3 "$score_component/packaging/skill-score/verify-release.py" "${score_verify_args[@]}"

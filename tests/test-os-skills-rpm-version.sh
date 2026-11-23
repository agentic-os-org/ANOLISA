#!/bin/bash
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/scripts" "$tmp/src/os-skills/packaging" \
    "$tmp/src/os-skills/ai/sample" "$tmp/src/os-skills/ai/incubating"
cp "$root/scripts/rpm-build.sh" "$tmp/scripts/"
cp "$root/src/os-skills/os-skills.spec.in" "$tmp/src/os-skills/"
cp "$root/src/os-skills/packaging/release_catalog.py" "$tmp/src/os-skills/packaging/"
printf 'ai/sample\n' > "$tmp/src/os-skills/packaging/released-skills.txt"
cat > "$tmp/src/os-skills/component.toml" <<'TOML'
[component]
name = "os-skills"
version = "7.8.9"

[[adapters]]
framework = "openclaw"

[[adapters.openclaw.skills]]
name = "sample"
source = "{datadir}/skills/sample/"

[[adapters.openclaw.skills]]
name = "incubating"
source = "{datadir}/skills/incubating/"
TOML
printf '%s\n' '---' 'name: sample' '---' 'test skill' > "$tmp/src/os-skills/ai/sample/SKILL.md"
printf '%s\n' '---' 'name: incubating' '---' 'test skill' > "$tmp/src/os-skills/ai/incubating/SKILL.md"
printf 'license\n' > "$tmp/src/os-skills/LICENSE"
cat > "$tmp/rpmbuild" <<'MOCK'
#!/bin/bash
set -euo pipefail
spec=${@: -1}
grep -Eq "^Version:[[:space:]]+${EXPECTED_VERSION}$" "$spec"
source_archive="$(dirname "$(dirname "$spec")")/SOURCES/os-skills-${EXPECTED_VERSION}.tar.gz"
archive_root="os-skills-${EXPECTED_VERSION}"
members=$(tar -tzf "$source_archive")
grep -Fxq "$archive_root/component.toml" <<< "$members"
grep -Fxq "$archive_root/ai/sample/SKILL.md" <<< "$members"
if grep -q '/ai/incubating/' <<< "$members"; then
    echo "Incubating skill is present in RPM Source0" >&2
    exit 1
fi
manifest=$(tar -Oxzf "$source_archive" "$archive_root/component.toml")
if grep -Fq 'name = "incubating"' <<< "$manifest"; then
    echo "Incubating skill is registered in the RPM contract" >&2
    exit 1
fi
if [ "${EXPECTED_SAMPLE_ADAPTER:-}" = true ]; then
    grep -Fq 'name = "sample"' <<< "$manifest"
fi
MOCK
chmod +x "$tmp/rpmbuild"
EXPECTED_VERSION=7.8.9 EXPECTED_SAMPLE_ADAPTER=true RPMBUILD="$tmp/rpmbuild" bash "$tmp/scripts/rpm-build.sh" os-skills
EXPECTED_VERSION=9.9.9 VERSION=9.9.9 RPMBUILD="$tmp/rpmbuild" bash "$tmp/scripts/rpm-build.sh" os-skills
cp "$root/src/os-skills/component.toml" "$tmp/src/os-skills/component.toml"
component_version=$(python3 - "$root/scripts/check-component-versions.py" <<'PY'
import runpy
import sys

checker = runpy.run_path(sys.argv[1])
print(checker["read_version"]("src/os-skills/component.toml"))
PY
)
EXPECTED_VERSION="$component_version" RPMBUILD="$tmp/rpmbuild" bash "$tmp/scripts/rpm-build.sh" os-skills

rm "$tmp/src/os-skills/packaging/released-skills.txt"
mkdir "$tmp/fail"
if TMPDIR="$tmp/fail" RPMBUILD=/usr/bin/true bash "$tmp/scripts/rpm-build.sh" os-skills; then
    echo "RPM builder accepted a missing os-skills release catalog" >&2
    exit 1
fi
test -z "$(find "$tmp/fail" -mindepth 1 -print -quit)"

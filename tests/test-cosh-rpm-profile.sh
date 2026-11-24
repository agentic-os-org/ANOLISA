#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT
mkdir -p "$temporary/scripts" "$temporary/src/cosh-ng/packaging/rpm"
cp "$root/scripts/rpm-build.sh" "$temporary/scripts/"
cp "$root/src/cosh-ng/packaging/rpm/cosh-ng.spec.in" "$temporary/src/cosh-ng/packaging/rpm/"
printf '%s\n' '[workspace.package]' 'version = "9.8.7"' > "$temporary/src/cosh-ng/Cargo.toml"
printf 'source fixture\n' > "$temporary/src/cosh-ng/source.txt"
cat > "$temporary/rpmbuild" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$@" > "$RPM_ARGUMENTS"
spec=${@: -1}
grep -Eq '^Version:[[:space:]]+9.8.7$' "$spec"
archive="$(dirname "$(dirname "$spec")")/SOURCES/cosh-ng-9.8.7.tar.gz"
tar -tzf "$archive" | grep -Fxq 'cosh-ng-9.8.7/source.txt'
if grep -q '/var/tmp/cosh-alinux2\|Source1:' "$spec"; then
    exit 1
fi
MOCK
chmod +x "$temporary/rpmbuild"
arguments="$temporary/arguments"
RPM_ARGUMENTS="$arguments" DIST_TAG=.alinux2 COSH_RPM_PROFILE=alinux2 \
    RPMBUILD="$temporary/rpmbuild" bash "$temporary/scripts/rpm-build.sh" cosh-ng
grep -Fxq 'dist .alinux2' "$arguments"
grep -Fxq -- '--with' "$arguments"
grep -Fxq 'alinux2' "$arguments"
RPM_ARGUMENTS="$arguments" COSH_RPM_PROFILE=default \
    RPMBUILD="$temporary/rpmbuild" bash "$temporary/scripts/rpm-build.sh" cosh-ng
if grep -Fxq 'alinux2' "$arguments"; then
    exit 1
fi
mkdir "$temporary/failed"
if RPM_ARGUMENTS="$arguments" COSH_RPM_PROFILE=unknown TMPDIR="$temporary/failed" \
    RPMBUILD="$temporary/rpmbuild" bash "$temporary/scripts/rpm-build.sh" cosh-ng; then
    exit 1
fi
test -z "$(find "$temporary/failed" -mindepth 1 -print -quit)"
printf 'cosh-ng RPM profile entry tests passed\n'

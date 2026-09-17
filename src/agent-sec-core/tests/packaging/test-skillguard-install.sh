#!/usr/bin/env bash
# Exercise staged source installation with the real Linux V2 build artifacts.
set -euo pipefail

if [ "$(uname -s)" != Linux ]; then
    echo "SkillGuard installation checks require Linux" >&2
    exit 1
fi

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
BIN_DIR=${V2_BIN_BUILD_DIR:-$ROOT/target/v2/bin}
for binary in agent-sec-cli agent-sec-daemon; do
    test -x "$BIN_DIR/$binary" || {
        echo "Missing $BIN_DIR/$binary; run make build-cli-v2 first" >&2
        exit 1
    }
done

TEMP=$(mktemp -d)
trap 'rm -rf -- "$TEMP"' EXIT
STAGE=$TEMP/v2
make -C "$ROOT" install-core-v2 DESTDIR="$STAGE" V2_BIN_BUILD_DIR="$BIN_DIR"

for binary in agent-sec-cli agent-sec-daemon; do
    cmp "$BIN_DIR/$binary" "$STAGE/usr/bin/$binary"
    test "$(stat -c %a "$STAGE/usr/bin/$binary")" = 755
    "$STAGE/usr/bin/$binary" --help > "$TEMP/$binary-help.txt"
done
grep -q skill-ledger "$TEMP/agent-sec-cli-help.txt"
test ! -e "$STAGE/usr/lib/systemd/user/agent-sec-core.service"
test ! -e "$STAGE/usr/lib/systemd/system/multi-user.target.wants/agent-sec-core.service"
test ! -e "$STAGE/var/lib/agent-sec/skillguard/signing-key.pk8"
test "$(stat -c %a "$STAGE/etc/agent-sec/skillguard.json")" = 600
test "$(stat -c %a "$STAGE/usr/lib/systemd/system/agent-sec-core.service")" = 644

# Reinstallation must preserve operator-selected settings and permissions.
printf '{"stateDir":"/var/lib/operator-skillguard","managedSkillDirs":[]}\n' \
    > "$STAGE/etc/agent-sec/skillguard.json"
cp "$STAGE/etc/agent-sec/skillguard.json" "$TEMP/expected.json"
make -C "$ROOT" install-core-v2 DESTDIR="$STAGE" V2_BIN_BUILD_DIR="$BIN_DIR"
cmp "$TEMP/expected.json" "$STAGE/etc/agent-sec/skillguard.json"
test "$(stat -c %a "$STAGE/etc/agent-sec/skillguard.json")" = 600

# V1 keeps its user unit and never acquires the V2 system service.
make -C "$ROOT" install-systemd-user DESTDIR="$TEMP/v1"
test -f "$TEMP/v1/usr/lib/systemd/user/agent-sec-core.service"
test ! -e "$TEMP/v1/usr/lib/systemd/system/agent-sec-core.service"
echo "PASS: V2 artifact installation, retained config and V1 unit isolation"

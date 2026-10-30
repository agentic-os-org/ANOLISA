#!/usr/bin/env bash
# Verify direct and DESTDIR file installation without builds or activation.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d /tmp/cosh-ng-system-staging.XXXXXX)"
TMP="$(cd "$TMP" && pwd -P)"
trap 'rm -rf "$TMP"' EXIT

# Every install path must remain independent of compilers and host activation.
TRAPS="$TMP/install-traps"
mkdir "$TRAPS"
export DELIVERY_TEST_LOG="$TMP/unexpected-install-tools"
export DELIVERY_TEST_FIND="$(command -v find)"
for tool in node npm cargo rustup just systemctl chsh openclaw; do
    printf '%s\n' '#!/bin/sh' 'printf "%s\\n" "$0" >> "$DELIVERY_TEST_LOG"' 'exit 97' > "$TRAPS/$tool"
    chmod 0755 "$TRAPS/$tool"
done
cat > "$TRAPS/find" <<'FINDSCRIPT'
#!/bin/sh
for argument; do
    case "$argument" in /|//) echo 'ERROR: installer scanned the host root' >&2; exit 93;; esac
done
exec "$DELIVERY_TEST_FIND" "$@"
FINDSCRIPT
chmod 0755 "$TRAPS/find"
export PATH="$TRAPS:$PATH"

layout() {
    env -u AWPRJ -u PREFIX -u INSTALL_PROFILE make --no-print-directory -s -C "$ROOT" layout "$@"
}
default_layout="$(layout INSTALL_PROFILE=system)"
grep -Fxq 'AWPRJ=' <<< "$default_layout"
grep -Fxq 'LIBEXECDIR=/usr/local/libexec/cosh-ng' <<< "$default_layout"
grep -Fxq 'DATADIR=/usr/local/share/cosh-ng' <<< "$default_layout"
grep -Fxq 'SKILLDIR=/usr/local/share/cosh-ng/skills' <<< "$default_layout"
distribution_layout="$(layout INSTALL_PROFILE=system AWPRJ=anolisa PREFIX=/opt/vendor/)"
grep -Fxq 'LIBEXECDIR=/opt/vendor/libexec/anolisa/cosh-ng' <<< "$distribution_layout"
grep -Fxq 'SKILLDIR=/opt/vendor/share/anolisa/skills' <<< "$distribution_layout"
mkdir -p "$TMP/user-home"
user_layout="$(HOME="$TMP/user-home" XDG_DATA_HOME="$TMP/xdg-data" layout INSTALL_PROFILE=user)"
grep -Fxq "PREFIX=$TMP/user-home/.local" <<< "$user_layout"
grep -Fxq "LIBEXECDIR=$TMP/user-home/.local/libexec/cosh-ng" <<< "$user_layout"
grep -Fxq "DATADIR=$TMP/xdg-data/cosh-ng" <<< "$user_layout"
fallback_layout="$(HOME="$TMP/user-home" XDG_DATA_HOME= layout INSTALL_PROFILE=user)"
grep -Fxq "DATADIR=$TMP/user-home/.local/share/cosh-ng" <<< "$fallback_layout"
XDG_DATA_HOME=relative layout INSTALL_PROFILE=system PREFIX=/usr >/dev/null
if HOME="$TMP/user-home" XDG_DATA_HOME=relative layout INSTALL_PROFILE=user >/dev/null 2>&1; then
    printf 'ERROR: relative XDG data root accepted\n' >&2; exit 1
fi
for invalid in '../other' 'a/b' '.hidden'; do
    if layout AWPRJ="$invalid" >/dev/null 2>&1; then
        printf 'ERROR: invalid namespace accepted: %s\n' "$invalid" >&2; exit 1
    fi
done
if layout INSTALL_PROFILE=unsupported >/dev/null 2>&1 || layout PREFIX=relative >/dev/null 2>&1; then
    printf 'ERROR: unsupported system layout accepted\n' >&2; exit 1
fi

# Exercise both effective-UID defaults without changing the test process identity.
UID_TOOLS="$TMP/uid-tools"
mkdir "$UID_TOOLS"
export DELIVERY_TEST_ID="$(command -v id)"
cat > "$UID_TOOLS/id" <<'IDSCRIPT'
#!/bin/sh
if [ "$#" = 1 ] && [ "$1" = -u ]; then
    printf '%s\n' "$DELIVERY_TEST_UID"
else
    exec "$DELIVERY_TEST_ID" "$@"
fi
IDSCRIPT
chmod 0755 "$UID_TOOLS/id"
for uid in 0 1001; do
    detected="$(PATH="$UID_TOOLS:$PATH" DELIVERY_TEST_UID="$uid" HOME="$TMP/user-home" XDG_DATA_HOME="$TMP/xdg-data" layout)"
    if [ "$uid" = 0 ]; then
        grep -Fxq 'INSTALL_PROFILE=system' <<< "$detected"
        grep -Fxq 'PREFIX=/usr/local' <<< "$detected"
        grep -Fxq 'DATADIR=/usr/local/share/cosh-ng' <<< "$detected"
    else
        grep -Fxq 'INSTALL_PROFILE=user' <<< "$detected"
        grep -Fxq "PREFIX=$TMP/user-home/.local" <<< "$detected"
        grep -Fxq "DATADIR=$TMP/xdg-data/cosh-ng" <<< "$detected"
    fi
    system_override="$(PATH="$UID_TOOLS:$PATH" DELIVERY_TEST_UID="$uid" XDG_DATA_HOME=relative layout INSTALL_PROFILE=system PREFIX=/usr)"
    grep -Fxq 'DATADIR=/usr/share/cosh-ng' <<< "$system_override"
    user_override="$(PATH="$UID_TOOLS:$PATH" DELIVERY_TEST_UID="$uid" HOME="$TMP/user-home" XDG_DATA_HOME="$TMP/xdg-data" layout INSTALL_PROFILE=user PREFIX=/opt/programs)"
    grep -Fxq 'PREFIX=/opt/programs' <<< "$user_override"
    grep -Fxq "DATADIR=$TMP/xdg-data/cosh-ng" <<< "$user_override"
    direct_layout="$(env -u INSTALL_PROFILE -u PREFIX -u AWPRJ PATH="$UID_TOOLS:$PATH" DELIVERY_TEST_UID="$uid" HOME="$TMP/user-home" XDG_DATA_HOME="$TMP/xdg-data" bash "$ROOT/packaging/install.sh" --print-layout)"
    test "$direct_layout" = "$detected"
done

# Explicit system staging remains independent of the caller's XDG environment.
export INSTALL_PROFILE=system PREFIX=/usr
SOURCE="$TMP/source"
mkdir -p "$SOURCE/skills/manage-task-checkpoints" "$TMP/bin"
cp -R "$ROOT/packaging" "$SOURCE/packaging"
cp "$ROOT/Makefile" "$SOURCE/Makefile"
cp "$ROOT/LICENSE" "$SOURCE/LICENSE"
printf 'fixture skill\n' > "$SOURCE/skills/manage-task-checkpoints/SKILL.md"
for binary in cosh-cli cosh-core cosh-gateway cosh-shell; do
    printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/bin/$binary"
    chmod 0755 "$TMP/bin/$binary"
done
# Exercise the included rules from an extracted, component-only source archive.
tar -cf "$TMP/source.tar" -C "$SOURCE" .
rm -rf "$SOURCE"
mkdir "$SOURCE"
tar -xf "$TMP/source.tar" -C "$SOURCE"
test -f "$SOURCE/packaging/anolisa/raw/component.toml"
test -f "$SOURCE/packaging/anolisa/raw/component.macos.toml"
test -f "$SOURCE/packaging/anolisa/rpm/component.toml"
test ! -e "$SOURCE/.anolisa"
make -s -C "$SOURCE" install \
    DESTDIR="$TMP/stage" BIN_DIR="$TMP/bin" PREFIX=/opt/vendor \
    BINDIR=/custom/bin LIBEXECDIR=/custom/runtime DATADIR=/custom/data SKILLDIR=/custom/skills
test -x "$TMP/stage/custom/bin/cosh-cli"
test -x "$TMP/stage/custom/runtime/cosh-shell"
test -f "$TMP/stage/custom/data/systemd/cosh-gateway@.service.in"
cmp "$SOURCE/LICENSE" "$TMP/stage/custom/data/LICENSE"
test -f "$TMP/stage/custom/skills/manage-task-checkpoints/SKILL.md"
test ! -e "$TMP/stage/custom/bin/cosh"
test ! -e "$TMP/stage/opt/vendor/lib/systemd/system/cosh-gateway@.service"
make -s -C "$SOURCE" install \
    DESTDIR="$TMP/distribution" BIN_DIR="$TMP/bin" PREFIX=/opt/vendor AWPRJ=anolisa
test -x "$TMP/distribution/opt/vendor/libexec/anolisa/cosh-ng/cosh-core"
cmp "$SOURCE/LICENSE" "$TMP/distribution/opt/vendor/share/anolisa/cosh-ng/LICENSE"
make -s -j4 -C "$SOURCE" install \
    DESTDIR="$TMP/parallel" BIN_DIR="$TMP/bin" PREFIX=/opt/vendor AWPRJ=parallel
test -x "$TMP/parallel/opt/vendor/libexec/parallel/cosh-ng/cosh-core"
test -f "$TMP/parallel/opt/vendor/share/parallel/skills/manage-task-checkpoints/SKILL.md"
make -s -C "$SOURCE" install BIN_DIR="$TMP/bin" PREFIX="$TMP/direct"
test -x "$TMP/direct/bin/cosh-cli"
test -x "$TMP/direct/libexec/cosh-ng/cosh-core"
test -f "$TMP/direct/share/cosh-ng/skills/manage-task-checkpoints/SKILL.md"
make -s -C "$SOURCE" install BIN_DIR="$TMP/bin" PREFIX="$TMP/direct"
make -s -C "$SOURCE" install BIN_DIR="$TMP/bin" PREFIX="$TMP/namespaced" AWPRJ=anolisa
test -x "$TMP/namespaced/libexec/anolisa/cosh-ng/cosh-core"
test -f "$TMP/namespaced/share/anolisa/skills/manage-task-checkpoints/SKILL.md"

for target in stage-package stage-package-core stage-package-plugin stage-package-skills build-core install-core install-skills install-plugins; do
    if make -s -n -C "$SOURCE" "$target" >/dev/null 2>&1; then
        printf 'ERROR: unsupported install/staging target exists: %s\n' "$target" >&2; exit 1
    fi
done
# User installation uses XDG resources and the explicitly chosen program prefix.
make -s -C "$SOURCE" install INSTALL_PROFILE=user PREFIX="$TMP/user-prefix" \
    BIN_DIR="$TMP/bin" HOME="$TMP/user-home" XDG_DATA_HOME="$TMP/user-data"
test -x "$TMP/user-prefix/bin/cosh-cli"
for uid in 0 1001; do
    stage="$TMP/uid-install-$uid"
    env -u INSTALL_PROFILE -u PREFIX PATH="$UID_TOOLS:$PATH" DELIVERY_TEST_UID="$uid" \
        HOME="$TMP/user-home" XDG_DATA_HOME="$TMP/xdg-data" \
        make -s -C "$SOURCE" install BIN_DIR="$TMP/bin" DESTDIR="$stage"
    if [ "$uid" = 0 ]; then
        test -x "$stage/usr/local/bin/cosh-cli"
    else
        test -x "$stage$TMP/user-home/.local/bin/cosh-cli"
    fi
done
test -x "$TMP/user-prefix/libexec/cosh-ng/cosh-core"
test -f "$TMP/user-data/cosh-ng/skills/manage-task-checkpoints/SKILL.md"
for group in core skills; do
    if COMPONENT_ROOT="$SOURCE" BIN_DIR="$TMP/bin" DESTDIR="$TMP/removed-$group" \
        bash "$SOURCE/packaging/install.sh" "$group" >/dev/null 2>&1; then
        printf 'ERROR: installer accepted removed file group: %s\n' "$group" >&2; exit 1
    fi
    test ! -e "$TMP/removed-$group"
done
mv "$TMP/bin/cosh-core" "$TMP/bin/cosh-core.saved"
if make -s -C "$SOURCE" install BIN_DIR="$TMP/bin" DESTDIR="$TMP/missing-binary" >/dev/null 2>&1; then
    printf 'ERROR: install accepted a missing binary\n' >&2; exit 1
fi
test ! -e "$TMP/missing-binary"
mv "$TMP/bin/cosh-core.saved" "$TMP/bin/cosh-core"
mv "$SOURCE/skills/manage-task-checkpoints/SKILL.md" "$TMP/SKILL.saved"
if make -s -C "$SOURCE" install BIN_DIR="$TMP/bin" DESTDIR="$TMP/missing-skill" >/dev/null 2>&1; then
    printf 'ERROR: install accepted a missing skill\n' >&2; exit 1
fi
test ! -e "$TMP/missing-skill"
mv "$TMP/SKILL.saved" "$SOURCE/skills/manage-task-checkpoints/SKILL.md"
mkdir -p "$TMP/outside"
ln -s "$TMP/outside" "$TMP/link"
for destination in "$TMP/link" "$TMP/link/child" "$TMP/../escape" relative / //; do
    if make -s -C "$SOURCE" install BIN_DIR="$TMP/bin" DESTDIR="$destination" >/dev/null 2>&1; then
        printf 'ERROR: unsafe staging root accepted: %s\n' "$destination" >&2; exit 1
    fi
done
mkdir -p "$TMP/unsafe/usr/libexec/cosh-ng"
ln -s "$TMP/outside/overwritten" "$TMP/unsafe/usr/libexec/cosh-ng/cosh-core"
if make -s -C "$SOURCE" install BIN_DIR="$TMP/bin" DESTDIR="$TMP/unsafe" >/dev/null 2>&1; then
    printf 'ERROR: symbolic link in target tree accepted\n' >&2; exit 1
fi
test -z "$(find "$TMP/outside" -mindepth 1 -print -quit)"
mkdir -p "$TMP/shared/usr/bin" "$TMP/shared/other-component"
ln -s "$TMP/outside" "$TMP/shared/other-component/runtime"
ln -s /unrelated/target "$TMP/shared/usr/bin/other-command"
make -s -C "$SOURCE" install BIN_DIR="$TMP/bin" DESTDIR="$TMP/shared"
test -x "$TMP/shared/usr/bin/cosh-cli"
test -L "$TMP/shared/usr/bin/other-command"
if AWPRJ= bash "$ROOT/packaging/raw/package.sh" stage >"$TMP/raw-error" 2>&1; then
    printf 'ERROR: incompatible raw namespace accepted\n' >&2; exit 1
fi
grep -Fq 'raw packages require AWPRJ=anolisa' "$TMP/raw-error"

# Root parsing must not invoke toolchains; explicit environment and CLI values
# remain distinct from legacy file-origin install defaults in mixed goals.
python3 - "$ROOT" "$TMP" <<'PYROOT'
import os
import subprocess
import sys
from pathlib import Path

root, work = map(Path, sys.argv[1:])
traps = work / "parse-traps"
traps.mkdir()
log = work / "unexpected-tools"
for tool in ("node", "npm", "cargo", "rustup"):
    script = traps / tool
    script.write_text('#!/bin/sh\nprintf "%s\\n" "$0" >> "$DELIVERY_TEST_LOG"\nexit 97\n')
    script.chmod(0o755)
env = {key: value for key, value in os.environ.items()
       if key not in {"AWPRJ", "PREFIX", "INSTALL_PROFILE", "BINDIR", "DATADIR"}}
env.update(PATH=f"{traps}:{os.environ['PATH']}", DELIVERY_TEST_LOG=str(log), INSTALL_PROFILE="system")
result = subprocess.run(["make", "-s", "-C", str(root), "layout"], env=env, text=True, capture_output=True)
assert result.returncode == 0, result.stderr
assert "PREFIX=/usr/local\n" in result.stdout, result.stdout
assert not log.exists(), log.read_text() if log.exists() else ""
env.update(AWPRJ="anolisa", PREFIX="/opt/environment", BINDIR="/opt/commands")
result = subprocess.run(["make", "-s", "-C", str(root), "layout", "AWPRJ="], env=env, text=True, capture_output=True)
assert result.returncode == 0, result.stderr
assert "AWPRJ=\n" in result.stdout and "PREFIX=/opt/environment" in result.stdout
assert "BINDIR=/opt/commands" in result.stdout
assert f"DATADIR=/opt/environment/share/{root.name}" in result.stdout
assert not log.exists()
if root.name == "tokenless":
    env.pop("INSTALL_PROFILE", None)
    for key in ("AWPRJ", "PREFIX", "BINDIR"):
        env.pop(key)
    target = "delivery-test-legacy:;@printf '%s\\n' 'LEGACY=$(INSTALL_PROFILE)|$(PREFIX)|$(AWPRJ)' 'DEFAULT=$(.DEFAULT_GOAL)'"
    probe = work / "legacy-probe.mk"
    probe.write_text(target + "\n")
    for goals in (("layout", "delivery-test-legacy"), ("delivery-test-legacy", "layout")):
        result = subprocess.run(["make", "-s", "-C", str(root), "-f", "Makefile", "-f", str(probe), *goals],
                                env=env, text=True, capture_output=True)
        assert result.returncode == 0, result.stderr
        assert f"LEGACY=user|{Path.home()}/.local|anolisa" in result.stdout, result.stdout
        data = "/usr/local/share" if os.geteuid() == 0 else (env.get("XDG_DATA_HOME") or str(Path.home() / ".local/share"))
        assert f"DATADIR={data}/tokenless" in result.stdout, result.stdout
        assert "DEFAULT=all" in result.stdout
    assert not log.exists()
PYROOT

test ! -e "$DELIVERY_TEST_LOG"

printf 'cosh-ng prebuilt install tests passed\n'

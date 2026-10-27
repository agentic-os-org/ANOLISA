#!/usr/bin/env bash
# Verify direct and DESTDIR file installation without builds or activation.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d /tmp/tokenless-system-staging.XXXXXX)"
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
    env -u AWPRJ -u PREFIX -u INSTALL_PROFILE make -s -C "$ROOT" layout "$@"
}
default_layout="$(layout INSTALL_PROFILE=system)"
grep -Fxq 'AWPRJ=' <<< "$default_layout"
grep -Fxq 'LIBEXECDIR=/usr/local/libexec/tokenless' <<< "$default_layout"
grep -Fxq 'DATADIR=/usr/local/share/tokenless' <<< "$default_layout"
grep -Fxq 'ADAPTERDIR=/usr/local/share/tokenless/adapters' <<< "$default_layout"
grep -Fxq 'EXTENSIONDIR=/usr/local/share/tokenless/cosh-extension' <<< "$default_layout"
distribution_layout="$(layout INSTALL_PROFILE=system AWPRJ=anolisa PREFIX=/opt/vendor/)"
grep -Fxq 'LIBEXECDIR=/opt/vendor/libexec/anolisa/tokenless' <<< "$distribution_layout"
grep -Fxq 'ADAPTERDIR=/opt/vendor/share/anolisa/adapters/tokenless' <<< "$distribution_layout"
grep -Fxq 'EXTENSIONDIR=/opt/vendor/share/anolisa/extensions/tokenless' <<< "$distribution_layout"
data_override_layout="$(layout INSTALL_PROFILE=system AWPRJ=anolisa DATADIR=/custom/data)"
grep -Fxq 'ADAPTERDIR=/custom/data/adapters' <<< "$data_override_layout"
grep -Fxq 'EXTENSIONDIR=/custom/data/cosh-extension' <<< "$data_override_layout"
mkdir -p "$TMP/user-home"
user_layout="$(HOME="$TMP/user-home" XDG_DATA_HOME="$TMP/xdg-data" layout INSTALL_PROFILE=user)"
grep -Fxq "PREFIX=$TMP/user-home/.local" <<< "$user_layout"
grep -Fxq "LIBEXECDIR=$TMP/user-home/.local/libexec/tokenless" <<< "$user_layout"
grep -Fxq "DATADIR=$TMP/xdg-data/tokenless" <<< "$user_layout"
fallback_layout="$(HOME="$TMP/user-home" XDG_DATA_HOME= layout INSTALL_PROFILE=user)"
grep -Fxq "DATADIR=$TMP/user-home/.local/share/tokenless" <<< "$fallback_layout"
XDG_DATA_HOME=relative layout INSTALL_PROFILE=system PREFIX=/usr >/dev/null
if HOME="$TMP/user-home" XDG_DATA_HOME=relative layout INSTALL_PROFILE=user >/dev/null 2>&1; then
    printf 'ERROR: relative XDG data root accepted\n' >&2; exit 1
fi
for invalid in '../other' 'a/b' '.hidden'; do
    if layout AWPRJ="$invalid" >/dev/null 2>&1; then
        printf 'ERROR: invalid namespace accepted: %s\n' "$invalid" >&2; exit 1
    fi
done
if layout INSTALL_PROFILE=unsupported >/dev/null 2>&1 || layout ADAPTERDIR=relative >/dev/null 2>&1; then
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
        grep -Fxq 'DATADIR=/usr/local/share/tokenless' <<< "$detected"
    else
        grep -Fxq 'INSTALL_PROFILE=user' <<< "$detected"
        grep -Fxq "PREFIX=$TMP/user-home/.local" <<< "$detected"
        grep -Fxq "DATADIR=$TMP/xdg-data/tokenless" <<< "$detected"
    fi
    system_override="$(PATH="$UID_TOOLS:$PATH" DELIVERY_TEST_UID="$uid" XDG_DATA_HOME=relative layout INSTALL_PROFILE=system PREFIX=/usr)"
    grep -Fxq 'DATADIR=/usr/share/tokenless' <<< "$system_override"
    user_override="$(PATH="$UID_TOOLS:$PATH" DELIVERY_TEST_UID="$uid" HOME="$TMP/user-home" XDG_DATA_HOME="$TMP/xdg-data" layout INSTALL_PROFILE=user PREFIX=/opt/programs)"
    grep -Fxq 'PREFIX=/opt/programs' <<< "$user_override"
    grep -Fxq "DATADIR=$TMP/xdg-data/tokenless" <<< "$user_override"
    direct_layout="$(env -u INSTALL_PROFILE -u PREFIX -u AWPRJ PATH="$UID_TOOLS:$PATH" DELIVERY_TEST_UID="$uid" HOME="$TMP/user-home" XDG_DATA_HOME="$TMP/xdg-data" bash "$ROOT/packaging/install.sh" --print-layout)"
    test "$direct_layout" = "$detected"
done

# Explicit system staging ignores user XDG roots.
export INSTALL_PROFILE=system PREFIX=/usr
SOURCE="$TMP/source"
ADAPTERS="$SOURCE/adapters/tokenless"
mkdir -p "$SOURCE" "$TMP/bin" "$ADAPTERS/common/hooks" "$ADAPTERS/common/commands" \
    "$ADAPTERS/openclaw/dist" "$ADAPTERS/openclaw/node_modules" "$ADAPTERS/dsh/dist" "$ADAPTERS/dsh/node_modules"
cp -R "$ROOT/packaging" "$SOURCE/packaging"
cp "$ROOT/Makefile" "$SOURCE/Makefile"
cp "$ROOT/Cargo.toml" "$SOURCE/Cargo.toml"
cp "$ROOT/LICENSE" "$SOURCE/LICENSE"
for binary in tokenless rtk; do
    printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/bin/$binary"
    chmod 0755 "$TMP/bin/$binary"
done
for relative in manifest.json common/cosh-extension.json common/tool-ready-spec.json; do
    printf '{}\n' > "$ADAPTERS/$relative"
done
printf 'export {};\n' > "$ADAPTERS/openclaw/dist/index.js"
printf 'export {};\n' > "$ADAPTERS/dsh/dist/index.js"
printf '#!/usr/bin/env bash\nexit 0\n' > "$ADAPTERS/common/tokenless-env-fix.sh"
printf 'print("fixture")\n' > "$ADAPTERS/common/hooks/rewrite_hook.py"
printf 'description="fixture"\n' > "$ADAPTERS/common/commands/tokenless-stats.toml"
printf '#!/usr/bin/env bash\nexit 0\n' > "$ADAPTERS/common/hooks/run-hook.sh"
mkdir -p "$ADAPTERS/claude-code/hooks" "$ADAPTERS/qwencode/hooks" "$ADAPTERS/qwenwork/hooks"
ln -s ../../common/hooks/run-hook.sh "$ADAPTERS/claude-code/hooks/run-hook.sh"
ln -s ../../common/hooks/run-hook.sh "$ADAPTERS/qwencode/hooks/run-hook.sh"
ln -s ../../common/hooks "$ADAPTERS/qwenwork/hooks/shared"
printf '{}\n' > "$ADAPTERS/openclaw/package-lock.json"
printf 'build template\n' > "$ADAPTERS/manifest.json.in"
# Exercise the included rules from an extracted, component-only source archive.
tar -cf "$TMP/source.tar" -C "$SOURCE" .
rm -rf "$SOURCE"
mkdir "$SOURCE"
tar -xf "$TMP/source.tar" -C "$SOURCE"
test -f "$SOURCE/packaging/anolisa/component.toml.in"
test -f "$SOURCE/Makefile"
test ! -e "$SOURCE/.anolisa"
make -s -C "$SOURCE" install \
    DESTDIR="$TMP/stage" BIN_DIR="$TMP/bin" RTK_BIN="$TMP/bin/rtk" PREFIX=/opt/vendor \
    BINDIR=/custom/bin LIBEXECDIR=/custom/runtime DATADIR=/custom/data \
    ADAPTERDIR=/custom/adapters EXTENSIONDIR=/custom/cosh-extension
test -x "$TMP/stage/custom/bin/tokenless"
test -x "$TMP/stage/custom/runtime/rtk"
cmp "$SOURCE/LICENSE" "$TMP/stage/custom/data/LICENSE"
test "$(readlink "$TMP/stage/custom/bin/rtk")" = /custom/runtime/rtk
test -f "$TMP/stage/custom/adapters/openclaw/dist/index.js"
test -x "$TMP/stage/custom/cosh-extension/hooks/rewrite_hook.py"
test ! -e "$TMP/stage/custom/adapters/openclaw/node_modules"
test ! -e "$TMP/stage/custom/adapters/dsh/node_modules"
test ! -e "$TMP/stage/custom/adapters/openclaw/package-lock.json"
test ! -e "$TMP/stage/custom/adapters/manifest.json.in"
test ! -e "$SOURCE/target/packaging/anolisa/component.toml"
make -s -C "$SOURCE" generate-component-contract
test -f "$SOURCE/target/packaging/anolisa/component.toml"
test ! -e "$SOURCE/.anolisa"
make -s -C "$SOURCE" install \
    DESTDIR="$TMP/distribution" BIN_DIR="$TMP/bin" RTK_BIN="$TMP/bin/rtk" PREFIX=/opt/vendor AWPRJ=anolisa
test -x "$TMP/distribution/opt/vendor/libexec/anolisa/tokenless/rtk"
cmp "$SOURCE/LICENSE" "$TMP/distribution/opt/vendor/share/anolisa/tokenless/LICENSE"
test -f "$TMP/distribution/opt/vendor/share/anolisa/adapters/tokenless/manifest.json"
test -f "$TMP/distribution/opt/vendor/share/anolisa/extensions/tokenless/cosh-extension.json"
make -s -j4 -C "$SOURCE" install \
    DESTDIR="$TMP/parallel" BIN_DIR="$TMP/bin" RTK_BIN="$TMP/bin/rtk" PREFIX=/opt/vendor AWPRJ=parallel
test "$(readlink "$TMP/parallel/opt/vendor/bin/rtk")" = /opt/vendor/libexec/parallel/tokenless/rtk
test -f "$TMP/parallel/opt/vendor/share/parallel/adapters/tokenless/manifest.json"
test -f "$TMP/parallel/opt/vendor/share/parallel/extensions/tokenless/cosh-extension.json"
make -s -C "$SOURCE" install BIN_DIR="$TMP/bin" RTK_BIN="$TMP/bin/rtk" PREFIX="$TMP/direct"
test -x "$TMP/direct/bin/tokenless"
test "$(readlink "$TMP/direct/bin/rtk")" = "$TMP/direct/libexec/tokenless/rtk"
test -f "$TMP/direct/share/tokenless/adapters/manifest.json"
test ! -L "$TMP/direct/share/tokenless/adapters/claude-code/hooks/run-hook.sh"
test ! -L "$TMP/direct/share/tokenless/adapters/qwenwork/hooks/shared"
make -s -C "$SOURCE" install BIN_DIR="$TMP/bin" RTK_BIN="$TMP/bin/rtk" PREFIX="$TMP/direct"
make -s -C "$SOURCE" install BIN_DIR="$TMP/bin" RTK_BIN="$TMP/bin/rtk" PREFIX="$TMP/namespaced" AWPRJ=anolisa
test -x "$TMP/namespaced/libexec/anolisa/tokenless/rtk"
test -f "$TMP/namespaced/share/anolisa/adapters/tokenless/manifest.json"

for target in stage-package stage-package-core stage-package-plugin stage-package-skills install-skills; do
    if make -s -n -C "$SOURCE" "$target" >/dev/null 2>&1; then
        printf 'ERROR: unsupported install/staging target exists: %s\n' "$target" >&2; exit 1
    fi
done
# Core installation must work without any built framework bundles.
# User installation separates executable and XDG resource roots.
make -s -C "$SOURCE" install INSTALL_PROFILE=user PREFIX="$TMP/user-prefix" \
    BIN_DIR="$TMP/bin" RTK_BIN="$TMP/bin/rtk" HOME="$TMP/user-home" XDG_DATA_HOME="$TMP/user-data"
test -x "$TMP/user-prefix/bin/tokenless"
for uid in 0 1001; do
    stage="$TMP/uid-install-$uid"
    env -u INSTALL_PROFILE -u PREFIX PATH="$UID_TOOLS:$PATH" DELIVERY_TEST_UID="$uid" \
        HOME="$TMP/user-home" XDG_DATA_HOME="$TMP/xdg-data" \
        make -s -C "$SOURCE" install-core BIN_DIR="$TMP/bin" RTK_BIN="$TMP/bin/rtk" DESTDIR="$stage"
    if [ "$uid" = 0 ]; then
        test -x "$stage/usr/local/bin/tokenless"
    else
        test -x "$stage$TMP/user-home/.local/bin/tokenless"
    fi
done
test -L "$TMP/user-prefix/bin/rtk"
test -f "$TMP/user-data/tokenless/adapters/common/tool-ready-spec.json"
test -f "$TMP/user-data/tokenless/cosh-extension/cosh-extension.json"
mv "$ADAPTERS/openclaw/dist/index.js" "$TMP/group-plugin.saved"
make -s -C "$SOURCE" install-core BIN_DIR="$TMP/bin" RTK_BIN="$TMP/bin/rtk" DESTDIR="$TMP/core-only"
test -x "$TMP/core-only/usr/bin/tokenless"
test -x "$TMP/core-only/usr/libexec/tokenless/rtk"
test ! -e "$TMP/core-only/usr/share/tokenless/adapters"
test ! -e "$TMP/core-only/usr/share/tokenless/cosh-extension"
mv "$TMP/group-plugin.saved" "$ADAPTERS/openclaw/dist/index.js"
mv "$TMP/bin" "$TMP/group-binaries.saved"
make -s -C "$SOURCE" install-plugins BIN_DIR="$TMP/bin" RTK_BIN="$TMP/bin/rtk" DESTDIR="$TMP/plugins-only"
test -f "$TMP/plugins-only/usr/share/tokenless/adapters/openclaw/dist/index.js"
test -f "$TMP/plugins-only/usr/share/tokenless/cosh-extension/cosh-extension.json"
test ! -e "$TMP/plugins-only/usr/bin"
test ! -e "$TMP/plugins-only/usr/libexec"
mv "$TMP/group-binaries.saved" "$TMP/bin"
make -s -j4 -C "$SOURCE" install-core install-plugins BIN_DIR="$TMP/bin" RTK_BIN="$TMP/bin/rtk" DESTDIR="$TMP/group-union" AWPRJ=example
make -s -C "$SOURCE" install BIN_DIR="$TMP/bin" RTK_BIN="$TMP/bin/rtk" DESTDIR="$TMP/group-all" AWPRJ=example
python3 - "$TMP/group-union" "$TMP/group-all" <<'PYGROUP'
from pathlib import Path
import sys

def inventory(root):
    return {str(p.relative_to(root)): (p.readlink().as_posix() if p.is_symlink() else p.read_bytes(), p.lstat().st_mode & 0o777)
            for p in root.rglob('*') if p.is_file() or p.is_symlink()}
assert inventory(Path(sys.argv[1])) == inventory(Path(sys.argv[2]))
PYGROUP
mv "$TMP/bin/rtk" "$TMP/bin/rtk.saved"
if make -s -C "$SOURCE" install BIN_DIR="$TMP/bin" RTK_BIN="$TMP/bin/rtk" DESTDIR="$TMP/missing-binary" >/dev/null 2>&1; then
    printf 'ERROR: install accepted a missing binary\n' >&2; exit 1
fi
test ! -e "$TMP/missing-binary"
mv "$TMP/bin/rtk.saved" "$TMP/bin/rtk"
mv "$ADAPTERS/openclaw/dist/index.js" "$TMP/plugin.saved"
if make -s -C "$SOURCE" install BIN_DIR="$TMP/bin" RTK_BIN="$TMP/bin/rtk" DESTDIR="$TMP/missing-plugin" >/dev/null 2>&1; then
    printf 'ERROR: install accepted a missing plugin input\n' >&2; exit 1
fi
test ! -e "$TMP/missing-plugin"
mv "$TMP/plugin.saved" "$ADAPTERS/openclaw/dist/index.js"
mkdir -p "$TMP/outside"
ln -s "$TMP/outside" "$TMP/link"
for destination in "$TMP/link" "$TMP/link/child" "$TMP/../escape" relative / //; do
    if make -s -C "$SOURCE" install BIN_DIR="$TMP/bin" RTK_BIN="$TMP/bin/rtk" DESTDIR="$destination" >/dev/null 2>&1; then
        printf 'ERROR: unsafe staging root accepted: %s\n' "$destination" >&2; exit 1
    fi
done
mkdir -p "$TMP/unsafe/usr/share/tokenless/adapters"
ln -s "$TMP/outside" "$TMP/unsafe/usr/share/tokenless/adapters/common"
if make -s -C "$SOURCE" install BIN_DIR="$TMP/bin" RTK_BIN="$TMP/bin/rtk" DESTDIR="$TMP/unsafe" >/dev/null 2>&1; then
    printf 'ERROR: symbolic link in target tree accepted\n' >&2; exit 1
fi
test -z "$(find "$TMP/outside" -mindepth 1 -print -quit)"
mkdir -p "$TMP/shared/usr/bin" "$TMP/shared/other-component"
ln -s "$TMP/outside" "$TMP/shared/other-component/runtime"
ln -s /unrelated/target "$TMP/shared/usr/bin/other-command"
make -s -C "$SOURCE" install BIN_DIR="$TMP/bin" RTK_BIN="$TMP/bin/rtk" DESTDIR="$TMP/shared"
test -x "$TMP/shared/usr/bin/tokenless"
test -L "$TMP/shared/usr/bin/other-command"
if AWPRJ= bash "$ROOT/packaging/raw/package.sh" stage >"$TMP/raw-error" 2>&1; then
    printf 'ERROR: incompatible raw namespace accepted\n' >&2; exit 1
fi
grep -Fq 'raw packages require AWPRJ=anolisa' "$TMP/raw-error"

# The old auto-building user installation is explicit; unified install never
# reaches these recipes, and setup keeps its historical host-registration flow.
legacy_plan="$(make -s -n -C "$ROOT" install-legacy INSTALL_PROFILE=user PREFIX="$TMP/legacy")"
grep -Fq 'cargo build --release' <<< "$legacy_plan"
grep -Fq "install -p -m 0755 target/release/tokenless $TMP/legacy/bin/" <<< "$legacy_plan"
grep -Fxq 'setup: install-legacy adapter-install' "$ROOT/Makefile"

# Model the two guarded RPM ownership entries without requiring rpmbuild.
python3 - "$ROOT/packaging/rpm/tokenless.spec.in" <<'PY'
import re
import sys
from pathlib import Path

section = Path(sys.argv[1]).read_text().split("%files\n", 1)[1].split("\n%post", 1)[0]

def directory_entries(adapter: str, extension: str) -> set[str]:
    macros = {
        "_datadir": "/usr/share",
        "_adapterdir_tokenless": adapter,
        "_extensiondir_tokenless": extension,
    }
    entries = set()
    active = True
    for line in section.splitlines():
        expanded = re.sub(r"%\{([^}]+)\}", lambda match: macros.get(match[1], match[0]), line)
        if expanded.startswith("%if "):
            comparison = re.fullmatch(r'%if "([^"]*)" == "([^"]*)"', expanded)
            assert comparison, expanded
            active = comparison[1] == comparison[2]
        elif expanded == "%endif":
            active = True
        elif active and expanded.startswith("%dir "):
            entries.add(expanded.split()[-1])
    return entries

legacy = directory_entries("/usr/share/anolisa/adapters/tokenless", "/usr/share/anolisa/extensions/tokenless")
assert "/usr/share/anolisa/adapters" in legacy
assert "/usr/share/anolisa/extensions" in legacy
custom = directory_entries("/opt/tokenless/adapters", "/opt/tokenless/cosh-extension")
assert "/usr/share/anolisa/adapters" not in custom
assert "/usr/share/anolisa/extensions" not in custom
assert "/opt/tokenless/adapters" in custom
assert "/opt/tokenless/cosh-extension" in custom
PY

# Root parsing must not invoke toolchains; explicit environment and CLI values
# remain distinct from legacy file-origin install defaults in mixed goals.
python3 - "$ROOT" "$TMP" <<'PYROOT'
import os
import subprocess
import sys
from pathlib import Path

root, work = map(Path, sys.argv[1:])
if root.name == "tokenless":
    import json
    assert json.loads((root / "packaging/npm/package.json").read_text())["type"] == "module"
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

printf 'tokenless prebuilt install tests passed\n'

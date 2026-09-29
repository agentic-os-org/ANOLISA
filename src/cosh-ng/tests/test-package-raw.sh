#!/usr/bin/env bash
# Exercise the component-owned raw packer without compiling native binaries.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d /tmp/cosh-ng-raw-package-test.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

SOURCE="$TMP/cosh-ng"
LINUX_CONTRACT="$SOURCE/.anolisa/component.toml"
MACOS_CONTRACT="$SOURCE/.anolisa/component.macos.toml"
VERSION="$(awk -F'"' '/^version = / { print $2; exit }' "$ROOT/Cargo.toml")"

install -d -m 0755 \
    "$SOURCE/.anolisa" \
    "$SOURCE/packaging/systemd" \
    "$SOURCE/skills/manage-task-checkpoints"
install -p -m 0644 "$ROOT/.anolisa/component.toml" "$LINUX_CONTRACT"
install -p -m 0644 "$ROOT/.anolisa/component.macos.toml" "$MACOS_CONTRACT"
install -p -m 0644 "$ROOT/Cargo.toml" "$SOURCE/Cargo.toml"
install -p -m 0644 "$ROOT/LICENSE" "$SOURCE/LICENSE"
install -p -m 0644 "$ROOT/README.md" "$SOURCE/README.md"
install -p -m 0644 "$ROOT/packaging/systemd/cosh-gateway@.service.in" \
    "$SOURCE/packaging/systemd/cosh-gateway@.service.in"
install -p -m 0644 "$ROOT/skills/manage-task-checkpoints/SKILL.md" \
    "$SOURCE/skills/manage-task-checkpoints/SKILL.md"

test_rpm_systemd_unit_render() {
    local rpm_libexec rpm_libexec_cosh rpm_libexec_cosh_macro rendered_unit

    rpm_libexec=/usr/libexec
    rpm_libexec_cosh_macro="$(
        awk '
            $1 == "%global" && $2 == "_libexecdir_cosh" {
                sub(/^%global[[:space:]]+_libexecdir_cosh[[:space:]]+/, "")
                print
                exit
            }
        ' "$ROOT/cosh-ng.spec.in"
    )"
    rpm_libexec_cosh="$(
        printf '%s\n' "$rpm_libexec_cosh_macro" |
            sed "s|%{_libexecdir}|$rpm_libexec|g"
    )"
    rendered_unit="$TMP/cosh-gateway@.service.rpm-rendered"

    if [ -z "$rpm_libexec_cosh_macro" ] ||
        [[ "$rpm_libexec_cosh" == *'%{'* ]]; then
        echo "ERROR: cannot resolve the RPM cosh libexec macro" >&2
        exit 1
    fi
    grep -Fq "s|{libexecdir}/cosh-ng|%{_libexecdir_cosh}|g" \
        "$ROOT/cosh-ng.spec.in" || {
        echo "ERROR: RPM unit render does not use the cosh libexec macro" >&2
        exit 1
    }
    sed "s|{libexecdir}/cosh-ng|$rpm_libexec_cosh|g" \
        "$ROOT/packaging/systemd/cosh-gateway@.service.in" \
        > "$rendered_unit"
    grep -Fqx \
        "ExecStart=\"$rpm_libexec_cosh/cosh-gateway\" serve --systemd-unit=cosh-gateway@%i.service --socket=/run/cosh-gateway-%i/gateway.sock --database=/var/lib/cosh-gateway-%i/gateway.sqlite --core-executable=\"$rpm_libexec_cosh/cosh-core\" --workspace=\${COSH_GATEWAY_WORKSPACE} \$COSH_GATEWAY_ACP_ARG \$COSH_GATEWAY_CHECKPOINT_ARG \$COSH_GATEWAY_SECURITY_AUDIT_ARG" \
        "$rendered_unit" || {
        echo "ERROR: rendered RPM Gateway unit paths do not match libexec installation" >&2
        exit 1
    }
}

test_rpm_systemd_unit_render

make_binaries() {
    local os="$1"
    local arch="$2"
    local destination="$3"

    install -d -m 0755 "$destination"
    python3 - "$os" "$arch" "$destination" "$VERSION" <<'PY'
import hashlib
import json
import pathlib
import struct
import sys

os_name, arch, destination, version = sys.argv[1:]
if os_name == "linux":
    machine = {"x86_64": 62, "aarch64": 183}[arch]
    header = bytearray(64)
    header[:6] = b"\x7fELF\x02\x01"
    struct.pack_into("<H", header, 16, 2)
    struct.pack_into("<H", header, 18, machine)
    struct.pack_into("<I", header, 20, 1)
    content = bytes(header)
else:
    cpu = {"aarch64": 0x0100000C, "x86_64": 0x01000007}[arch]
    content = struct.pack("<IiiIIIII", 0xFEEDFACF, cpu, 0, 2, 0, 0, 0, 0)
for name in ("cosh-cli", "cosh-core", "cosh-gateway", "cosh-shell"):
    (pathlib.Path(destination) / name).write_bytes(content)
digest = hashlib.sha256(content).hexdigest()
metadata = [
    f"version = {json.dumps(version)}",
    f"target_os = {json.dumps(os_name)}",
    f"target_arch = {json.dumps(arch)}",
    "",
    "[binaries]",
]
metadata.extend(
    f'{name} = "{digest}"'
    for name in ("cosh-cli", "cosh-core", "cosh-gateway", "cosh-shell")
)
(pathlib.Path(destination) / "cosh-ng-build.toml").write_text(
    "\n".join(metadata) + "\n",
    encoding="utf-8",
)
PY
    chmod 0755 \
        "$destination/cosh-cli" \
        "$destination/cosh-core" \
        "$destination/cosh-gateway" \
        "$destination/cosh-shell"
}

LINUX_X64="$TMP/bin-linux-x86_64"
LINUX_ARM64="$TMP/bin-linux-aarch64"
MACOS_ARM64="$TMP/bin-macos-aarch64"
MACOS_X64="$TMP/bin-macos-x86_64"
make_binaries linux x86_64 "$LINUX_X64"
make_binaries linux aarch64 "$LINUX_ARM64"
make_binaries macos aarch64 "$MACOS_ARM64"
make_binaries macos x86_64 "$MACOS_X64"

run_pack() {
    local os="$1"
    local arch="$2"
    local binaries="$3"
    local output="$4"

    COSH_NG_SOURCE_DIR="$SOURCE" \
    BIN_DIR="$binaries" \
    TARGET_OS="$os" \
    TARGET_ARCH="$arch" \
    OUTPUT_DIR="$output" \
    SOURCE_DATE_EPOCH=1700000000 \
        "$ROOT/packaging/raw/package.sh" package >/dev/null
}

OUT_ONE="$TMP/out-one"
OUT_TWO="$TMP/out-two"
run_pack linux x64 "$LINUX_X64" "$OUT_ONE"
run_pack linux x86_64 "$LINUX_X64" "$OUT_TWO"
X64_ARTIFACT="cosh-ng-$VERSION-linux-x86_64.tar.gz"
cmp "$OUT_ONE/$X64_ARTIFACT" "$OUT_TWO/$X64_ARTIFACT"

run_pack linux arm64 "$LINUX_ARM64" "$TMP/out-linux-arm64"
test -f "$TMP/out-linux-arm64/cosh-ng-$VERSION-linux-aarch64.tar.gz"
run_pack darwin arm64 "$MACOS_ARM64" "$TMP/out-macos-arm64"
MACOS_ARTIFACT="cosh-ng-$VERSION-macos-aarch64.tar.gz"
test -f "$TMP/out-macos-arm64/$MACOS_ARTIFACT"

run_pack macos x64 "$MACOS_X64" "$TMP/out-macos-x64"
run_pack darwin x86_64 "$MACOS_X64" "$TMP/out-macos-x64-repeat"
MACOS_X64_ARTIFACT="cosh-ng-$VERSION-macos-x86_64.tar.gz"
cmp "$TMP/out-macos-x64/$MACOS_X64_ARTIFACT" \
    "$TMP/out-macos-x64-repeat/$MACOS_X64_ARTIFACT"
if run_pack macos x64 "$MACOS_ARM64" "$TMP/wrong-macos-arch" 2>/dev/null; then
    echo "ERROR: arm64 binaries unexpectedly packaged as macOS x86_64" >&2
    exit 1
fi
if run_pack macos arm64 "$MACOS_X64" "$TMP/wrong-macos-arm64" 2>/dev/null; then
    echo "ERROR: x86_64 binaries unexpectedly packaged as macOS arm64" >&2
    exit 1
fi
if run_pack macos x64 "$LINUX_X64" "$TMP/wrong-macos-format" 2>/dev/null; then
    echo "ERROR: ELF binaries unexpectedly packaged as macOS x86_64" >&2
    exit 1
fi
python3 "$ROOT/packaging/raw/verify-binaries.py" \
    --os linux --arch aarch64 "$LINUX_ARM64/cosh-cli" >/dev/null
python3 "$ROOT/packaging/raw/verify-binaries.py" \
    --os macos --arch aarch64 "$MACOS_ARM64/cosh-cli" >/dev/null
if python3 "$ROOT/packaging/raw/verify-binaries.py" \
    --os linux --arch aarch64 "$LINUX_X64/cosh-cli" >/dev/null 2>&1; then
    echo "ERROR: mislabeled x86_64 binary unexpectedly passed as aarch64" >&2
    exit 1
fi
if python3 "$ROOT/packaging/raw/verify-binaries.py" \
    --os macos --arch aarch64 "$LINUX_ARM64/cosh-cli" >/dev/null 2>&1; then
    echo "ERROR: ELF binary unexpectedly passed as Mach-O" >&2
    exit 1
fi

OPENSSL_LINKED="$TMP/openssl-linked-cosh-core"
python3 - "$OPENSSL_LINKED" <<'PY'
import struct
import sys

# Minimal ELF carrying DT_NEEDED libssl.so.1.1, used to verify the
# packaging validator rejects any OpenSSL-linked binary.
strings = b"\0libssl.so.1.1\0"
dynamic = struct.pack("<qQqQqQ", 1, 1, 5, 0x1000, 0, 0)
vaddr = 0x1000
strtab_offset = 0x1000
dynamic_offset = strtab_offset + len(strings)

header = bytearray(64)
header[:6] = b"\x7fELF\x02\x01"
struct.pack_into("<H", header, 16, 2)
struct.pack_into("<H", header, 18, 62)
struct.pack_into("<I", header, 20, 1)
struct.pack_into("<Q", header, 32, 64)
struct.pack_into("<HH", header, 54, 56, 2)

load = struct.pack(
    "<II6Q", 1, 4, strtab_offset, vaddr, vaddr, len(strings), len(strings), 0x1000
)
dyn = struct.pack(
    "<II6Q", 2, 4, dynamic_offset, 0x2000, 0x2000, len(dynamic), len(dynamic), 8
)

blob = bytearray(header + load + dyn)
blob.extend(b"\0" * (strtab_offset - len(blob)))
blob.extend(strings)
blob.extend(dynamic)
open(sys.argv[1], "wb").write(bytes(blob))
PY
if python3 "$ROOT/packaging/raw/verify-binaries.py" \
    --os linux --arch x86_64 "$OPENSSL_LINKED" >/dev/null 2>&1; then
    echo "ERROR: binary dynamically linking OpenSSL was accepted" >&2
    exit 1
fi

EXTRA_BUILD="$TMP/extra-build.toml"
install -p -m 0644 "$LINUX_ARM64/cosh-ng-build.toml" "$EXTRA_BUILD"
printf 'unexpected-tool = "%064d"\n' 0 >> "$EXTRA_BUILD"
if python3 "$ROOT/packaging/raw/verify-binaries.py" \
    --os linux \
    --arch aarch64 \
    --metadata "$EXTRA_BUILD" \
    --component-version "$VERSION" \
    "$LINUX_ARM64/cosh-cli" \
    "$LINUX_ARM64/cosh-core" \
    "$LINUX_ARM64/cosh-gateway" \
    "$LINUX_ARM64/cosh-shell" >/dev/null 2>&1; then
    echo "ERROR: unexpected build metadata entry was accepted" >&2
    exit 1
fi

test_native_without_metadata() {
    local host_arch native_version native_source native_bins native_stage python_bin

    [ "$(uname -s)" = Linux ] || return
    case "$(uname -m)" in
        x86_64 | amd64) host_arch=x86_64 ;;
        aarch64 | arm64) host_arch=aarch64 ;;
        *) return ;;
    esac

    python_bin="$(command -v python3)"
    native_version="$("$python_bin" --version 2>&1 | awk 'NR == 1 { print $NF; exit }')"
    native_source="$TMP/native-source"
    native_bins="$TMP/native-bins"
    native_stage="$TMP/native-stage"
    install -d -m 0755 \
        "$native_source/.anolisa" \
        "$native_source/packaging/systemd" \
        "$native_source/skills/manage-task-checkpoints" \
        "$native_bins"
    sed "0,/version = \"$VERSION\"/s//version = \"$native_version\"/" \
        "$ROOT/Cargo.toml" > "$native_source/Cargo.toml"
    sed "0,/version = \"$VERSION\"/s//version = \"$native_version\"/" \
        "$ROOT/.anolisa/component.toml" > "$native_source/.anolisa/component.toml"
    install -p -m 0644 "$ROOT/LICENSE" "$native_source/LICENSE"
    install -p -m 0644 "$ROOT/README.md" "$native_source/README.md"
    install -p -m 0644 "$ROOT/packaging/systemd/cosh-gateway@.service.in" \
        "$native_source/packaging/systemd/cosh-gateway@.service.in"
    install -p -m 0644 "$ROOT/skills/manage-task-checkpoints/SKILL.md" \
        "$native_source/skills/manage-task-checkpoints/SKILL.md"
    for binary in cosh-cli cosh-core cosh-gateway cosh-shell; do
        install -p -m 0755 "$python_bin" "$native_bins/$binary"
    done

    COSH_NG_SOURCE_DIR="$native_source" \
    BIN_DIR="$native_bins" \
    TARGET_OS=linux \
    TARGET_ARCH="$host_arch" \
    DESTDIR="$native_stage" \
        "$ROOT/packaging/raw/package.sh" stage >/dev/null
}

test_native_without_metadata

BUILD_ALL_UNINSTALL_OUTPUT="$TMP/build-all-uninstall.out"
"$ROOT/../../scripts/build-all.sh" \
    --uninstall --component cosh-ng --system --dry-run \
    > "$BUILD_ALL_UNINSTALL_OUTPUT"
grep -Fxq 'DRY-RUN: systemctl stop cosh-gateway@*.service' \
    "$BUILD_ALL_UNINSTALL_OUTPUT"
grep -Fxq 'DRY-RUN: systemctl disable cosh-gateway@*.service' \
    "$BUILD_ALL_UNINSTALL_OUTPUT"
grep -Fxq 'DRY-RUN: systemctl stop cosh-gateway-acp@*.service' \
    "$BUILD_ALL_UNINSTALL_OUTPUT"
grep -Fxq 'DRY-RUN: systemctl disable cosh-gateway-acp@*.service' \
    "$BUILD_ALL_UNINSTALL_OUTPUT"

STAGED="$TMP/staged"
COSH_NG_SOURCE_DIR="$SOURCE" \
BIN_DIR="$LINUX_X64" \
TARGET_OS=linux \
TARGET_ARCH=x86_64 \
DESTDIR="$STAGED" \
    "$ROOT/packaging/raw/package.sh" stage >/dev/null

install -d -m 0755 "$TMP/not-empty"
printf 'occupied\n' > "$TMP/not-empty/file"
if COSH_NG_SOURCE_DIR="$SOURCE" \
    RAW_CONTRACT="$LINUX_CONTRACT" \
    BIN_DIR="$LINUX_X64" \
    TARGET_OS=linux \
    TARGET_ARCH=x86_64 \
    DESTDIR="$TMP/not-empty" \
        "$ROOT/packaging/raw/package.sh" stage >/dev/null 2>&1; then
    echo "ERROR: non-empty DESTDIR unexpectedly succeeded" >&2
    exit 1
fi

MISMATCH="$TMP/mismatched-component.toml"
sed "0,/version = \"$VERSION\"/s//version = \"9.8.7\"/" \
    "$LINUX_CONTRACT" > "$MISMATCH"
if COSH_NG_SOURCE_DIR="$SOURCE" \
    RAW_CONTRACT="$MISMATCH" \
    BIN_DIR="$LINUX_X64" \
    TARGET_OS=linux \
    TARGET_ARCH=x86_64 \
    DESTDIR="$TMP/mismatch-stage" \
        "$ROOT/packaging/raw/package.sh" stage >/dev/null 2>&1; then
    echo "ERROR: mismatched contract version unexpectedly succeeded" >&2
    exit 1
fi

MISMATCHED_BUILD="$TMP/mismatched-build.toml"
sed "0,/version = \"$VERSION\"/s//version = \"9.8.7\"/" \
    "$LINUX_ARM64/cosh-ng-build.toml" > "$MISMATCHED_BUILD"
if COSH_NG_SOURCE_DIR="$SOURCE" \
    COSH_NG_BUILD_METADATA="$MISMATCHED_BUILD" \
    BIN_DIR="$LINUX_ARM64" \
    TARGET_OS=linux \
    TARGET_ARCH=aarch64 \
    DESTDIR="$TMP/mismatched-build-stage" \
        "$ROOT/packaging/raw/package.sh" stage >/dev/null 2>&1; then
    echo "ERROR: mismatched cross-target build version unexpectedly succeeded" >&2
    exit 1
fi

EXTRACTED="$TMP/extracted"
install -d -m 0755 "$EXTRACTED"
tar --same-permissions -xzf "$OUT_ONE/$X64_ARTIFACT" -C "$EXTRACTED"
cmp "$LINUX_CONTRACT" "$EXTRACTED/.anolisa/component.toml"
cmp "$LINUX_X64/cosh-cli" "$EXTRACTED/bin/cosh-cli"
cmp "$LINUX_X64/cosh-core" "$EXTRACTED/libexec/anolisa/cosh-ng/cosh-core"
cmp "$LINUX_X64/cosh-gateway" "$EXTRACTED/libexec/anolisa/cosh-ng/cosh-gateway"
cmp "$LINUX_X64/cosh-shell" "$EXTRACTED/libexec/anolisa/cosh-ng/cosh-shell"
cmp "$ROOT/packaging/systemd/cosh-gateway@.service.in" \
    "$EXTRACTED/share/anolisa/cosh-ng/cosh-gateway@.service.in"
cmp "$ROOT/skills/manage-task-checkpoints/SKILL.md" \
    "$EXTRACTED/share/anolisa/skills/manage-task-checkpoints/SKILL.md"
test ! -e "$EXTRACTED/share/anolisa/cosh-ng/cosh-gateway-acp@.service.in"
grep -Fqx 'EnvironmentFile=/etc/cosh/gateway-%i.env' \
    "$EXTRACTED/share/anolisa/cosh-ng/cosh-gateway@.service.in"
if grep -Fq 'Environment=COSH_GATEWAY_WORKSPACE=' \
    "$EXTRACTED/share/anolisa/cosh-ng/cosh-gateway@.service.in"; then
    echo "ERROR: packaged Gateway unit must require an explicit workspace" >&2
    exit 1
fi
grep -Fqx 'Conflicts=cosh-gateway-acp@%i.service' \
    "$EXTRACTED/share/anolisa/cosh-ng/cosh-gateway@.service.in"
if grep -Fq 'ws-ckpt.service' \
    "$EXTRACTED/share/anolisa/cosh-ng/cosh-gateway@.service.in"; then
    echo "ERROR: packaged Gateway unit depends on ws-ckpt" >&2
    exit 1
fi
if grep -Fq -- '--checkpoint-socket=' \
    "$EXTRACTED/share/anolisa/cosh-ng/cosh-gateway@.service.in"; then
    echo "ERROR: packaged Gateway unit configures a checkpoint socket" >&2
    exit 1
fi
if grep -Fq -- '--security-audit=' \
    "$EXTRACTED/share/anolisa/cosh-ng/cosh-gateway@.service.in"; then
    echo "ERROR: packaged Gateway unit configures checkpoint audit" >&2
    exit 1
fi
if grep -Fq 'packaging/systemd/cosh-gateway-acp@.service.in' \
    "$ROOT/cosh-ng.spec.in" || \
    grep -Fxq '%{_unitdir}/cosh-gateway-acp@.service' \
        "$ROOT/cosh-ng.spec.in"; then
    echo "ERROR: RPM spec still ships the retired split ACP Gateway unit" >&2
    exit 1
fi
cmp "$ROOT/LICENSE" "$EXTRACTED/share/doc/cosh-ng/LICENSE"
cmp "$ROOT/README.md" "$EXTRACTED/share/doc/cosh-ng/README.md"
test -z "$(find "$EXTRACTED" -type l -print -quit)"
file_mode() {
    stat -c '%a' "$1" 2>/dev/null || stat -f '%Lp' "$1"
}
test "$(file_mode "$EXTRACTED/bin/cosh")" = 755
test "$(file_mode "$EXTRACTED/libexec/anolisa/cosh-ng/cosh-gateway")" = 755
test "$(file_mode "$EXTRACTED/libexec/anolisa/cosh-ng/cosh-shell")" = 755
test "$(file_mode "$EXTRACTED/share/anolisa/cosh-ng/cosh-gateway@.service.in")" = 644
test "$(file_mode "$EXTRACTED/share/anolisa/skills/manage-task-checkpoints/SKILL.md")" = 644
test "$(file_mode "$EXTRACTED/share/doc/cosh-ng/README.md")" = 644
test ! -e "$EXTRACTED/share/anolisa/hooks"
cmp "$STAGED/bin/cosh" "$EXTRACTED/bin/cosh"

cat > "$EXTRACTED/libexec/anolisa/cosh-ng/cosh-shell" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$#" "$@"
EOF
chmod 0755 "$EXTRACTED/libexec/anolisa/cosh-ng/cosh-shell"
cat > "$EXTRACTED/libexec/anolisa/cosh-ng/cosh-gateway" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$#" "$@"
EOF
chmod 0755 "$EXTRACTED/libexec/anolisa/cosh-ng/cosh-gateway"
test "$("$EXTRACTED/bin/cosh" --version)" = $'1\n--version'
test "$("$EXTRACTED/bin/cosh" agent doctor --profile codex)" = \
    $'4\nagent\ndoctor\n--profile\ncodex'
READLINK_STUB="$TMP/readlink-without-f"
install -d -m 0755 "$READLINK_STUB"
install -p -m 0755 /bin/false "$READLINK_STUB/readlink"
test "$(PATH="$READLINK_STUB:/usr/bin:/bin" \
    "$EXTRACTED/bin/cosh" --version)" = $'1\n--version'
LINKED_BIN="$TMP/linked-bin"
install -d -m 0755 "$LINKED_BIN"
ln -s "$EXTRACTED/bin/cosh" "$LINKED_BIN/cosh"
test "$(bash "$LINKED_BIN/cosh" --version)" = $'1\n--version'
# The wrapper forwards argv untouched (no injected raw/adapter prefix);
# dispatch belongs to cosh-shell's invocation classifier.
test "$("$EXTRACTED/bin/cosh" prompt)" = $'1\nprompt'
test "$(printf '' | "$EXTRACTED/bin/cosh")" = "0"

NO_RPM_OUTPUT="$TMP/cosh-switch-no-rpm.out"
if PATH="$TMP/no-rpm-path" "$BASH" \
    "$EXTRACTED/bin/cosh-switch" >"$NO_RPM_OUTPUT" 2>&1; then
    echo "ERROR: cosh-switch unexpectedly succeeded without RPM ownership" >&2
    exit 1
else
    test "$?" -eq 1
fi
grep -Fq 'only supported for RPM-managed cosh-ng/copilot-shell' "$NO_RPM_OUTPUT"
grep -Fq 'switch components with anolisa install/uninstall' "$NO_RPM_OUTPUT"

python3 "$ROOT/packaging/raw/verify-release.py" \
    "$SOURCE" "$LINUX_CONTRACT" --os linux --arch x86_64 >/dev/null
python3 "$ROOT/packaging/raw/verify-release.py" \
    "$SOURCE" "$MACOS_CONTRACT" --os macos --arch aarch64 >/dev/null
python3 - "$LINUX_CONTRACT" "$MACOS_CONTRACT" <<'PY'
import pathlib
import sys
import tomllib

linux_path, macos_path = (pathlib.Path(value) for value in sys.argv[1:])
with linux_path.open("rb") as stream:
    linux = tomllib.load(stream)
with macos_path.open("rb") as stream:
    macos = tomllib.load(stream)

assert linux["component"]["platform"]["os"] == ["linux"]
assert macos["component"]["platform"]["os"] == ["macos"]
assert macos["component"]["platform"]["arch"] == ["aarch64", "x86_64"]
assert linux["component"]["contract"]["min_anolisa_version"] == "0.2.17"
assert macos["component"]["contract"]["min_anolisa_version"] == "0.2.17"
assert linux["component"]["conflicts"] == ["cosh"]
assert macos["component"]["conflicts"] == ["cosh"]
linux_dependencies = {
    dependency["name"]: dependency for dependency in linux["component"]["dependencies"]
}
macos_dependencies = {
    dependency["name"]: dependency for dependency in macos["component"]["dependencies"]
}
for dependencies in (linux_dependencies, macos_dependencies):
    assert not [name for name in dependencies if "ssl" in name]
linux_common = dict(linux["component"])
macos_common = dict(macos["component"])
for component in (linux_common, macos_common):
    component.pop("platform")
    component.pop("dependencies")
gateway_service_file = {
    "source": "share/anolisa/cosh-ng/cosh-gateway@.service.in",
    "target": "{unitdir}/cosh-gateway@.service",
    "mode": "0644",
    "render": "anolisa-paths-v1",
}
task_checkpoint_skill_file = {
    "source": "share/anolisa/skills/manage-task-checkpoints/SKILL.md",
    "target": "{datadir}/anolisa/skills/manage-task-checkpoints/SKILL.md",
    "mode": "0644",
}
assert gateway_service_file in linux_common["layout"]["files"]
assert gateway_service_file not in macos_common["layout"]["files"]
assert task_checkpoint_skill_file in linux_common["layout"]["files"]
assert task_checkpoint_skill_file not in macos_common["layout"]["files"]
assert linux_common.pop("services") == [
    {
        "unit": "cosh-gateway@.service",
        "scope": "system",
        "enable": False,
        "start": False,
    },
]
assert "services" not in macos_common
linux_common["layout"] = dict(linux_common["layout"])
linux_common["layout"]["files"] = [
    entry
    for entry in linux_common["layout"]["files"]
    if entry not in (gateway_service_file, task_checkpoint_skill_file)
]
assert linux_common == macos_common
assert linux.get("backends") == macos.get("backends")
PY

LEGACY_CONFLICT_CONTRACT="$TMP/legacy-conflict-component.toml"
sed 's/min_anolisa_version = "0.2.17"/min_anolisa_version = "0.2.16"/' \
    "$LINUX_CONTRACT" > "$LEGACY_CONFLICT_CONTRACT"
if python3 "$ROOT/packaging/raw/verify-release.py" \
    "$SOURCE" "$LEGACY_CONFLICT_CONTRACT" \
    --os linux --arch x86_64 >/dev/null 2>&1; then
    echo "ERROR: conflict contract accepted an ANOLISA version before 0.2.17" >&2
    exit 1
fi
MACOS_EXTRACTED="$TMP/extracted-macos"
install -d -m 0755 "$MACOS_EXTRACTED"
tar -xzf "$TMP/out-macos-arm64/$MACOS_ARTIFACT" -C "$MACOS_EXTRACTED"
cmp "$MACOS_CONTRACT" "$MACOS_EXTRACTED/.anolisa/component.toml"
test ! -e "$MACOS_EXTRACTED/share/anolisa/cosh-ng/cosh-gateway@.service.in"
test ! -e "$MACOS_EXTRACTED/share/anolisa/cosh-ng/cosh-gateway-acp@.service.in"

MACOS_X64_EXTRACTED="$TMP/extracted-macos-x64"
mkdir "$MACOS_X64_EXTRACTED"
tar -xzf "$TMP/out-macos-x64/$MACOS_X64_ARTIFACT" -C "$MACOS_X64_EXTRACTED"
cmp "$MACOS_CONTRACT" "$MACOS_X64_EXTRACTED/.anolisa/component.toml"
cmp "$MACOS_X64/cosh-cli" "$MACOS_X64_EXTRACTED/bin/cosh-cli"
for binary in cosh-core cosh-gateway cosh-shell; do
    cmp "$MACOS_X64/$binary" "$MACOS_X64_EXTRACTED/libexec/anolisa/cosh-ng/$binary"
done
test ! -e "$MACOS_X64_EXTRACTED/share/anolisa/cosh-ng/cosh-gateway@.service.in"
test ! -e "$MACOS_EXTRACTED/share/anolisa/skills/manage-task-checkpoints"
test -z "$(grep -F 'libssl' "$ROOT/.anolisa/component.toml" || true)"
grep -Fq 'source = "bin/cosh"' "$ROOT/.anolisa/component.toml"
grep -Fq 'source = "libexec/anolisa/cosh-ng/cosh-shell"' \
    "$ROOT/.anolisa/component.toml"

echo "cosh-ng component-owned raw package tests passed"

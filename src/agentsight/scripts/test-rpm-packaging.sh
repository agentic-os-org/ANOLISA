#!/usr/bin/env bash
# Validates the portable source-level contract for AgentSight RPM payloads.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
failures=0
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

bash "$repo_root/src/agentsight/scripts/test-policy-classifier.sh"

# EM_BPF does not identify the host ABI. Assert the dispatcher inside the ELF
# function symbol, including its map relocations, without scanning BTF/debug data.
python3 - "$repo_root/src/agentsight/crates/ebpf-ifc-engine" <<'PY'
import hashlib
import pathlib
import re
import struct
import sys


def git_blob(data):
    return hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()


def rename_dispatcher(data, arch):
    assert data[:6] == b"\x7fELF\x02\x01", "expected ELF64 little-endian"
    header = struct.unpack_from("<HHIQQQIHHHHHH", data, 16)
    assert header[0] == 1 and header[1] == 247, "expected relocatable EM_BPF"
    shoff, shsize, shnum, shstrndx = header[5], header[10], header[11], header[12]
    assert shsize == 64
    sections = [struct.unpack_from("<IIQQQQIIQQ", data, shoff + i * shsize)
                for i in range(shnum)]

    def section_data(section):
        start, size = section[4:6]
        assert start + size <= len(data), "section exceeds ELF bounds"
        return data[start:start + size]

    def string(table, offset):
        return table[offset:table.index(b"\0", offset)].decode("ascii")

    names = section_data(sections[shstrndx])
    symtabs = [(i, section) for i, section in enumerate(sections) if section[1] == 2]
    assert len(symtabs) == 1
    symtab_index, symtab = symtabs[0]
    assert symtab[9] == 24
    strings = section_data(sections[symtab[6]])
    symbols = [struct.unpack_from("<IBBHQQ", section_data(symtab), offset)
               for offset in range(0, symtab[5], 24)]
    functions = [symbol for symbol in symbols
                 if symbol[1] & 15 == 2 and string(strings, symbol[0]) == "guard_rename_enter"]
    assert len(functions) == 1, "missing/duplicate guard_rename_enter function"
    _, _, _, section_index, address, size = functions[0]
    section = sections[section_index]
    assert string(names, section[0]) == "tracepoint/raw_syscalls/sys_enter"
    start = address - section[3]
    assert size > 0 and start >= 0 and start + size <= section[5] and size % 8 == 0
    code = section_data(section)[start:start + size]
    instructions = list(struct.iter_unpack("<BBhi", code))
    # clang 17 -O2 emits direct JEQ/JNE immediates (verified by disassembly),
    # each immediately following a ctx->id load through the saved r6 pointer.
    assert instructions[0] == (0xbf, 0x16, 0, 0), "ctx is not saved in r6"
    comparisons = []
    for index, (opcode, registers, offset, immediate) in enumerate(instructions):
        if opcode in (0x15, 0x55) and immediate:
            assert index > 0
            assert instructions[index - 1] == (0x79, 0x60 | (registers & 15), 8, 0), \
                "nonzero compare is not against ctx->id"
            comparisons.append(immediate)
    expected = {"x86_64": {316, 82, 264}, "aarch64": {276, 38}}[arch]
    assert set(comparisons) == expected and len(comparisons) == len(expected), \
        f"{arch} guard_rename_enter comparisons {comparisons}, expected {sorted(expected)}"

    map_helpers = []
    for relocation_section in sections:
        if relocation_section[1] not in (4, 9) or relocation_section[7] != section_index:
            continue
        assert relocation_section[6] == symtab_index
        fmt = "<QQq" if relocation_section[1] == 4 else "<QQ"
        assert relocation_section[9] == struct.calcsize(fmt)
        for entry in struct.iter_unpack(fmt, section_data(relocation_section)):
            offset, info = entry[:2]
            if not start <= offset < start + size:
                continue
            symbol = symbols[info >> 32]
            if string(strings, symbol[0]) != "te_rename_context":
                continue
            assert info & 0xffffffff == 1, "expected R_BPF_64_64 map relocation"
            index = (offset - start) // 8
            assert instructions[index][0:2] == (0x18, 0x01), "map is not loaded into r1"
            calls = [insn for insn in instructions[index + 2:] if insn[0] == 0x85]
            assert calls and calls[0][1] == 0
            map_helpers.append(calls[0][3])
    assert map_helpers == [3, 2], "rename context must be deleted then updated"
    return comparisons


engine = pathlib.Path(sys.argv[1])
constants = dict(re.findall(r'^(ACTPLANE_\w+)="([^"]+)"',
                           (engine.parents[1] / "scripts/build-enforcer.sh").read_text(), re.M))
for arch, opposite in (("x86_64", "aarch64"), ("aarch64", "x86_64")):
    for name in ("process.bpf.o", "process-inode-only.bpf.o"):
        path = engine / "prebuilt" / arch / name
        data = path.read_bytes()
        variant = "PREBUILT_BPF" if name == "process.bpf.o" else "INODE_BPF"
        assert git_blob(data) == constants[f"ACTPLANE_STAGED_{arch.upper()}_{variant}_BLOB"], \
            f"stale artifact inventory: {path}"
        rename_dispatcher(data, arch)
        # Opposite-ABI immediates outside the function must not affect the test.
        rename_dispatcher(data + struct.pack("<III", 276, 38, 316), arch)
        try:
            rename_dispatcher(data, opposite)
        except AssertionError:
            pass
        else:
            raise AssertionError(f"wrong-architecture object accepted: {path}")
        print(f"Verified {arch} {name} guard_rename_enter function and relocations")
PY

# Execute the real build.rs with both target ABIs on this host, without Cargo
# cross-linking. Missing or unsupported artifacts must never trigger make.
rustc --edition=2021 "$repo_root/src/agentsight/crates/ebpf-ifc-engine/build.rs" \
    -o "$tmp_dir/engine-build"
python3 - "$repo_root/src/agentsight/crates/ebpf-ifc-engine" "$tmp_dir" <<'PY'
import os
import pathlib
import subprocess
import sys

engine, tmp = map(pathlib.Path, sys.argv[1:])
for arch in ("x86_64", "aarch64", "riscv64"):
    out = tmp / f"engine-out-{arch}"
    out.mkdir()
    env = dict(os.environ, CARGO_MANIFEST_DIR=str(engine), OUT_DIR=str(out),
               CARGO_CFG_TARGET_ARCH=arch)
    env.pop("ACTPLANE_REBUILD_BPF", None)
    result = subprocess.run([str(tmp / "engine-build")], env=env, capture_output=True)
    if arch == "riscv64":
        assert result.returncode != 0 and b"unsupported BPF target architecture" in result.stderr
        assert not list(out.iterdir())
    else:
        assert result.returncode == 0, result.stderr
        for name in ("process.bpf.o", "process-inode-only.bpf.o"):
            assert (out / name).read_bytes() == (engine / "prebuilt" / arch / name).read_bytes()

fixture = tmp / "rebuild-engine"
fixture.mkdir()
tools = tmp / "rebuild-tools"
tools.mkdir()
(tools / "make").write_text('''#!/usr/bin/env python3
import pathlib, sys
args = sys.argv[1:]
assert "-W" in args and args[args.index("-W") + 1] == "process.bpf.c"
arch = next(arg[5:] for arg in args if arg.startswith("ARCH="))
output = next(arg[7:] for arg in args if arg.startswith("OUTPUT="))
assert arch == {".output/x86_64": "x86", ".output/aarch64": "arm64"}[output]
assert output + "/process.bpf.o" in args and "process-inode-only" in args
root = pathlib.Path(args[args.index("-C") + 1])
(root / output).mkdir(parents=True, exist_ok=True)
for name in ("process.bpf.o", "process-inode-only.bpf.o"):
    (root / output / name).write_text(arch + name)
''')
(tools / "make").chmod(0o755)
for arch, bpf_arch in (("x86_64", "x86"), ("aarch64", "arm64")):
    env = dict(os.environ, CARGO_MANIFEST_DIR=str(fixture),
               OUT_DIR=str(tmp / f"rebuild-out-{arch}"),
               CARGO_CFG_TARGET_ARCH=arch,
               PATH=str(tools) + os.pathsep + os.environ["PATH"])
    pathlib.Path(env["OUT_DIR"]).mkdir()
    env.pop("ACTPLANE_REBUILD_BPF", None)
    result = subprocess.run([str(tmp / "engine-build")], env=env, capture_output=True)
    assert result.returncode != 0 and b"copy " in result.stderr, "missing object triggered make"
    env["ACTPLANE_REBUILD_BPF"] = "1"
    subprocess.run([str(tmp / "engine-build")], env=env, check=True, capture_output=True)
    for name in ("process.bpf.o", "process-inode-only.bpf.o"):
        assert (pathlib.Path(env["OUT_DIR"]) / name).read_text() == bpf_arch + name
PY

require_literal() {
    local file="$1"
    local expected="$2"

    if ! grep -Fq -- "$expected" "$repo_root/$file"; then
        printf 'missing %s in %s\n' "$expected" "$file" >&2
        failures=$((failures + 1))
    fi
}

require_count() {
    local file="$1"
    local expected="$2"
    local count="$3"
    local actual

    actual="$(grep -Fc -- "$expected" "$repo_root/$file" || true)"
    if [[ "$actual" != "$count" ]]; then
        printf 'expected %s occurrences of %s in %s, found %s\n' \
            "$count" "$expected" "$file" "$actual" >&2
        failures=$((failures + 1))
    fi
}

require_in_order() {
    local file="$1"
    local first="$2"
    local second="$3"
    local third="$4"
    local first_line second_line third_line

    first_line="$(grep -Fn -- "$first" "$repo_root/$file" | head -n 1 | cut -d: -f1 || true)"
    second_line="$(grep -Fn -- "$second" "$repo_root/$file" | head -n 1 | cut -d: -f1 || true)"
    third_line="$(grep -Fn -- "$third" "$repo_root/$file" | head -n 1 | cut -d: -f1 || true)"
    if [[ -z "$first_line" || -z "$second_line" || -z "$third_line" \
        || "$first_line" -ge "$second_line" || "$second_line" -ge "$third_line" ]]; then
        printf 'expected installation order in %s: %s, %s, %s\n' \
            "$file" "$first" "$second" "$third" >&2
        failures=$((failures + 1))
    fi
}

require_file() {
    local file="$1"

    if [[ ! -f "$file" ]]; then
        printf 'missing staged file %s\n' "$file" >&2
        failures=$((failures + 1))
    fi
}

fixture_root="$tmp_dir/source"
fixture_payload="$tmp_dir/payload"
mkdir -p "$fixture_root/target/release" "$fixture_root/scripts"
for file in \
    target/release/agentsight \
    target/release/agentsight-enforcer \
    scripts/agentsight-start.sh \
    scripts/agentsight.service \
    scripts/agentsight-enforcer.service \
    agentsight.json \
    component.toml \
    README.md \
    README_zh.md \
    LICENSE; do
    printf 'fixture: %s\n' "$file" > "$fixture_root/$file"
done

# Run a copy of the real wrapper with fake Git/Cargo/Rust only. No network,
# real compilation, or changes to the checkout/source cache are permitted here.
cp "$repo_root/src/agentsight/scripts/build-enforcer.sh" "$fixture_root/scripts/"
cp "$repo_root/src/agentsight/scripts/copy-enforcer.py" "$fixture_root/scripts/"
mkdir -p "$tmp_dir/tools" "$tmp_dir/actplane/.git" "$tmp_dir/actplane/bpf/prebuilt"
python3 - "$fixture_root" "$tmp_dir/tools" "$tmp_dir/actplane" <<'PY'
import pathlib
import re
import sys

root, tools, source = map(pathlib.Path, sys.argv[1:])
constants = dict(re.findall(r'^(ACTPLANE_\w+)="([^"]+)"',
                           (root / "scripts/build-enforcer.sh").read_text(), re.M))
(root / "Cargo.toml").write_text(f'rev = "{constants["ACTPLANE_REVISION"]}"\n' * 2)
engine = root / "crates/ebpf-ifc-engine"
(engine / "prebuilt").mkdir(parents=True)
for name in ("process.bpf.c", "process.h", "taint.h", "taint_engine.bpf.h",
             "capability.bpf.h", "channel.bpf.h"):
    (engine / name).write_text(name)
(engine / "build.rs").write_bytes(b"staged-build")
for arch in ("x86_64", "aarch64"):
    (engine / "prebuilt" / arch).mkdir()
    (engine / "prebuilt" / arch / "process.bpf.o").write_bytes(f"{arch}-full-object".encode())
    (engine / "prebuilt" / arch / "process-inode-only.bpf.o").write_bytes(
        f"{arch}-inode-object".encode())
(source / "bpf/prebuilt/process.bpf.o").write_bytes(b"upstream-object")
(tools / "git").write_text('''#!/usr/bin/env python3
import pathlib, sys
constants = ''' + repr(constants) + '''
source, command, *args = sys.argv[2:]
if command == "rev-parse":
    print(constants["ACTPLANE_REVISION"])
elif command == "checkout":
    # Model the source reset performed by the real cache checkout/reset.
    (pathlib.Path(source) / "bpf/prebuilt/process.bpf.o").write_bytes(b"upstream-object")
    (pathlib.Path(source) / "bpf/build.rs").write_bytes(b"patched-build")
    print("fixture Git diagnostic")
elif command == "hash-object":
    path = pathlib.Path(source) / args[0]
    key = {"lib.rs": "PATCHED_BPF_LIB", "Makefile": "PATCHED_BPF_MAKEFILE"}.get(path.name)
    if path.name == "build.rs":
        key = {b"staged-build": "STAGED_BPF_BUILD",
               b"patched-build": "PATCHED_BPF_BUILD"}.get(path.read_bytes())
    elif path.suffix == ".o":
        data = path.read_bytes()
        key = "PREBUILT_BPF" if data == b"upstream-object" else None
        for arch in ("x86_64", "aarch64"):
            for variant, suffix in (("full", "PREBUILT_BPF"), ("inode", "INODE_BPF")):
                if data == f"{arch}-{variant}-object".encode():
                    key = f"STAGED_{arch.upper()}_{suffix}"
    print(constants["ACTPLANE_" + key + "_BLOB"] if key else "0" * 40)
elif command not in ("reset", "clean", "diff", "ls-files", "submodule"):
    sys.exit("unexpected fixture Git command: " + command)
''')
(tools / "rustc").write_text('#!/bin/sh\nprintf "host: x86_64-unknown-linux-gnu\\n"\n')
(tools / "cargo").write_text('''#!/usr/bin/env python3
import os, pathlib, sys
args = sys.argv
root = pathlib.Path.cwd()
target = pathlib.Path(args[args.index("--target-dir") + 1])
host = args[args.index("--target") + 1]
assert host == os.environ.get("EXPECTED_TARGET", "x86_64-unknown-linux-gnu")
assert str(target) == os.environ["CARGO_TARGET_DIR"]
assert target.is_absolute() and target.parent.name.startswith("enforcer.")
assert target != root / "target"
output = target / host / "release/agentsight-enforcer"
output.parent.mkdir(parents=True)
arch = host.split("-")[0]
patch_config = args[args.index("--config") + 1]
source = pathlib.Path(patch_config.split('path="')[1].rstrip('"'))
assert (source / "build.rs").read_bytes() == b"staged-build"
objects = source / "prebuilt" / arch
assert (objects / "process.bpf.o").read_bytes() == f"{arch}-full-object".encode()
assert (objects / "process-inode-only.bpf.o").read_bytes() == f"{arch}-inode-object".encode()
if os.environ.get("FIXTURE_WRONG_PAIR"):
    arch = "aarch64" if arch == "x86_64" else "x86_64"
output.write_bytes(str(target).encode() + f"{arch}-full-object {arch}-inode-object".encode())
if os.environ.get("FIXTURE_BAD_OBJECT"):
    output.write_bytes(b"missing embedded objects")
output.chmod(0o755)
# A different Cargo invocation replaces the old shared executable before return.
(root / "target/release/agentsight-enforcer").write_bytes(b"unattested shared replacement")
print("fixture Cargo diagnostic")
''')
for tool in tools.iterdir():
    tool.chmod(0o755)
PY

fixture_env=(env -u ENFORCER_BIN -u CARGO_BUILD_TARGET -u ACTPLANE_REBUILD_BPF
    "PATH=$tmp_dir/tools:$PATH" "CARGO=$tmp_dir/tools/cargo"
    "CARGO_TARGET_DIR=$tmp_dir/cargo parent" "ACTPLANE_SOURCE_DIR=$tmp_dir/actplane")
first_bin="$("${fixture_env[@]}" "$fixture_root/scripts/build-enforcer.sh" 2>"$tmp_dir/build.err")"
second_bin="$("${fixture_env[@]}" "$fixture_root/scripts/build-enforcer.sh" 2>>"$tmp_dir/build.err")"
[[ "$first_bin" = /* && "$second_bin" = /* && "$first_bin" != "$second_bin" ]]
[[ "$first_bin" != *$'\n'* && "$second_bin" != *$'\n'* ]]
[[ -x "$first_bin" && -f "$first_bin.sha256" && -x "$second_bin" ]]
grep -q 'fixture Cargo diagnostic' "$tmp_dir/build.err"
grep -q 'fixture Git diagnostic' "$tmp_dir/build.err"
[[ "$(stat -c %a "$(dirname "$first_bin")")" = 700 ]]

ENFORCER_BIN="$first_bin" AGENTSIGHT_PROJECT_ROOT="$fixture_root" \
    bash "$repo_root/src/agentsight/scripts/stage-rpm-payload.sh" "$fixture_payload"
ENFORCER_BIN="$second_bin" AGENTSIGHT_PROJECT_ROOT="$fixture_root" \
    bash "$repo_root/src/agentsight/scripts/stage-rpm-payload.sh" "$tmp_dir/payload-two"
cmp "$first_bin" "$fixture_payload/agentsight-enforcer"
cmp "$second_bin" "$tmp_dir/payload-two/agentsight-enforcer"
if cmp -s "$first_bin" "$second_bin"; then
    printf 'independent builds unexpectedly returned the same bytes\n' >&2
    exit 1
fi

# Even with a shared binary present, no handoff means another fresh attested build.
"${fixture_env[@]}" AGENTSIGHT_PROJECT_ROOT="$fixture_root" \
    bash "$repo_root/src/agentsight/scripts/stage-rpm-payload.sh" "$tmp_dir/fresh-payload" \
    2>>"$tmp_dir/build.err"
if cmp -s "$tmp_dir/fresh-payload/agentsight-enforcer" "$second_bin"; then
    printf 'RPM staging reused a previous build without an explicit handoff\n' >&2
    exit 1
fi
for failure in CARGO_BUILD_TARGET=other-target ACTPLANE_REBUILD_BPF=1 FIXTURE_BAD_OBJECT=1 \
    FIXTURE_WRONG_PAIR=1; do
    if "${fixture_env[@]}" "$failure" "$fixture_root/scripts/build-enforcer.sh" \
        >"$tmp_dir/failed-path" 2>"$tmp_dir/failed-build.err"; then
        printf 'attested build accepted %s\n' "$failure" >&2
        exit 1
    fi
    [[ ! -s "$tmp_dir/failed-path" ]]
    if [[ "$failure" = FIXTURE_WRONG_PAIR=1 ]]; then
        grep -Fq 'BPF object is not embedded' "$tmp_dir/failed-build.err"
    fi
done

# Cross builds preserve the same private-output and receipt contract.
arm_bin="$("${fixture_env[@]}" EXPECTED_TARGET=aarch64-unknown-linux-gnu \
    "$fixture_root/scripts/build-enforcer.sh" --target aarch64-unknown-linux-gnu \
    2>>"$tmp_dir/build.err")"
[[ -x "$arm_bin" && -f "$arm_bin.sha256" && "$arm_bin" != "$first_bin" ]]
python3 "$repo_root/src/agentsight/scripts/copy-enforcer.py" "$arm_bin" "$tmp_dir/arm-enforcer"
cmp "$arm_bin" "$tmp_dir/arm-enforcer"
# Each target embeds only its own object pair; EM_BPF alone does not identify the ABI.
python3 - "$first_bin" "$arm_bin" <<'PY'
import pathlib
import sys

x86, arm = (pathlib.Path(path).read_bytes() for path in sys.argv[1:])
for variant in ("full", "inode"):
    assert f"x86_64-{variant}-object".encode() in x86
    assert f"aarch64-{variant}-object".encode() not in x86
    assert f"aarch64-{variant}-object".encode() in arm
    assert f"x86_64-{variant}-object".encode() not in arm
PY
if "${fixture_env[@]}" "$fixture_root/scripts/build-enforcer.sh" --target aarch64-apple-darwin \
    >"$tmp_dir/unsupported-path" 2>"$tmp_dir/unsupported.err"; then
    echo 'unsupported enforcer target was accepted' >&2
    exit 1
fi
[[ ! -s "$tmp_dir/unsupported-path" ]]
grep -Fq 'unsupported enforcer target' "$tmp_dir/unsupported.err"

# Make install must hand off the captured path in the same recipe shell.
"${fixture_env[@]}" make --no-print-directory -C "$fixture_root" \
    -f "$repo_root/src/agentsight/Makefile" -o build -o build-frontend install \
    "DESTDIR=$tmp_dir/install" SETCAP=0 INSTALL_SYSTEMD=0 >"$tmp_dir/make.out" 2>"$tmp_dir/make.err"
[[ -x "$tmp_dir/install/usr/local/bin/agentsight-enforcer" ]]
! cmp -s "$tmp_dir/install/usr/local/bin/agentsight-enforcer" \
    "$fixture_root/target/release/agentsight-enforcer"
make --no-print-directory -C "$fixture_root" -f "$repo_root/src/agentsight/Makefile" \
    -o build -o build-frontend install ENFORCER_BUILD=false "ENFORCER_BIN=$first_bin" \
    "DESTDIR=$tmp_dir/install" SETCAP=0 INSTALL_SYSTEMD=0 >>"$tmp_dir/make.out" 2>>"$tmp_dir/make.err"
cmp "$first_bin" "$tmp_dir/install/usr/local/bin/agentsight-enforcer"

# Mutate the source just after hashing: the destination must still receive the
# checked buffer, not bytes from a subsequent reopen of that source path.
python3 - "$repo_root/src/agentsight/scripts/copy-enforcer.py" "$first_bin" "$tmp_dir" <<'PY'
import hashlib
import pathlib
import runpy
import sys
from unittest.mock import patch

copy_enforcer = runpy.run_path(sys.argv[1])["copy_enforcer"]
source = pathlib.Path(sys.argv[3]) / "copy-input"
original = pathlib.Path(sys.argv[2]).read_bytes()
source.write_bytes(original)
source.chmod(0o755)
pathlib.Path(str(source) + ".sha256").write_text(hashlib.sha256(original).hexdigest())
digest = hashlib.sha256

def replace_after_hash(data):
    result = digest(data)
    source.write_bytes(b"replacement after hash")
    return result

output = source.with_name("copy-output")
with patch.object(hashlib, "sha256", side_effect=replace_after_hash):
    copy_enforcer(source, output)
assert output.read_bytes() == original
try:
    copy_enforcer(source, output)
except ValueError:
    pass
else:
    raise AssertionError("stale receipt accepted")
assert output.read_bytes() == original
pathlib.Path(str(source) + ".sha256").unlink()
try:
    copy_enforcer(source, output)
except FileNotFoundError:
    pass
else:
    raise AssertionError("missing receipt accepted")
PY

for file in \
    agentsight \
    agentsight-enforcer \
    agentsight-start \
    agentsight.service \
    agentsight-enforcer.service \
    agentsight.json \
    component.toml \
    README.md \
    README_zh.md \
    LICENSE; do
    require_file "$fixture_payload/$file"
done

existing_payload="$tmp_dir/existing-payload"
mkdir -p "$existing_payload"
printf 'preserve me\n' > "$existing_payload/sentinel"
if AGENTSIGHT_PROJECT_ROOT="$fixture_root" \
    bash "$repo_root/src/agentsight/scripts/stage-rpm-payload.sh" \
    "$existing_payload" >"$tmp_dir/existing.out" 2>"$tmp_dir/existing.err"; then
    printf 'existing RPM payload destination unexpectedly replaced\n' >&2
    failures=$((failures + 1))
fi
if [[ ! -f "$existing_payload/sentinel" ]] \
    || [[ "$(cat "$existing_payload/sentinel")" != "preserve me" ]]; then
    printf 'existing RPM payload destination was modified\n' >&2
    failures=$((failures + 1))
fi

complete_list="$tmp_dir/complete-rpm-files.txt"
cat > "$complete_list" <<'EOF'
/usr/local/bin/agentsight
/usr/local/bin/agentsight-enforcer
/usr/local/bin/agentsight-start
/usr/lib/systemd/system/agentsight.service
/usr/lib/systemd/system/agentsight-enforcer.service
/etc/agentsight/config.json
/usr/share/anolisa/components/agentsight/component.toml
EOF

bash "$repo_root/src/agentsight/scripts/verify-rpm-package.sh" \
    --file-list "$complete_list"

incomplete_list="$tmp_dir/incomplete-rpm-files.txt"
grep -v -E 'agentsight-enforcer($|\.service$)' "$complete_list" > "$incomplete_list"
if bash "$repo_root/src/agentsight/scripts/verify-rpm-package.sh" \
    --file-list "$incomplete_list" >"$tmp_dir/incomplete.out" 2>"$tmp_dir/incomplete.err"; then
    printf 'incomplete RPM file list unexpectedly passed\n' >&2
    failures=$((failures + 1))
else
    require_literal_from_path() {
        local file="$1"
        local expected="$2"
        if ! grep -Fq -- "$expected" "$file"; then
            printf 'missing verifier diagnostic %s\n' "$expected" >&2
            failures=$((failures + 1))
        fi
    }
    require_literal_from_path "$tmp_dir/incomplete.err" "/usr/local/bin/agentsight-enforcer"
    require_literal_from_path "$tmp_dir/incomplete.err" \
        "/usr/lib/systemd/system/agentsight-enforcer.service"
fi

require_literal src/agentsight/scripts/rpm-build.sh \
    'ENFORCER_BIN="$(./scripts/build-enforcer.sh)"'
require_literal src/agentsight/scripts/rpm-build.sh \
    'ENFORCER_BIN="$ENFORCER_BIN" ./scripts/stage-rpm-payload.sh "$TARBALL_DIR"'
require_literal scripts/rpm-build.sh \
    './scripts/build-enforcer.sh >&3'
require_literal scripts/rpm-build.sh \
    'ENFORCER_BIN="$ENFORCER_BIN" "${SIGHT_DIR}/scripts/stage-rpm-payload.sh" "$pkg_dir"'
require_literal .github/workflows/_rpm-build.yaml \
    '"$SOURCE_ROOT/scripts/stage-rpm-payload.sh" "$PACKAGE_DIR/${COMPONENT}-${VERSION}"'
require_literal .github/workflows/_rpm-build.yaml \
    '"$AGENTSIGHT_VERIFY_RPM" "$rpm_path"'

require_literal src/agentsight/agentsight.spec.in \
    "install -p -m 0755 agentsight-enforcer %{buildroot}/usr/local/bin/"
require_literal src/agentsight/agentsight.spec.in \
    "install -p -m 0644 agentsight-enforcer.service %{buildroot}%{_unitdir}/"
require_literal src/agentsight/agentsight.spec.in "%{_unitdir}/agentsight-enforcer.service"
require_literal src/agentsight/agentsight.spec.in "%systemd_post agentsight-enforcer.service"
require_literal src/agentsight/agentsight.spec.in "%systemd_preun agentsight-enforcer.service"
require_literal src/agentsight/agentsight.spec.in "%systemd_postun agentsight-enforcer.service"
# rpm expands macros in comments too, and with systemd-rpm-macros installed a
# bare %systemd_post (zero arguments) is fatal: "The %systemd_post macro
# requires some arguments". Any comment that names the scriptlet macros must
# escape them, or rpmbuild aborts before it produces a package.
require_count src/agentsight/agentsight.spec.in \
    "%systemd_post/%systemd_preun/%systemd_postun" 0
require_literal src/agentsight/agentsight.spec.in \
    "%%systemd_post/%%systemd_preun/%%systemd_postun"
require_literal src/agentsight/agentsight.spec.in "%%systemd_* scriptlet macros"
require_literal src/agentsight/scripts/agentsight.service "Wants=agentsight-enforcer.service"
require_literal src/agentsight/scripts/agentsight-enforcer.service "PartOf=agentsight.service"
require_literal src/agentsight/scripts/agentsight.service "UMask=0077"
require_literal src/agentsight/scripts/agentsight-enforcer.service "UMask=0077"

require_literal distribution/anolisa/manifests/components/agentsight/component.toml \
    'source = "bin/agentsight-enforcer"'
require_literal distribution/anolisa/manifests/components/agentsight/component.toml \
    'target = "{bindir}/agentsight-enforcer"'
require_literal distribution/anolisa/manifests/components/agentsight/component.toml \
    'source = "share/anolisa/agentsight/agentsight-enforcer.service"'
require_literal distribution/anolisa/manifests/components/agentsight/component.toml \
    'target = "{unitdir}/agentsight-enforcer.service"'
require_literal distribution/anolisa/manifests/components/agentsight/component.toml \
    'unit = "agentsight-enforcer.service"'

require_count src/agentsight/tests/security_pipeline.rs \
    "let required_subscription = client.subscribe_required().expect(\"subscribe required\");" 2
require_count src/agentsight/tests/security_pipeline.rs \
    ".apply(request.clone(), required_subscription.subscription_id())" 2

# Companion-service setup belongs in the component guides, not the product overview.
for document in \
    docs/user-guide/en/agentsight/index.md \
    docs/user-guide/zh/agentsight/index.md \
    src/agentsight/README.md src/agentsight/README_zh.md; do
    require_literal "$document" "agentsight-enforcer"
done
require_in_order src/agentsight/README.md \
    "### Install with Anolisa" "### Install via RPM" "### Build from Source"
require_in_order src/agentsight/README_zh.md \
    "### 通过 Anolisa 安装" "### 通过 RPM 安装" "### 从源码构建"

if (( failures > 0 )); then
    exit 1
fi

printf 'AgentSight RPM packaging contract passed.\n'

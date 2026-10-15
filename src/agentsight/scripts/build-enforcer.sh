#!/bin/sh

set -eu

# Only the verified executable path is returned to command-substitution callers.
exec 3>&1 1>&2

ACTPLANE_REPOSITORY="https://github.com/eunomia-bpf/ActPlane.git"
ACTPLANE_REVISION="a62e5d9d96f91101cda019519053e950d532380a"
ACTPLANE_BASE_BPF_LIB_BLOB="9bcefcdca83b89635788beaf8de15f33252427bb"
ACTPLANE_POST_0001_BPF_LIB_BLOB="f4c0598596a5134725cc1e2fd27da4a5f6a16cdd"
ACTPLANE_POST_0002_BPF_LIB_BLOB="6ddf254640e41c227a8e6d794cef247f2705bd26"
ACTPLANE_POST_0003_BPF_LIB_BLOB="9aa60178ba61a6ad0723b1fe01e823ee94ee742d"
ACTPLANE_POST_0004_BPF_LIB_BLOB="e1bca6268e0cca0a2d4aa59cc807592f65671a01"
ACTPLANE_POST_0005_BPF_LIB_BLOB="efe99bacf1f50ec388e35542aa0210b74c9ac970"
ACTPLANE_POST_0007_BPF_LIB_BLOB="1f7208a5d81ce822ac6ddc25b4c126d1ac96c891"
ACTPLANE_POST_0008_BPF_LIB_BLOB="d76db22d5517abf3c5b43d5d30fe11d641d4c805"
ACTPLANE_POST_0009_BPF_LIB_BLOB="450b8c3035bb66602f5b946da7dc4af72cdc5202"
ACTPLANE_PATCHED_BPF_LIB_BLOB="0d5a9c60ff943ad0076f02948897e4a5141a7784"
ACTPLANE_BASE_BPF_BUILD_BLOB="ae2345ecd26bbf47a6f7dae228ed08ec7e83bcbc"
ACTPLANE_PATCHED_BPF_BUILD_BLOB="c3ba506d246d3139b8ee148d942db6ba4159b1b9"
ACTPLANE_BASE_BPF_MAKEFILE_BLOB="eddb4267ba5a10e7f94ad64f19323040368813c1"
ACTPLANE_PATCHED_BPF_MAKEFILE_BLOB="b354320bf25dacc72af1a60419f81ea0a173eb1f"
ACTPLANE_STAGED_PREBUILT_BPF_BLOB="999b37639c50c6daa9495e8d45c7f993b9c9681d"
ACTPLANE_STAGED_INODE_BPF_BLOB="83cd4269b7ac4c19bf553ba6b186ee6ead24f7ce"
ACTPLANE_PREBUILT_BPF_BLOB="0ef15841f84be784774024ad844e70bc6124a753"

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
AGENTSIGHT_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
PATCH_DIR="$AGENTSIGHT_ROOT/patches/actplane"
PATCH_0001_FILE="$PATCH_DIR/0001-add-file-enforcement-profile.patch"
PATCH_0002_FILE="$PATCH_DIR/0002-bound-pinned-event-handoff.patch"
PATCH_0003_FILE="$PATCH_DIR/0003-add-credential-exfiltration-hooks.patch"
PATCH_0004_FILE="$PATCH_DIR/0004-add-config-blob-size-guard.patch"
PATCH_0005_FILE="$PATCH_DIR/0005-add-agent-file-guard-profile.patch"
PATCH_0006_FILE="$PATCH_DIR/0006-add-inode-guard-map.patch"
PATCH_0007_FILE="$PATCH_DIR/0007-dedicated-drain-trigger.patch"
PATCH_0008_FILE="$PATCH_DIR/0008-clean-stale-pin-root.patch"
PATCH_0009_FILE="$PATCH_DIR/0009-migrate-session-tree.patch"
PATCH_0010_FILE="$PATCH_DIR/0010-support-legacy-kernel-runtime.patch"
PATCH_FILES="$PATCH_0001_FILE
$PATCH_0002_FILE
$PATCH_0003_FILE
$PATCH_0004_FILE
$PATCH_0005_FILE
$PATCH_0006_FILE
$PATCH_0007_FILE
$PATCH_0008_FILE
$PATCH_0009_FILE
$PATCH_0010_FILE"
CARGO=${CARGO:-cargo}

DECLARED_REVISION_COUNT=$(grep -F -c "rev = \"$ACTPLANE_REVISION\"" "$AGENTSIGHT_ROOT/Cargo.toml" || true)
if [ "$DECLARED_REVISION_COUNT" -ne 2 ]; then
    echo "ActPlane build revision does not match both workspace dependencies" >&2
    echo "Update build-enforcer.sh and the compatibility patch together." >&2
    exit 1
fi

# CARGO_TARGET_DIR selects only a parent, never the actual Cargo output directory.
TARGET_ROOT=${CARGO_TARGET_DIR:-$AGENTSIGHT_ROOT/target}
if [ -n "${CARGO_BUILD_TARGET:-}" ]; then
    echo "CARGO_BUILD_TARGET is not supported by the native attested enforcer build" >&2
    exit 1
fi
if [ -n "${ACTPLANE_SOURCE_DIR:-}" ]; then
    SOURCE_DIR=$ACTPLANE_SOURCE_DIR
else
    SOURCE_DIR="$TARGET_ROOT/actplane-src/$ACTPLANE_REVISION"
fi

if [ "${ACTPLANE_REBUILD_BPF+x}" = x ]; then
    echo "ACTPLANE_REBUILD_BPF is not allowed for the attested enforcer build" >&2
    exit 1
fi
if ! command -v flock >/dev/null 2>&1 || ! command -v realpath >/dev/null 2>&1; then
    echo "flock and realpath are required to protect the shared ActPlane source cache" >&2
    exit 1
fi
mkdir -p "$(dirname -- "$SOURCE_DIR")"
if [ -e "$SOURCE_DIR" ]; then
    SOURCE_DIR=$(realpath "$SOURCE_DIR")
else
    SOURCE_PARENT=$(realpath "$(dirname -- "$SOURCE_DIR")")
    SOURCE_DIR="$SOURCE_PARENT/$(basename -- "$SOURCE_DIR")"
fi
LOCK_FILE="$SOURCE_DIR.agentsight.lock"
exec 9>"$LOCK_FILE"
flock 9

# Reuse the cached source only if it can be reset to the pinned revision.
# A cached shallow clone whose objects were advanced to a newer upstream tip
# (CI cache reuse) cannot re-fetch the older pinned SHA in place -- the server
# rejects it with "upload-pack: not our ref". In that case discard the cache
# and perform a fresh shallow clone, which fetches the pinned SHA cleanly.
if [ -d "$SOURCE_DIR/.git" ]; then
    if git -C "$SOURCE_DIR" checkout -q --detach "$ACTPLANE_REVISION" 2>/dev/null; then
        # Drop any leftover applied patches / untracked files from a prior run
        # so the attestation starts from a pristine pinned tree.
        git -C "$SOURCE_DIR" reset -q --hard "$ACTPLANE_REVISION"
        git -C "$SOURCE_DIR" clean -qfd
    else
        rm -rf "$SOURCE_DIR"
    fi
fi

if [ ! -d "$SOURCE_DIR/.git" ]; then
    INCOMPLETE_DIR="$SOURCE_DIR.incomplete.$$"
    trap 'rm -rf "$INCOMPLETE_DIR"' EXIT HUP INT TERM
    git init -q "$INCOMPLETE_DIR"
    git -C "$INCOMPLETE_DIR" remote add origin "$ACTPLANE_REPOSITORY"
    git -C "$INCOMPLETE_DIR" fetch --depth 1 origin "$ACTPLANE_REVISION"
    git -C "$INCOMPLETE_DIR" checkout -q --detach FETCH_HEAD
    mv "$INCOMPLETE_DIR" "$SOURCE_DIR"
    trap - EXIT HUP INT TERM
fi

if ! ACTUAL_REVISION=$(git -C "$SOURCE_DIR" rev-parse HEAD 2>/dev/null); then
    echo "ActPlane source cache is incomplete: $SOURCE_DIR" >&2
    echo "Remove that directory and retry." >&2
    exit 1
fi
if [ "$ACTUAL_REVISION" != "$ACTPLANE_REVISION" ]; then
    echo "ActPlane source revision mismatch: expected $ACTPLANE_REVISION, got $ACTUAL_REVISION" >&2
    exit 1
fi

UNEXPECTED_TRACKED=$(git -C "$SOURCE_DIR" diff --name-only --ignore-submodules=all \
    "$ACTPLANE_REVISION" -- . | grep -Ev '^bpf/(src/lib\.rs|build\.rs|Makefile)$' || true)
UNEXPECTED_UNTRACKED=$(git -C "$SOURCE_DIR" ls-files --others --exclude-standard \
    | grep -v '^\.cargo-ok$' || true)
UNEXPECTED_IGNORED=$(git -C "$SOURCE_DIR" ls-files --others --ignored --exclude-standard || true)
DIRTY_SUBMODULES=$(git -C "$SOURCE_DIR" submodule status --recursive \
    | grep -E '^[+U]' || true)
if [ -n "$UNEXPECTED_TRACKED" ] || [ -n "$UNEXPECTED_UNTRACKED" ] \
    || [ -n "$UNEXPECTED_IGNORED" ] || [ -n "$DIRTY_SUBMODULES" ]; then
    echo "ActPlane source contains changes outside the reviewed patch queue" >&2
    printf '%s\n%s\n%s\n%s\n' "$UNEXPECTED_TRACKED" "$UNEXPECTED_UNTRACKED" \
        "$UNEXPECTED_IGNORED" "$DIRTY_SUBMODULES" >&2
    exit 1
fi

ACTUAL_PREBUILT_BPF_BLOB=$(git -C "$SOURCE_DIR" hash-object bpf/prebuilt/process.bpf.o)
if [ "$ACTUAL_PREBUILT_BPF_BLOB" != "$ACTPLANE_PREBUILT_BPF_BLOB" ]; then
    echo "ActPlane prebuilt BPF object failed source attestation" >&2
    exit 1
fi

ACTUAL_BPF_BUILD_BLOB=$(git -C "$SOURCE_DIR" hash-object bpf/build.rs)
ACTUAL_BPF_MAKEFILE_BLOB=$(git -C "$SOURCE_DIR" hash-object bpf/Makefile)
if [ "$ACTUAL_BPF_BUILD_BLOB" != "$ACTPLANE_BASE_BPF_BUILD_BLOB" ] \
    && [ "$ACTUAL_BPF_BUILD_BLOB" != "$ACTPLANE_PATCHED_BPF_BUILD_BLOB" ]; then
    echo "ActPlane BPF build script failed source attestation" >&2
    exit 1
fi
if [ "$ACTUAL_BPF_MAKEFILE_BLOB" != "$ACTPLANE_BASE_BPF_MAKEFILE_BLOB" ] \
    && [ "$ACTUAL_BPF_MAKEFILE_BLOB" != "$ACTPLANE_PATCHED_BPF_MAKEFILE_BLOB" ]; then
    echo "ActPlane BPF Makefile failed source attestation" >&2
    exit 1
fi

ACTUAL_BPF_LIB_BLOB=$(git -C "$SOURCE_DIR" hash-object bpf/src/lib.rs)
# Zero-context patches keep nested diffs whitespace-clean; exact input-blob
# checks above prevent them from applying to any unreviewed source state.
if [ "$ACTUAL_BPF_LIB_BLOB" = "$ACTPLANE_BASE_BPF_LIB_BLOB" ]; then
    for patch_file in $PATCH_FILES; do
        git -C "$SOURCE_DIR" apply --unidiff-zero --check "$patch_file"
        git -C "$SOURCE_DIR" apply --unidiff-zero "$patch_file"
    done
elif [ "$ACTUAL_BPF_LIB_BLOB" = "$ACTPLANE_POST_0001_BPF_LIB_BLOB" ]; then
    for patch_file in "$PATCH_0002_FILE" "$PATCH_0003_FILE" "$PATCH_0004_FILE" "$PATCH_0005_FILE" "$PATCH_0006_FILE" "$PATCH_0007_FILE" "$PATCH_0008_FILE" "$PATCH_0009_FILE" "$PATCH_0010_FILE"; do
        git -C "$SOURCE_DIR" apply --unidiff-zero --check "$patch_file"
        git -C "$SOURCE_DIR" apply --unidiff-zero "$patch_file"
    done
elif [ "$ACTUAL_BPF_LIB_BLOB" = "$ACTPLANE_POST_0002_BPF_LIB_BLOB" ]; then
    for patch_file in "$PATCH_0003_FILE" "$PATCH_0004_FILE" "$PATCH_0005_FILE" "$PATCH_0006_FILE" "$PATCH_0007_FILE" "$PATCH_0008_FILE" "$PATCH_0009_FILE" "$PATCH_0010_FILE"; do
        git -C "$SOURCE_DIR" apply --unidiff-zero --check "$patch_file"
        git -C "$SOURCE_DIR" apply --unidiff-zero "$patch_file"
    done
elif [ "$ACTUAL_BPF_LIB_BLOB" = "$ACTPLANE_POST_0003_BPF_LIB_BLOB" ]; then
    for patch_file in "$PATCH_0004_FILE" "$PATCH_0005_FILE" "$PATCH_0006_FILE" "$PATCH_0007_FILE" "$PATCH_0008_FILE" "$PATCH_0009_FILE" "$PATCH_0010_FILE"; do
        git -C "$SOURCE_DIR" apply --unidiff-zero --check "$patch_file"
        git -C "$SOURCE_DIR" apply --unidiff-zero "$patch_file"
    done
elif [ "$ACTUAL_BPF_LIB_BLOB" = "$ACTPLANE_POST_0004_BPF_LIB_BLOB" ]; then
    for patch_file in "$PATCH_0005_FILE" "$PATCH_0006_FILE" "$PATCH_0007_FILE" "$PATCH_0008_FILE" "$PATCH_0009_FILE" "$PATCH_0010_FILE"; do
        git -C "$SOURCE_DIR" apply --unidiff-zero --check "$patch_file"
        git -C "$SOURCE_DIR" apply --unidiff-zero "$patch_file"
    done
elif [ "$ACTUAL_BPF_LIB_BLOB" = "$ACTPLANE_POST_0005_BPF_LIB_BLOB" ]; then
    for patch_file in "$PATCH_0006_FILE" "$PATCH_0007_FILE" "$PATCH_0008_FILE" "$PATCH_0009_FILE" "$PATCH_0010_FILE"; do
        git -C "$SOURCE_DIR" apply --unidiff-zero --check "$patch_file"
        git -C "$SOURCE_DIR" apply --unidiff-zero "$patch_file"
    done
elif [ "$ACTUAL_BPF_LIB_BLOB" = "$ACTPLANE_POST_0007_BPF_LIB_BLOB" ]; then
    for patch_file in "$PATCH_0008_FILE" "$PATCH_0009_FILE" "$PATCH_0010_FILE"; do
        git -C "$SOURCE_DIR" apply --unidiff-zero --check "$patch_file"
        git -C "$SOURCE_DIR" apply --unidiff-zero "$patch_file"
    done
elif [ "$ACTUAL_BPF_LIB_BLOB" = "$ACTPLANE_POST_0008_BPF_LIB_BLOB" ]; then
    for patch_file in "$PATCH_0009_FILE" "$PATCH_0010_FILE"; do
        git -C "$SOURCE_DIR" apply --unidiff-zero --check "$patch_file"
        git -C "$SOURCE_DIR" apply --unidiff-zero "$patch_file"
    done
elif [ "$ACTUAL_BPF_LIB_BLOB" = "$ACTPLANE_POST_0009_BPF_LIB_BLOB" ]; then
    git -C "$SOURCE_DIR" apply --unidiff-zero --check "$PATCH_0010_FILE"
    git -C "$SOURCE_DIR" apply --unidiff-zero "$PATCH_0010_FILE"
elif [ "$ACTUAL_BPF_LIB_BLOB" != "$ACTPLANE_PATCHED_BPF_LIB_BLOB" ]; then
    echo "ActPlane BPF loader does not match the pinned revision or reviewed patch queue" >&2
    exit 1
fi

ACTUAL_BPF_LIB_BLOB=$(git -C "$SOURCE_DIR" hash-object bpf/src/lib.rs)
ACTUAL_BPF_BUILD_BLOB=$(git -C "$SOURCE_DIR" hash-object bpf/build.rs)
ACTUAL_BPF_MAKEFILE_BLOB=$(git -C "$SOURCE_DIR" hash-object bpf/Makefile)
if [ "$ACTUAL_BPF_LIB_BLOB" != "$ACTPLANE_PATCHED_BPF_LIB_BLOB" ] \
    || [ "$ACTUAL_BPF_BUILD_BLOB" != "$ACTPLANE_PATCHED_BPF_BUILD_BLOB" ] \
    || [ "$ACTUAL_BPF_MAKEFILE_BLOB" != "$ACTPLANE_PATCHED_BPF_MAKEFILE_BLOB" ]; then
    echo "ActPlane compatibility patch result failed source attestation" >&2
    exit 1
fi

# Kernel-side changes live in this repository and ship as two reviewed
# prebuilt objects: the full variant for kernels that permit bpf_d_path in LSM
# hooks, and the inode/mailbox variant for legacy kernels. Pin both by hash so
# the attested enforcer embeds exactly the reviewed bytes. The C sources and
# Makefile are copied alongside for auditability; ACTPLANE_REBUILD_BPF remains
# forbidden for this release build.
VENDORED_ENGINE_DIR="$AGENTSIGHT_ROOT/crates/ebpf-ifc-engine"
STAGED_PREBUILT_BLOB=$(git -C "$AGENTSIGHT_ROOT" hash-object "$VENDORED_ENGINE_DIR/prebuilt/process.bpf.o")
STAGED_INODE_BLOB=$(git -C "$AGENTSIGHT_ROOT" hash-object "$VENDORED_ENGINE_DIR/prebuilt/process-inode-only.bpf.o")
if [ "$STAGED_PREBUILT_BLOB" != "$ACTPLANE_STAGED_PREBUILT_BPF_BLOB" ] \
    || [ "$STAGED_INODE_BLOB" != "$ACTPLANE_STAGED_INODE_BPF_BLOB" ]; then
    echo "vendored ebpf-ifc-engine prebuilt objects failed source attestation" >&2
    exit 1
fi
for engine_file in process.bpf.c process.h taint.h taint_engine.bpf.h capability.bpf.h channel.bpf.h; do
    cp "$VENDORED_ENGINE_DIR/$engine_file" "$SOURCE_DIR/bpf/$engine_file"
done
cp "$VENDORED_ENGINE_DIR/prebuilt/process.bpf.o" "$SOURCE_DIR/bpf/prebuilt/process.bpf.o"
cp "$VENDORED_ENGINE_DIR/prebuilt/process-inode-only.bpf.o" \
    "$SOURCE_DIR/bpf/prebuilt/process-inode-only.bpf.o"

SOURCE_DIR=$(CDPATH= cd -- "$SOURCE_DIR" && pwd)
mkdir -p "$TARGET_ROOT"
TARGET_ROOT=$(realpath "$TARGET_ROOT")
PRIVATE_DIR=$(mktemp -d "$TARGET_ROOT/enforcer.XXXXXXXXXX")
trap 'rm -rf "$PRIVATE_DIR"' EXIT
trap 'exit 1' HUP INT TERM
PRIVATE_TARGET="$PRIVATE_DIR/target"
cd "$AGENTSIGHT_ROOT"
# An explicit host target also overrides build.target in Cargo configuration.
HOST_TARGET=$(rustc -vV | sed -n 's/^host: //p')
if [ -z "$HOST_TARGET" ]; then
    echo "could not determine the native Rust target" >&2
    exit 1
fi
CARGO_TARGET_DIR="$PRIVATE_TARGET" "$CARGO" build --release -p agentsight-enforcer \
    --target-dir "$PRIVATE_TARGET" --target "$HOST_TARGET" \
    --no-default-features --features actplane \
    --config "patch.\"$ACTPLANE_REPOSITORY\".ebpf-ifc-engine.path=\"$SOURCE_DIR/bpf\""

ACTUAL_BPF_LIB_BLOB=$(git -C "$SOURCE_DIR" hash-object bpf/src/lib.rs)
ACTUAL_BPF_BUILD_BLOB=$(git -C "$SOURCE_DIR" hash-object bpf/build.rs)
ACTUAL_BPF_MAKEFILE_BLOB=$(git -C "$SOURCE_DIR" hash-object bpf/Makefile)
if [ "$ACTUAL_BPF_LIB_BLOB" != "$ACTPLANE_PATCHED_BPF_LIB_BLOB" ] \
    || [ "$ACTUAL_BPF_BUILD_BLOB" != "$ACTPLANE_PATCHED_BPF_BUILD_BLOB" ] \
    || [ "$ACTUAL_BPF_MAKEFILE_BLOB" != "$ACTPLANE_PATCHED_BPF_MAKEFILE_BLOB" ]; then
    echo "attested ActPlane sources mutated during the build" >&2
    exit 1
fi

# Artifact-level assertion: the objects embedded by the enforcer must still be
# the pinned files after the build.
STAGED_BLOB=$(git -C "$SOURCE_DIR" hash-object "$SOURCE_DIR/bpf/prebuilt/process.bpf.o")
STAGED_INODE_BLOB=$(git -C "$SOURCE_DIR" hash-object "$SOURCE_DIR/bpf/prebuilt/process-inode-only.bpf.o")
if [ "$STAGED_BLOB" != "$ACTPLANE_STAGED_PREBUILT_BPF_BLOB" ] \
    || [ "$STAGED_INODE_BLOB" != "$ACTPLANE_STAGED_INODE_BPF_BLOB" ]; then
    echo "attested prebuilt objects mutated during the build" >&2
    exit 1
fi

ENFORCER_BIN="$PRIVATE_DIR/agentsight-enforcer"
python3 - "$PRIVATE_TARGET/$HOST_TARGET/release/agentsight-enforcer" "$ENFORCER_BIN" \
    "$SOURCE_DIR/bpf/prebuilt/process.bpf.o" \
    "$SOURCE_DIR/bpf/prebuilt/process-inode-only.bpf.o" <<'PY'
import hashlib
import pathlib
import sys

# Verify, publish and hash the same buffer; never reopen Cargo's executable.
binary = pathlib.Path(sys.argv[1]).read_bytes()
for object_path in sys.argv[3:]:
    path = pathlib.Path(object_path)
    if path.read_bytes() not in binary:
        raise SystemExit(f"attested BPF object is not embedded in {sys.argv[1]}: {path}")
output = pathlib.Path(sys.argv[2])
output.write_bytes(binary)
output.chmod(0o500)
receipt = pathlib.Path(str(output) + ".sha256")
receipt.write_text(hashlib.sha256(binary).hexdigest() + "\n", encoding="ascii")
receipt.chmod(0o400)
PY

rm -rf "$PRIVATE_TARGET"
flock -u 9
exec 9>&-
trap - EXIT HUP INT TERM
printf '%s\n' "$ENFORCER_BIN" >&3

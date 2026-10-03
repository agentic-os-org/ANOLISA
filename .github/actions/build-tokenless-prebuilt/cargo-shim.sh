#!/usr/bin/env bash
# Keep Maturin metadata on the host and route compilation through the release Cross profile.
set -euo pipefail

die() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 1
}

HOST_CARGO="${TOKENLESS_HOST_CARGO:?TOKENLESS_HOST_CARGO is required}"
CROSS_PROFILE_SCRIPT="${TOKENLESS_CROSS_PROFILE_SCRIPT:?TOKENLESS_CROSS_PROFILE_SCRIPT is required}"
PROFILE="${TOKENLESS_CROSS_PROFILE:?TOKENLESS_CROSS_PROFILE is required}"
EXPECTED_TARGET="${TOKENLESS_RUST_TARGET:?TOKENLESS_RUST_TARGET is required}"
EXPECTED_MANIFEST="${TOKENLESS_CARGO_MANIFEST:?TOKENLESS_CARGO_MANIFEST is required}"
PROJECT_ROOT="${TOKENLESS_CROSS_PROJECT_ROOT:?TOKENLESS_CROSS_PROJECT_ROOT is required}"
OUTPUT_REWRITER="${TOKENLESS_CARGO_OUTPUT_REWRITER:?TOKENLESS_CARGO_OUTPUT_REWRITER is required}"

case "$PROFILE/$EXPECTED_TARGET" in
    gnu2.17-x86_64/x86_64-unknown-linux-gnu | \
    gnu2.17-aarch64/aarch64-unknown-linux-gnu | \
    darwin11-aarch64/aarch64-apple-darwin) ;;
    *) die "Cross profile $PROFILE does not match target $EXPECTED_TARGET" ;;
esac
[ -x "$HOST_CARGO" ] || die "host Cargo is not executable: $HOST_CARGO"
[ -x "$CROSS_PROFILE_SCRIPT" ] || \
    die "Cross profile script is not executable: $CROSS_PROFILE_SCRIPT"
[ -d "$PROJECT_ROOT" ] || die "Cross project root is not a directory: $PROJECT_ROOT"
[ -f "$OUTPUT_REWRITER" ] || die "Cargo output rewriter is not a file: $OUTPUT_REWRITER"

case "${1:-}" in
    metadata | locate-project | --version | -V)
        exec "$HOST_CARGO" "$@"
        ;;
    build | rustc)
        command_name="$1"
        shift
        arguments=()
        while [ "$#" -gt 0 ]; do
            case "$1" in
                --target)
                    [ "$#" -ge 2 ] || die '--target requires a value'
                    [ "$2" = "$EXPECTED_TARGET" ] || \
                        die "Maturin requested target $2, expected $EXPECTED_TARGET"
                    shift 2
                    ;;
                --target=*)
                    requested_target="${1#--target=}"
                    [ "$requested_target" = "$EXPECTED_TARGET" ] || \
                        die "Maturin requested target $requested_target, expected $EXPECTED_TARGET"
                    shift
                    ;;
                --manifest-path)
                    [ "$#" -ge 2 ] || die '--manifest-path requires a value'
                    [ "$2" = "$EXPECTED_MANIFEST" ] || \
                        die "Maturin requested unexpected manifest path: $2"
                    [ "$PWD/Cargo.toml" = "$EXPECTED_MANIFEST" ] || \
                        die "Maturin Cargo manifest is not in the current directory"
                    arguments+=(--manifest-path Cargo.toml)
                    shift 2
                    ;;
                --manifest-path=*)
                    requested_manifest="${1#--manifest-path=}"
                    [ "$requested_manifest" = "$EXPECTED_MANIFEST" ] || \
                        die "Maturin requested unexpected manifest path: $requested_manifest"
                    [ "$PWD/Cargo.toml" = "$EXPECTED_MANIFEST" ] || \
                        die "Maturin Cargo manifest is not in the current directory"
                    arguments+=(--manifest-path=Cargo.toml)
                    shift
                    ;;
                *)
                    arguments+=("$1")
                    shift
                    ;;
            esac
        done
        "$CROSS_PROFILE_SCRIPT" "$PROFILE" "$command_name" "${arguments[@]}" | \
            python3 "$OUTPUT_REWRITER" "$PROJECT_ROOT"
        ;;
    *)
        die "unsupported Maturin Cargo command: ${1:-<empty>}"
        ;;
esac

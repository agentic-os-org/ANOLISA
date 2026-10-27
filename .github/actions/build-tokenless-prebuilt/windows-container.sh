#!/usr/bin/env bash
# Invoked directly inside the frozen MSVC builder; never launches nested Docker.
set -euo pipefail

unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CC CXX AR CFLAGS CXXFLAGS LDFLAGS
export RUSTC_WRAPPER='' RUSTC_WORKSPACE_WRAPPER=''
mkdir -p "$HOME"
target=x86_64-pc-windows-msvc

case "$1" in
    build)
        bash /opt/builder/container.sh check
        # Keep the frozen SDK/tooling while compiling the unified CLI with its pinned Rust.
        export PATH="/tokenless-rust/bin:$PATH"
        rustc --version | tee /out/tokenless-rust-version.txt
        grep -q '^rustc 1.91.0 ' /out/tokenless-rust-version.txt
        cd /source
        cargo fetch --locked --target "$target"
        export CARGO_NET_OFFLINE=true
        # Portable CLI archives must not depend on a separately installed VC++ runtime.
        export RUSTFLAGS='-C link-arg=/Brepro -C target-feature=+crt-static'
        reproducible=(python3 /common/reproducible-build.py
            --source-root /source --source-date-epoch "$SOURCE_DATE_EPOCH" --)
        "${reproducible[@]}" cargo xwin build --locked --release --target "$target" -p tokenless-cli
        for binary in tokenless; do
            llvm-readobj --coff-imports "$CARGO_TARGET_DIR/$target/release/$binary.exe" \
                > "/out/$binary-imports.txt"
            if grep -Eiq 'Name: (vcruntime|msvcp|msvcr)[0-9]' "/out/$binary-imports.txt"; then
                printf 'ERROR: %s depends on a separately installed VC++ runtime\n' "$binary" >&2
                exit 1
            fi
        done
        # The extension uses the CRT supplied with Windows Python.
        export RUSTFLAGS='-C link-arg=/Brepro'
        # PowerShell requires the executable suffix when resolving the SDK's CLI function.
        cli_dir=/source/python/tokenless/python/anolisa_tokenless/_bin
        mkdir -p "$cli_dir"
        install -m 0755 "$CARGO_TARGET_DIR/$target/release/tokenless.exe" "$cli_dir/tokenless.exe"
        bash /source/scripts/stage-rtk-notices.sh "$cli_dir/third_party/rtk"
        trap 'rm -f "$cli_dir/tokenless.exe"' EXIT
        cd /source/python/tokenless
        "${reproducible[@]}" maturin build --locked --target "$target" \
            --profile python-release --out /wheels
        ;;
    smoke)
        trap '/usr/lib/wine/wineserver64 -k || true' EXIT
        # Windows Python requires pipe handles for its standard streams under Wine.
        printf '' | timeout --kill-after=10s 180s /usr/lib/wine/wine64 \
            /opt/python/tools/python.exe -X utf8 /action/windows-smoke.py \
            /artifacts /wheels "$2" 2>&1 | tee /out/wine-smoke.log
        ;;
    *) printf 'usage: windows-container.sh {build|smoke VERSION}\n' >&2; exit 2 ;;
esac

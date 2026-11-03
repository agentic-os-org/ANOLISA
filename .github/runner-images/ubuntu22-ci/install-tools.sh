#!/usr/bin/env bash
set -euo pipefail

# Match setup actions' tool-cache layout so selecting a version needs no download.
cache=/opt/hostedtoolcache
tools=/opt/anolisa/ci-tools
mkdir -p "$cache" "$tools/bin"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fetch() {
    curl -fsSL --retry 3 --max-time 300 "$1" -o "$tmp/archive.tar.gz"
    printf '%s  %s\n' "$2" "$tmp/archive.tar.gz" | sha256sum -c -
}
install_binary() {
    local name=$1 url=$2 checksum=$3
    fetch "$url" "$checksum"
    mkdir "$tmp/$name"
    tar -xzf "$tmp/archive.tar.gz" -C "$tmp/$name"
    install -m 0755 "$(find "$tmp/$name" -type f -name "$name" -print -quit)" "$tools/bin/$name"
}
install_binary cargo-tarpaulin https://github.com/xd009642/tarpaulin/releases/download/0.37.2/cargo-tarpaulin-x86_64-unknown-linux-musl.tar.gz 837dc54d21da101e96e888c6f6680d25f3c06554b0614880ea385cdf68cd5173
install_binary cargo-llvm-cov https://github.com/taiki-e/cargo-llvm-cov/releases/download/v0.9.1/cargo-llvm-cov-x86_64-unknown-linux-musl.tar.gz 3fca950394a3c49457657c158b1619cec8dfd2647ae5b48746734c0ab969a522
install_binary just https://github.com/casey/just/releases/download/1.58.0/just-1.58.0-x86_64-unknown-linux-musl.tar.gz 4a5cc2f53e6f0f8c59092a6cc38291eb729d46a7dd95d3ae582008881b84931d

install_node() {
    local version=$1 url=$2 checksum=$3
    fetch "$url" "$checksum"
    mkdir -p "$cache/node/$version/x64"
    tar -xzf "$tmp/archive.tar.gz" -C "$cache/node/$version/x64"
    test -x "$cache/node/$version/x64/bin/node"
    touch "$cache/node/$version/x64.complete"
}
install_node 20.20.2 https://github.com/actions/node-versions/releases/download/20.20.2-23521894959/node-20.20.2-linux-x64.tar.gz cc47fc6eb872f905f48c85397856e8a097b2020bb65b394e728af606bfb6e1a9
install_node 22.23.2 https://github.com/actions/node-versions/releases/download/22.23.2-30508424155/node-22.23.2-linux-x64.tar.gz eebfee5f7ded97a9364ebf5275923c62d3b09e98df2334bd1ef65b5e6b0a8e2d

# uv's cache uses x86_64 rather than the Node/Python x64 architecture name.
fetch https://github.com/astral-sh/uv/releases/download/0.13.0/uv-x86_64-unknown-linux-gnu.tar.gz 1468ebd5a5541121837c5a2817b9972ba6090fa6caa3d142620850a47fb75154
mkdir -p "$cache/uv/0.13.0/x86_64"
tar -xzf "$tmp/archive.tar.gz" --strip-components=1 -C "$cache/uv/0.13.0/x86_64"
touch "$cache/uv/0.13.0/x86_64.complete"
install -m 0755 "$cache/uv/0.13.0/x86_64/uv" "$tools/bin/uv"
install -m 0755 "$cache/uv/0.13.0/x86_64/uvx" "$tools/bin/uvx"

fetch https://github.com/astral-sh/uv/releases/download/0.11.7/uv-x86_64-unknown-linux-gnu.tar.gz 6681d691eb7f9c00ac6a3af54252f7ab29ae72f0c8f95bdc7f9d1401c23ea868
mkdir -p "$cache/uv/0.11.7/x86_64"
tar -xzf "$tmp/archive.tar.gz" --strip-components=1 -C "$cache/uv/0.11.7/x86_64"
touch "$cache/uv/0.11.7/x86_64.complete"

fetch https://github.com/actions/python-versions/releases/download/3.11.17-36876195594/python-3.11.17-linux-22.04-x64.tar.gz 9f7233ce8c4dacb2112c52842d6eb21be6e43907234f74c5a34f29bbba441eab
mkdir "$tmp/python"
tar -xzf "$tmp/archive.tar.gz" -C "$tmp/python"
(cd "$tmp/python" && AGENT_TOOLSDIRECTORY="$cache" bash ./setup.sh)

# System-installed virtual environments must not depend on runner-private paths.
rm -rf /home/runner/.local/share/uv/python /home/runner/.local/bin/python3.11
UV_PYTHON_INSTALL_DIR="$tools/uv-python" UV_PYTHON_BIN_DIR="$tools/bin" \
    "$tools/bin/uv" python install 3.11.6
UV_TOOL_DIR="$tools/uv-tools" UV_TOOL_BIN_DIR="$tools/bin" "$tools/bin/uv" tool install --python "$cache/Python/3.11.17/x64/bin/python3" diff-cover==10.6.0

# Preserve component MSRV checks rather than replacing them with the default stable.
export CARGO_HOME=/home/runner/.cargo RUSTUP_HOME=/home/runner/.rustup
export RUSTUP_DIST_SERVER=https://static.rust-lang.org
export RUSTUP_UPDATE_ROOT=https://static.rust-lang.org/rustup
export PATH="$CARGO_HOME/bin:$PATH"
for version in 1.86.0 1.88.0 1.89.0 1.91.0 1.93.0 1.93.1 1.97.1 1.98.0; do
    rustup toolchain install "$version" --profile minimal --component rustfmt --component clippy
done
for version in 1.89.0 1.93.0 1.93.1; do
    rustup component add llvm-tools-preview --toolchain "$version"
done
cargo +1.98.0 install cargo-sbom --version 0.10.0 --locked --root "$tools"
# Keep installed tools, but don't bake their build/download caches into the image.
rm -rf "$CARGO_HOME/registry" "$CARGO_HOME/git" /home/runner/.cache/uv /root/.cache/uv

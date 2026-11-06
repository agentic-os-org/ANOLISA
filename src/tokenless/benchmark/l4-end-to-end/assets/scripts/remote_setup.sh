#!/usr/bin/env bash
# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
#
# Idempotent remote environment setup for the L4 end-to-end comparison.
#
# Required environment:
#   L4_SSH_HOST   remote host or IP
# Optional:
#   L4_SSH_PASS   ssh password; only needed to bootstrap a host that does not
#                 have the eval key yet (never hard-coded here)
#   L4_SSH_USER   remote user             (default: root)
#   L4_REMOTE_WORK remote workspace root  (default: /root/l4)
#   L4_NODE_VERSION pinned Node release   (default: v24.20.0)
#   L4_PY_VERSION pinned CPython for the swe-runner venv (default: 3.12.14)
#   L4_OPENCLAW_VERSION pinned OpenClaw release (default: the one l4_env.sh names)
#   L4_HEADROOM_CONSTRAINTS  pip constraints file ON the host for the headroom
#                 venv, e.g. the `pip freeze --exclude-editable` of the environment
#                 the lower layers measured headroom in (default: unconstrained)
#
# ISOLATION: everything this script installs lands under $L4_REMOTE_WORK -- Node
# and its global npm prefix ($WORK/node), the Rust toolchains ($WORK/cargo,
# $WORK/rustup), pip configuration, and a private HOME ($WORK/home) that holds
# ~/.openclaw, ~/.local and ~/.tokenless for the benchmark only. The host's
# /usr/local/bin, /opt/node, /root/.openclaw, /root/.cargo, /root/.pip and
# /root/.bashrc are never written, so the harness can share a machine that runs
# its own OpenClaw. System packages are verified, not installed: a missing
# prerequisite fails the setup instead of changing the host. The resulting
# environment is defined once, in l4_env.sh, and every script that runs on the
# host sources it.
#
# Expects the source trees to be present under $L4_REMOTE_WORK already:
#   tokenless/  headroom/  runner/  swe-runner/ (the prompt corpus clone)
# Syncing them is a separate concern, mirroring L2's remote_sync.sh split.
#
# TARGET OS: Alibaba Cloud Linux 4 (dnf/rpm, Python 3.11.6 from the distro).
# The distro interpreter runs everything except the swe-runner venv, which needs
# >=3.12 and gets its own; see section 9.
# An earlier revision targeted Debian/Ubuntu and drove apt-get; the package
# manager and every package name below changed with the host family. Package
# names are the ones actually present in the alinux4 repos, verified against a
# live host rather than translated from the Debian list by guesswork.
#
# NETWORK: nodejs.org, static.rust-lang.org and registry.npmjs.org are reached
# directly. This reverses the previous revision, which routed those fetches
# through mirrors on an earlier evaluation host. Hugging Face remains the
# exception: run/verify/evaluate use the configurable HF_ENDPOINT mirror because
# huggingface.co is blocked from these hosts. pypi and crates.io stay mirrored
# purely for transfer speed. The distinction matters for one specific reason
# recorded at step 3.
#
# The heavy work (toolchain installs, cargo/maturin builds, npm installs) runs
# for tens of minutes, so it is NOT executed inside one long-lived ssh session.
# An inner script is uploaded, launched under nohup, and a small status file is
# polled with short ssh calls — the same shape as L2's remote_setup.sh.
#
# PROVENANCE: this script consolidates a sequence of throwaway stage scripts
# that were executed by hand, in order, against one host of a different OS
# family. Its idempotency guards are reasoned rather than proven. Treat the
# first clean-host run as a test of this script, and fix it here rather than by
# hand.

set -euo pipefail

: "${L4_SSH_HOST:?L4_SSH_HOST is required (remote host or IP)}"
# Needed only to bootstrap a host that does not have the key yet; see below.
L4_SSH_PASS="${L4_SSH_PASS:-}"
L4_SSH_USER="${L4_SSH_USER:-root}"
L4_REMOTE_WORK="${L4_REMOTE_WORK:-/root/l4}"
L4_NODE_VERSION="${L4_NODE_VERSION:-v24.20.0}"
# swe-runner requires >=3.12; see the section that installs it for why this is
# pinned to a patch version rather than tracking 3.12.
L4_PY_VERSION="${L4_PY_VERSION:-3.12.14}"

# Reuse the verified default SSH identity; an explicit key is used only when the
# caller names one.
SSH_OPTS=(-o StrictHostKeyChecking=yes -o BatchMode=yes -o ConnectTimeout=30 -o LogLevel=ERROR)
L4_SSH_KEY=${L4_SSH_KEY:-}
SSH_PW=()
if [ -n "$L4_SSH_KEY" ]; then
    [ -f "$L4_SSH_KEY" ] || { printf 'explicit SSH key missing: %s\n' "$L4_SSH_KEY" >&2; exit 1; }
    SSH_OPTS+=(-i "$L4_SSH_KEY" -o IdentitiesOnly=yes)
fi

remote_ssh() {
    ${SSH_PW[@]+"${SSH_PW[@]}"} ssh "${SSH_OPTS[@]}" "$L4_SSH_USER@$L4_SSH_HOST" "$@"
}

# --- 1. Upload the idempotent inner setup script -----------------------------
# The heredoc is quoted so nothing expands locally; every $VAR resolves on the
# remote when the script actually runs.
echo "[setup] uploading remote setup script"
remote_ssh "mkdir -p $L4_REMOTE_WORK/logs && cat > $L4_REMOTE_WORK/remote_setup_inner.sh" <<'INNER'
#!/usr/bin/env bash
# Heavy, idempotent remote setup. Runs under nohup; each step streams to its own
# log under $WORK/logs/. Final state lands in setup.status as "DONE" or
# "FAIL:<step>".
set -uo pipefail

WORK="${L4_REMOTE_WORK:-/root/l4}"
NODE_VERSION="${L4_NODE_VERSION:-v24.20.0}"
PY_VERSION="${L4_PY_VERSION:-3.12.14}"
LOG="$WORK/logs"
STATUS="$LOG/setup.status"
mkdir -p "$LOG"
: > "$STATUS"

fail() {
    echo "FAIL:$1" > "$STATUS"
    echo "[inner] FAILED at step: $1 (see $LOG)" >&2
    exit 1
}

step() { echo "[inner] === $1 ==="; }

# Brings a freshly created venv's pip up to a current upstream release.
#
# --no-cache-dir is load-bearing here, not hygiene. The pip that ensurepip bundles
# on this distro carries a mis-applied patch to its vendored urllib3: in
# _vendor/urllib3/response.py the `len(self._decoded_buffer) >= amt` test sits
# outside the `if amt is not None:` guard it belongs in, so any read() with no
# explicit amount raises TypeError. cachecontrol issues exactly one such read --
# in its "304 Not Modified" branch -- so triggering it requires a cached entry
# with an ETag to revalidate, meaning the crash only appears once the HTTP cache
# is warm. That is what made it look mirror- or host-specific: the first venv
# built on a fresh box filled a cold cache and succeeded, and the next one died
# on its first 304.
#
# Skipping the cache keeps pip on a plain adapter that never revalidates. Only
# this call needs it: once it returns, pip is a clean upstream wheel from the
# mirror and every later install in that venv can use the cache normally.
pip_bootstrap() { "$1/bin/pip" install --no-cache-dir --upgrade pip setuptools wheel; }

# Private prefixes and HOME; see ISOLATION in the header. Every later step reads
# these instead of the host locations it used to write.
ENV_SH="$WORK/anolisa/src/tokenless/benchmark/l4-end-to-end/assets/scripts/l4_env.sh"
[ -f "$ENV_SH" ] || fail "l4_env_missing (run remote_sync.sh first)"
# shellcheck source=l4_env.sh
. "$ENV_SH" "$WORK"
NODE_DIR="$WORK/node"
export npm_config_prefix="$NODE_DIR" npm_config_cache="$WORK/npm-cache" npm_config_userconfig="$WORK/npmrc"
export PIP_CONFIG_FILE="$WORK/pip.conf"
mkdir -p "$HOME" "$CARGO_HOME" "$RUSTUP_HOME"
export CARGO_TERM_COLOR=never

# --- 1. base OS prerequisites, verified only --------------------------------
# A shared host is not ours to change, so nothing is installed system-wide: the
# build tools must already be present, and patchelf -- which maturin needs to set
# the rpath on headroom's compiled _core extension (without it the proxy imports
# as pure Python and fails at `import headroom._core`) -- comes from PyPI into a
# private tools venv instead of the distro.
step "base prerequisites"
for tool in gcc g++ make pkg-config git rsync curl tar xz patch python3; do
    command -v "$tool" >/dev/null 2>&1 || fail "missing_prerequisite_$tool"
done
[ -f /usr/include/openssl/ssl.h ] || fail missing_prerequisite_openssl_devel
python3 -m venv --help >/dev/null 2>&1 || fail python3_venv_missing
if [ ! -x "$WORK/venv-tools/bin/patchelf" ]; then
    rm -rf "$WORK/venv-tools"
    python3 -m venv "$WORK/venv-tools" || fail venv_tools
    "$WORK/venv-tools/bin/pip" install --no-cache-dir patchelf \
        > "$LOG/patchelf_install.log" 2>&1 || fail patchelf_install
fi
"$WORK/venv-tools/bin/patchelf" --version > "$LOG/patchelf_version.txt" 2>&1 || fail patchelf_missing

# --- 2. container runtime ---------------------------------------------------
# swe-runner executes every instance inside its SWE-bench image, so docker is a
# hard precondition. It is a host service, so it is required, not provisioned.
step "docker"
docker info > "$LOG/docker_info.log" 2>&1 || fail docker_unusable

# --- 3. package index configuration, private ---------------------------------
# Only pypi and crates.io are mirrored, and purely for transfer speed -- both
# serve identical artefacts, so the component under test is unaffected.
#
# RUSTUP_DIST_SERVER is deliberately NOT set. The aliyun rustup mirror does not
# carry the 1.95.0 dist that headroom pins (404 on the dist path) while
# static.rust-lang.org does, so pointing rustup at the mirror is what previously
# forced the build onto an unpinned stable toolchain. Mirroring rustup would
# change *which compiler* builds the comparison side; mirroring pypi does not.
#
# Written into $CARGO_HOME and $WORK/pip.conf, which only this workspace reads.
step "package indexes"
printf '[source.crates-io]\nreplace-with = "aliyun"\n[source.aliyun]\nregistry = "sparse+https://mirrors.aliyun.com/crates.io-index/"\n' \
    > "$CARGO_HOME/config.toml"
printf '[global]\nindex-url = https://mirrors.aliyun.com/pypi/simple/\ntrusted-host = mirrors.aliyun.com\n' \
    > "$PIP_CONFIG_FILE"

# --- 4. Rust toolchain, including headroom's pinned version -----------------
# Two toolchains are installed on purpose. tokenless builds with stable; the
# headroom proxy's rust-toolchain.toml pins an exact channel and maturin honours
# that file, so the pin must be installed or the build fails outright. The pin is
# read from the file rather than hardcoded so a headroom bump does not silently
# fall back to stable here.
step "rust toolchain"
if ! "$CARGO_HOME/bin/rustc" --version >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        -o "$WORK/rustup-init" > "$LOG/rustup_fetch.log" 2>&1 || fail rustup_fetch
    chmod +x "$WORK/rustup-init"
    "$WORK/rustup-init" -y --no-modify-path --default-toolchain stable \
        > "$LOG/rustup_init.log" 2>&1 || fail rustup_init
fi
"$CARGO_HOME/bin/rustc" --version || fail rustc_missing

HEADROOM_PIN=$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' \
    "$WORK/headroom/rust-toolchain.toml" 2>/dev/null | head -1)
[ -n "$HEADROOM_PIN" ] || fail headroom_pin_unreadable
echo "[inner] headroom pins rust $HEADROOM_PIN"
if ! "$CARGO_HOME/bin/rustc" "+$HEADROOM_PIN" --version >/dev/null 2>&1; then
    # The pin's components (rustfmt, clippy) come along because the toolchain
    # file names them; rustup reads it when the toolchain is installed by name.
    "$CARGO_HOME/bin/rustup" toolchain install "$HEADROOM_PIN" \
        > "$LOG/rustup_pin.log" 2>&1 || fail rustup_pin_install
fi
"$CARGO_HOME/bin/rustc" "+$HEADROOM_PIN" --version > "$LOG/headroom_toolchain.txt" 2>&1 \
    || fail headroom_pin_missing

# --- 5. Node that satisfies openclaw's engine range -------------------------
# alinux4 has no nodejs package at a usable version, so Node is installed from
# the official tarball regardless. The version is pinned rather than discovered:
# a moving Node version would make results non-reproducible. openclaw requires
# >=22.22.3 <23 || >=24.15.0 <25 || >=25.9.0. It lives in $NODE_DIR and is
# reached through PATH only, never through a /usr/local/bin symlink.
step "node $NODE_VERSION"
if [ "$("$NODE_DIR/bin/node" -v 2>/dev/null)" != "$NODE_VERSION" ]; then
    cd "$WORK" || fail cd_work
    curl -sSL --max-time 900 -o node.tar.xz \
        "https://nodejs.org/dist/$NODE_VERSION/node-$NODE_VERSION-linux-x64.tar.xz" \
        > "$LOG/node_fetch.log" 2>&1 || fail node_fetch
    tar xJf node.tar.xz || fail node_untar
    rm -rf "$NODE_DIR" && mv "node-$NODE_VERSION-linux-x64" "$NODE_DIR"
    rm -f node.tar.xz
fi
"$NODE_DIR/bin/node" -v || fail node_missing

# Generous retries and timeout rather than a registry substitution: the default
# registry serves tarballs from these hosts at ~1.4 MB/s, and a mirror could lag
# behind on the openclaw version under test.
npm config set fetch-retries 5
npm config set fetch-timeout 600000

# --- 6. openclaw, with install scripts allowed ------------------------------
# npm's allowScripts gate is the subtle one: a plain `npm install -g openclaw`
# succeeds, but bundled plugins are never materialised and koffi /
# tree-sitter-bash never build their native bits, so failures surface much later
# as missing-plugin errors. The package list is explicit rather than a blanket
# opt-out so a new transitive dependency cannot silently gain script execution.
#
# The guard names the private binary and the release l4_env.sh pins: neither a
# host-wide openclaw on PATH nor whatever npm currently tags latest may satisfy it.
# A workspace config another release wrote is refused rather than migrated in
# place; removing $HOME/.openclaw lets the pinned release provision it afresh.
step "openclaw $L4_OPENCLAW_VERSION"
openclaw_release() { awk '$1 == "OpenClaw" { print $2; exit }'; }
if [ "$("$NODE_DIR/bin/openclaw" --version 2>/dev/null | openclaw_release)" != "$L4_OPENCLAW_VERSION" ]; then
    npm install -g \
        --allow-scripts=openclaw,@google/genai,koffi,tree-sitter-bash,protobufjs \
        "openclaw@$L4_OPENCLAW_VERSION" > "$LOG/openclaw_install.log" 2>&1 || fail openclaw_install
fi
"$NODE_DIR/bin/openclaw" --version > "$LOG/openclaw_version.txt" 2>&1 || fail openclaw_missing
[ "$(openclaw_release < "$LOG/openclaw_version.txt")" = "$L4_OPENCLAW_VERSION" ] || fail openclaw_version
[ "$(command -v openclaw)" = "$NODE_DIR/bin/openclaw" ] || fail openclaw_not_private
touched=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1])).get("meta", {}).get("lastTouchedVersion", ""))' \
    "$HOME/.openclaw/openclaw.json" 2>/dev/null || true)
if [ -n "$touched" ] && [ "$touched" != "$L4_OPENCLAW_VERSION" ]; then
    echo "[inner] $HOME/.openclaw/openclaw.json was written by OpenClaw $touched" >&2
    fail openclaw_config_from_other_release
fi

# headroom's openclaw plugin keeps headroom-ai as a runtime npm dependency
# rather than bundling it.
npm ls -g headroom-ai >/dev/null 2>&1 || \
    npm install -g headroom-ai > "$LOG/headroom_npm.log" 2>&1 || fail headroom_npm

# --- 7. tokenless ------------------------------------------------------------
# tokenless links RTK and TOON as library crates, so one cargo build produces the
# whole binary under test. RTK is a path dependency on third_party/rtk, which is
# not in the synced tree: it is a gitignored clone, and remote_sync.sh excludes it
# so that a developer's local copy cannot silently stand in for the pinned one.
# It therefore has to be provisioned here, before cargo can even resolve the
# workspace, and `just setup-rtk` is what does it -- the same recipe the
# component's own build uses, cloning the tag named in the justfile and applying
# the tracked patches under third_party/patches. Driving it through just instead
# of reimplementing the clone keeps that tag in one place.
#
# just is a build driver, not a component under test, so its version is left
# unpinned: RTK's bits come from a git tag plus tracked patches and are identical
# whichever just applies them.
step "tokenless"
cd "$WORK/tokenless" || fail cd_tokenless
if [ ! -x "$CARGO_HOME/bin/just" ]; then
    cargo install just > "$LOG/install_just.log" 2>&1 || fail install_just
fi
just setup-rtk > "$LOG/setup_rtk.log" 2>&1 || {
    # The recipe refuses a checkout from another pin or patch set and names
    # `just clean-rtk` as the fix. Here the checkout is a derived artifact of the
    # synced revision, so it is replaced once; any other failure stays fatal.
    grep -q "just clean-rtk" "$LOG/setup_rtk.log" || fail setup_rtk
    { just clean-rtk && just setup-rtk; } >> "$LOG/setup_rtk.log" 2>&1 || fail setup_rtk
}
# Checked explicitly so a recipe that exits 0 without a checkout fails here, not
# as an unresolvable path dependency in the middle of the build log.
[ -f third_party/rtk/Cargo.toml ] || fail rtk_tree_missing
# Recorded so the report can tie its numbers to the exact embedded RTK revision.
git -C third_party/rtk rev-parse HEAD > "$LOG/rtk_commit.txt" 2>&1 || true
cargo build --release --locked > "$LOG/build_tokenless.log" 2>&1 \
    || fail build_tokenless
sha256sum target/release/tokenless > "$LOG/tokenless_binary.sha256" \
    || fail tokenless_fingerprint

# --- 8. headroom venv -------------------------------------------------------
# headroom-ai builds through maturin (it ships a Rust proxy in
# crates/headroom-proxy) and its rust-toolchain.toml pins an exact channel.
# Step 4 installed that pin, and RUSTUP_TOOLCHAIN is deliberately NOT set here:
# letting maturin read the toolchain file means the comparison side is built with
# the compiler its authors specify. An earlier revision had to override the pin
# with stable because the mirror lacked the dist, and that override was a
# reported deviation in the L4 limitations; installing the pin removes it.
#
# PIP_PROGRESS_BAR=off and -v are deliberate: an earlier attempt piped output
# through `tail`, which buffered everything away, so a 27-minute hang looked
# like silence instead of showing where it blocked.
step "headroom venv"
# headroom pins its own source revision but only ranges for its dependencies, so
# an unconstrained install resolves whatever is newest that day -- and with
# litellm's release cadence the resolver can backtrack for a long time. A
# constraints file reproduces a known environment instead; the resolved set is
# recorded either way.
HR_CONSTRAINTS=()
if [ -n "${L4_HEADROOM_CONSTRAINTS:-}" ]; then
    [ -f "$L4_HEADROOM_CONSTRAINTS" ] || fail headroom_constraints_missing
    cp "$L4_HEADROOM_CONSTRAINTS" "$LOG/headroom_constraints.txt" || fail headroom_constraints_copy
    HR_CONSTRAINTS=(-c "$LOG/headroom_constraints.txt")
fi
# 配置身份包括源码 revision 与约束内容；已有环境不能仅凭 import 成功复用。
HR_REFERENCE="$(git -C "$WORK/headroom" rev-parse HEAD 2>/dev/null || echo unknown)"
if [ "${#HR_CONSTRAINTS[@]}" -gt 0 ]; then
    constraints_digest=$(sha256sum "$LOG/headroom_constraints.txt") || fail headroom_constraints_digest
    HR_REFERENCE="$HR_REFERENCE ${constraints_digest%% *}"
else
    HR_REFERENCE="$HR_REFERENCE unconstrained"
fi
HR_PREVIOUS=$(cat "$WORK/venv-headroom/.l4-reference" 2>/dev/null || true)
# Guard on the native extension, not just `import headroom`. The pure-Python
# package imports even when maturin failed to place _core.abi3.so, so guarding on
# `import headroom` let a venv missing the proxy extension pass and skip the
# rebuild -- exactly how the missing-patchelf breakage survived into the dry run.
if [[ "$HR_REFERENCE" == unknown\ * ]] || [ "$HR_PREVIOUS" != "$HR_REFERENCE" ] || ! "$WORK/venv-headroom/bin/python" -c "import headroom._core" >/dev/null 2>&1; then
    rm -rf "$WORK/venv-headroom"
    python3 -m venv "$WORK/venv-headroom" || fail venv_headroom
    pip_bootstrap "$WORK/venv-headroom" \
        > "$LOG/headroom_pip_base.log" 2>&1 || fail headroom_pip_base
    PIP_PROGRESS_BAR=off \
        "$WORK/venv-headroom/bin/pip" install -v ${HR_CONSTRAINTS[@]+"${HR_CONSTRAINTS[@]}"} \
        -e "$WORK/headroom[proxy]" > "$LOG/headroom_pip.log" 2>&1 || fail headroom_pip
fi
echo "[setup] headroom built with: $(cat "$LOG/headroom_toolchain.txt")"

# tree-sitter-language-pack gates headroom's --code-aware transform. The version
# is pinned to 0.13.0 because 1.16.x stopped shipping grammars in the wheel
# (2.4 MB) and downloads them at first use. Grammars fetched at runtime would
# also mean the parser under test is whatever was current that day, so the pin
# stays even though the download now works. Pinning here additionally keeps pip
# from resolving the `[code]` extra, which would pull a newer headroom-ai and
# change the component under test.
if ! "$WORK/venv-headroom/bin/python" -c \
        "from tree_sitter_language_pack import get_parser; get_parser('python')" >/dev/null 2>&1; then
    "$WORK/venv-headroom/bin/pip" install ${HR_CONSTRAINTS[@]+"${HR_CONSTRAINTS[@]}"} "tree-sitter-language-pack==0.13.0" \
        > "$LOG/tree_sitter.log" 2>&1 || fail tree_sitter
fi
"$WORK/venv-headroom/bin/pip" freeze > "$LOG/headroom_freeze.txt" 2>&1 || fail headroom_freeze
printf '%s\n' "$HR_REFERENCE" > "$WORK/venv-headroom/.l4-reference" || fail headroom_reference

# --- 9. swe-runner venv -----------------------------------------------------
# The interpreter has to be provisioned first. swe-runner declares
# requires-python >=3.12 while alinux4 ships 3.11.6 and carries no 3.12 package
# in any of its repositories -- os, plus and updates, checked with the disabled
# ones included. So the choice is between compiling CPython on four hosts and
# fetching a prebuilt one; uv's managed builds cost a ~33 MB download and under
# 30 seconds, and uv is already what swe-runner's own pyproject expects, since it
# declares [[tool.uv.index]].
#
# The patch version is pinned rather than tracking "3.12" so that all four hosts
# run their shards on the same interpreter. A floating minor would let a host
# provisioned later drift onto a different build, turning the interpreter into an
# uncontrolled variable in what is meant to be a paired comparison.
step "python $PY_VERSION via uv"
if [ ! -x "$WORK/venv-uv/bin/uv" ]; then
    rm -rf "$WORK/venv-uv"
    python3 -m venv "$WORK/venv-uv" || fail venv_uv
    # Still the distro's patched pip at this point, so --no-cache-dir for the
    # reason spelled out at pip_bootstrap.
    "$WORK/venv-uv/bin/pip" install --no-cache-dir uv \
        > "$LOG/uv_install.log" 2>&1 || fail uv_install
fi
"$WORK/venv-uv/bin/uv" python install "$PY_VERSION" \
    > "$LOG/uv_python.log" 2>&1 || fail uv_python
PY_SWE="$("$WORK/venv-uv/bin/uv" python find "$PY_VERSION" 2>/dev/null || true)"
[ -x "$PY_SWE" ] || fail uv_python_not_found
"$PY_SWE" -V > "$LOG/swe_python.txt" 2>&1
echo "[inner] swe-runner interpreter: $("$PY_SWE" -V 2>&1) at $PY_SWE"

# Installed from $WORK/runner, the symlink remote_sync.sh points at the synced
# monorepo checkout. $WORK/swe-runner is a different tree (the prompt corpus)
# and installing from it would silently benchmark a different runner revision.
#
# The guard checks *which tree* the venv imports, not merely that the entry point
# runs. An earlier version tested `swe-runner --help` alone and that guard failed
# in exactly the way that matters: a venv left over from the hand-run stage
# scripts answered --help perfectly while resolving to the corpus clone, which has
# no --headroom flag. Every headroom arm would have degraded to a baseline and the
# idempotent re-run would have skipped the step that fixes it.
step "swe-runner venv"
runner_real="$(readlink -f "$WORK/runner" 2>/dev/null || true)"
imported="$("$WORK/venv-swe/bin/python" -c \
    'import swe_runner, pathlib; print(pathlib.Path(swe_runner.__file__).resolve())' \
    2>/dev/null || true)"
if [ -z "$runner_real" ] || [ ! -d "$runner_real" ]; then
    fail runner_tree_missing
fi
case "$imported" in
    "$runner_real"/*) swe_venv_ok=1 ;;
    *)                swe_venv_ok=0 ;;
esac
if [ "$swe_venv_ok" = "0" ]; then
    echo "[inner] reinstalling venv-swe: imports '${imported:-nothing}'," \
         "expected a path under $runner_real"
    # Removed rather than re-created in place: `python -m venv` over an existing
    # tree reuses it, so switching interpreter versions would leave the old
    # lib/python3.11 sitting beside a new lib/python3.12 and let imports resolve
    # out of either.
    rm -rf "$WORK/venv-swe"
    "$PY_SWE" -m venv "$WORK/venv-swe" || fail venv_swe
    pip_bootstrap "$WORK/venv-swe" \
        > "$LOG/swe_pip_base.log" 2>&1 || fail swe_pip_base
    "$WORK/venv-swe/bin/pip" install -e "$WORK/runner" \
        > "$LOG/swe_pip.log" 2>&1 || fail swe_pip
fi
# pytest lives in the runner's `dev` extra, not its runtime deps, and
# remote_sync.sh runs the unit suite as a gate before any run. Installed
# idempotently here rather than folded into the line above so a venv built by an
# earlier revision -- runtime deps only, so its import guard passes and the
# reinstall block is skipped -- still gets pytest on a re-run.
"$WORK/venv-swe/bin/python" -c "import pytest" >/dev/null 2>&1 || \
    "$WORK/venv-swe/bin/pip" install -e "$WORK/runner[dev]" \
        > "$LOG/swe_pip_dev.log" 2>&1 || fail swe_pip_dev
# Assert the flags the arms depend on are actually exposed. Without this the
# failure surfaces only as a run that looks successful and measures nothing.
#
# The help text is captured once and matched in-shell rather than piped into grep
# once per flag. Under `pipefail` a `cmd | grep -q` pipeline reports failure
# whenever grep matches early enough to close the pipe before cmd has finished
# writing: cmd dies of SIGPIPE and its 141 becomes the pipeline's status, so a
# flag that is present gets reported missing. That race failed three of four hosts
# here in one run, each on a different flag, while probing the same five flags
# through a captured variable succeeded 80 times out of 80.
swe_help="$("$WORK/venv-swe/bin/swe-runner" run --help 2>&1)" || fail swe_runner_help
for flag in --headroom --tokenless --per-case-prompt --prompts-dir --base-config; do
    case "$swe_help" in
        *"$flag"*) ;;
        *) fail "swe_runner_missing_flag$flag" ;;
    esac
done

# --- 10. tokenless openclaw extension ----------------------------------------
# swe-runner's sandbox probes these host locations:
#   binaries:  PATH, /usr/share/tokenless/bin, ~/.local/bin,
#              ~/.openclaw/extensions/tokenless/bin
#   extension: ~/.openclaw/extensions/tokenless
# INSTALL_PROFILE=user is spelled out because this runs as root, and the
# component's newer delivery entry point defaults root to the system profile
# (/usr/local); the user profile targets $HOME/.local, i.e. the private
# $WORK/home/.local/bin that l4_env.sh puts first on PATH.
#
# The umbrella `build` / `install` targets are skipped on purpose: `build` would
# rebuild both crates with a plain `cargo build --release`, discarding the
# --locked-first behaviour and the per-step logs of step 7, and `install` pulls in
# targets this host has no use for. Only the packaging targets are needed here.
# (`just setup-rtk` is not avoided -- step 7 calls it directly to provision the
# pinned rtk clone, which is not part of the synced tree.)
step "tokenless extension"
cd "$WORK/tokenless" || fail cd_tokenless2
# Always rebuild. The OpenClaw bundle's dist/ is excluded from the sync (a
# gitignored build artefact), and even a dist/ left by a prior run may predate
# the source remote_sync.sh just pushed. This does NOT apply to
# adapters/tokenless/dsh/dist/index.js, which is tracked and synced -- see the
# anchored exclude list in remote_sync.sh. The old file-exists guard is exactly
# what let a stale plugin be deployed and measured as the new one.
make build-openclaw-plugin > "$LOG/plugin_build.log" 2>&1 || fail plugin_build
make INSTALL_PROFILE=user install-binaries install-helpers install-adapter-resources \
    > "$LOG/plugin_install.log" 2>&1 || fail plugin_install
[ "$(command -v tokenless)" = "$HOME/.local/bin/tokenless" ] || fail tokenless_not_private
[ -f "$TOKENLESS_TOOL_READY_SPEC" ] || fail tokenless_spec_missing
# Record the exact bytes deployed so the report can tie numbers to a bundle. No
# 2>&1 || true: a failed sha256sum must not leave its error text in the file for
# provenance to mistake for a digest -- fail loudly instead.
sha256sum adapters/tokenless/openclaw/dist/index.js > "$LOG/tokenless_dist.sha256" || fail tokenless_sha256

# `make openclaw-install` runs detect.sh first, and detect.sh exits 1 when
# ~/.openclaw does not exist yet ("not installed (ready to install)"), aborting
# the target before the deploy step on a host where openclaw has never run.
# Creating the profile dir and calling install.sh directly is equivalent —
# install.sh shells out to `openclaw plugins install <path> --force`.
mkdir -p "$HOME/.openclaw/extensions"
SHARE="$L4_TOKENLESS_SHARE"
# Deploy unconditionally: install.sh passes --force, so this replaces an older
# deployed bundle. The old "skip if the extension dir exists" guard meant a
# rerun never picked up freshly built source. OPENCLAW_BIN is passed explicitly
# because install.sh puts /usr/local/bin ahead of PATH: on a host with its own
# openclaw there, that copy would register the plugin and rewrite the workspace
# config in its own schema version.
ANOLISA_TARGET=openclaw ANOLISA_COMPONENT=tokenless ANOLISA_ADAPTER_DIR="$SHARE" \
    OPENCLAW_BIN="$NODE_DIR/bin/openclaw" \
    bash "$SHARE/openclaw/scripts/install.sh" \
    > "$LOG/tokenless_deploy.log" 2>&1 || fail tokenless_deploy

# --- 11. headroom openclaw plugin -------------------------------------------
# The plugin ships as TypeScript source with no dist/, so it needs a tsup build
# first. Its prepare-dist.mjs keeps headroom-ai as a runtime dependency rather
# than bundling it, so dist/ gets its own node_modules copy — that makes module
# resolution independent of whichever global install happens to be present.
step "headroom plugin"
cd "$WORK/headroom/plugins/openclaw" || fail cd_headroom_plugin
# Always rebuild, same reasoning as the tokenless plugin: dist/ is not synced and
# a leftover build may be stale relative to the pushed source.
npm install --no-audit --no-fund > "$LOG/hr_plugin_npm.log" 2>&1 || fail hr_plugin_npm
npm run build > "$LOG/hr_plugin_build.log" 2>&1 || fail hr_plugin_build
mkdir -p dist/node_modules
# This copy is what lets dist/ resolve headroom-ai without a global install. A
# swallowed failure would defer the breakage to run time (arm C-F reach it
# through the proxy, not the venv), so fail loudly here where it is free.
cp -r node_modules/headroom-ai dist/node_modules/ > "$LOG/hr_plugin_copy.log" 2>&1 || fail hr_plugin_copy
sha256sum dist/index.js > "$LOG/headroom_dist.sha256" || fail headroom_sha256
# Deploy unconditionally with --force so a rerun replaces an older bundle.
openclaw plugins install "$PWD/dist" --force --accept-capabilities \
    > "$LOG/hr_plugin_deploy.log" 2>&1 || fail hr_plugin_deploy

# --- 12. arm isolation: both plugins installed, neither globally enabled ----
# headroom claims the exclusive "contextEngine" slot, so the arms must toggle
# plugins per run. swe-runner copies ~/.openclaw/openclaw.json (the private
# $WORK/home one) into each per-instance profile and only symlinks an extension
# into <profile>/extensions when the matching flag is set -- which means a plugin
# left enabled in that config would leak into the baseline arm and make every
# delta meaningless. Disabling both here makes the per-profile symlink the only
# activation path.
step "arm isolation"
openclaw plugins disable headroom  > "$LOG/disable_headroom.log"  2>&1 || true
openclaw plugins disable tokenless > "$LOG/disable_tokenless.log" 2>&1 || true
openclaw plugins list > "$LOG/plugins_list.log" 2>&1 || true

echo "DONE" > "$STATUS"
echo "[inner] setup complete"
INNER

# --- 2. Launch the setup in the background ----------------------------------
# The old status file is removed in the same call that launches, and before it.
# The inner script truncates it too, but the first poll races that truncation --
# and losing that race means reading the *previous* run's verdict, reporting a
# failure that has already been fixed, and exiting while the real setup carries
# on in the background. Deleting it here leaves no stale verdict to misread.
# Only the nohup'd command is backgrounded, inside the braces, with its stdin
# detached: a trailing `&` would background the whole `&&` list in a subshell
# that keeps this ssh session's descriptors open, holding the launch call
# for the full length of the setup instead of returning to the poll below.
echo "[setup] launching remote setup under nohup"
remote_ssh "rm -f $L4_REMOTE_WORK/logs/setup.status && \
    chmod +x $L4_REMOTE_WORK/remote_setup_inner.sh && \
    { L4_REMOTE_WORK=$L4_REMOTE_WORK L4_NODE_VERSION=$L4_NODE_VERSION \
      L4_PY_VERSION=$L4_PY_VERSION L4_OPENCLAW_VERSION=${L4_OPENCLAW_VERSION:-} \
      L4_HEADROOM_CONSTRAINTS=${L4_HEADROOM_CONSTRAINTS:-} \
      nohup bash $L4_REMOTE_WORK/remote_setup_inner.sh \
      > $L4_REMOTE_WORK/logs/setup.log 2>&1 < /dev/null & }" || true

# --- 3. Poll the status file with short ssh calls ----------------------------
# Each poll is its own short-lived ssh call, so no single command approaches the
# session timeout no matter how long the builds take.
#
# Every poll reports the step the host is currently on, because this loop used to
# print nothing at all between launch and verdict: a 16-minute setup looked
# identical to a dead one, and under the fleet driver -- which shows each host's
# last line -- all four hosts sat blank for the entire run. The step name is the
# part worth having; the elapsed counter beside it only proves the poll is alive.
# Both come from one ssh call, separated by '|', so reporting does not double the
# connection count.
echo "[setup] polling (this typically takes 30-60 minutes on a cold host)"
deadline=$(( SECONDS + 7200 ))
started=$SECONDS
while [ "$SECONDS" -lt "$deadline" ]; do
    probe="$(remote_ssh "L=$L4_REMOTE_WORK/logs; printf '%s|%s' \
        \"\$(cat \$L/setup.status 2>/dev/null)\" \
        \"\$(grep '^\[inner\] === ' \$L/setup.log 2>/dev/null | tail -1)\"" || true)"
    status=${probe%%|*}
    current=${probe#*|}
    current=${current#\[inner\] === }
    current=${current% ===}
    case "$status" in
        DONE)
            echo "[setup] remote setup complete in $((SECONDS - started))s"
            exit 0
            ;;
        FAIL:*)
            echo "[setup] remote setup failed: ${status#FAIL:}" >&2
            echo "[setup] logs: $L4_REMOTE_WORK/logs/ on $L4_SSH_HOST" >&2
            exit 1
            ;;
    esac
    printf '[setup] %ds elapsed, step: %s\n' "$((SECONDS - started))" \
        "${current:-starting up}"
    sleep 60
done

echo "[setup] timed out after 2h; inspect $L4_REMOTE_WORK/logs/ on $L4_SSH_HOST" >&2
exit 1

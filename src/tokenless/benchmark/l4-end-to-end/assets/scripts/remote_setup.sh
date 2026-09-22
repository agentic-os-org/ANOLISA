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

# Throwaway benchmark host: skip host-key pinning so reprovisioned machines do
# not break the pipeline.
SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR)

# Key first, sshpass only to bootstrap a host that does not have the key yet.
# sshpass allocates a pty per connection and that setup races under concurrency,
# which appears as ssh failing with "Bad file descriptor".
L4_SSH_KEY=${L4_SSH_KEY:-$HOME/.ssh/l4_eval_ed25519}
if [ -f "$L4_SSH_KEY" ]; then
    SSH_OPTS+=(-i "$L4_SSH_KEY" -o IdentitiesOnly=yes)
    SSH_PW=()
else
    : "${L4_SSH_PASS:?L4_SSH_PASS is required when $L4_SSH_KEY does not exist}"
    SSH_PW=(sshpass -p "$L4_SSH_PASS")
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

# openclaw's global bin lives in /opt/node/bin (step 5 installs Node there);
# /root/.cargo/bin carries the Rust toolchain and cargo-installed binaries.
export PATH=/usr/local/bin:/opt/node/bin:/root/.cargo/bin:$PATH
export CARGO_TERM_COLOR=never

# --- 1. base OS packages ----------------------------------------------------
# alinux4 preinstalls gcc, make, curl, ca-certificates, xz, python3-devel and
# pkgconf-pkg-config; the rest are missing on a clean image. Named individually
# because alinux4 has no build-essential metapackage, and `dnf install` of an
# already-present package is a no-op, so listing all of them stays idempotent.
# python3-venv does not exist here either -- venv ships inside python3-libs.
#
# patchelf is required by the headroom proxy build, not the base tooling: maturin
# uses it to set the rpath on the compiled _core extension and, without it, quietly
# leaves _core.abi3.so out of the package tree. The proxy then imports fine as a
# CLI (pure Python) but fails at `import headroom._core`, so arms c and d could not
# activate at all -- a failure that only surfaced in the dry run, eight steps later.
step "dnf base packages"
dnf -y install \
    gcc gcc-c++ make pkgconf-pkg-config openssl-devel \
    git rsync curl ca-certificates tar xz jq bc python3-devel patch patchelf \
    > "$LOG/dnf_install.log" 2>&1 || fail dnf_install
python3 -m venv --help >/dev/null 2>&1 || fail python3_venv_missing

# --- 2. container runtime ---------------------------------------------------
# swe-runner executes every instance inside its SWE-bench image, so docker is a
# hard precondition, not an optional extra. The distro package (24.0.9) is used
# rather than docker-ce: it is already in the alinux4 repos and needs no
# third-party repo setup. enable --now covers the reboot case; a clean image has
# the package absent AND the unit stopped, and only starting it would leave a
# host that works today and fails after the next restart.
step "docker"
if ! docker info >/dev/null 2>&1; then
    dnf -y install docker > "$LOG/dnf_docker.log" 2>&1 || fail dnf_docker
    systemctl enable --now docker > "$LOG/docker_enable.log" 2>&1 || fail docker_enable
fi
docker info > "$LOG/docker_info.log" 2>&1 || fail docker_unusable

# --- 3. package index configuration, persisted ------------------------------
# Only pypi and crates.io are mirrored, and purely for transfer speed -- both
# serve identical artefacts, so the component under test is unaffected.
#
# RUSTUP_DIST_SERVER is deliberately NOT set. The aliyun rustup mirror does not
# carry the 1.95.0 dist that headroom pins (404 on the dist path) while
# static.rust-lang.org does, so pointing rustup at the mirror is what previously
# forced the build onto an unpinned stable toolchain. Mirroring rustup would
# change *which compiler* builds the comparison side; mirroring pypi does not.
#
# Persisted to .bashrc so interactive debugging shells resolve the same
# toolchain and Node as the non-interactive run.
step "package indexes"
grep -q 'PATH=/usr/local/bin:/opt/node/bin' /root/.bashrc 2>/dev/null || \
    echo 'export PATH=/usr/local/bin:/opt/node/bin:/root/.cargo/bin:$PATH' >> /root/.bashrc
mkdir -p /root/.cargo /root/.pip
printf '[source.crates-io]\nreplace-with = "aliyun"\n[source.aliyun]\nregistry = "sparse+https://mirrors.aliyun.com/crates.io-index/"\n' \
    > /root/.cargo/config.toml
printf '[global]\nindex-url = https://mirrors.aliyun.com/pypi/simple/\ntrusted-host = mirrors.aliyun.com\n' \
    > /root/.pip/pip.conf

# --- 4. Rust toolchain, including headroom's pinned version -----------------
# Two toolchains are installed on purpose. tokenless builds with stable; the
# headroom proxy's rust-toolchain.toml pins an exact channel and maturin honours
# that file, so the pin must be installed or the build fails outright. The pin is
# read from the file rather than hardcoded so a headroom bump does not silently
# fall back to stable here.
step "rust toolchain"
if ! /root/.cargo/bin/rustc --version >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        -o /tmp/rustup-init > "$LOG/rustup_fetch.log" 2>&1 || fail rustup_fetch
    chmod +x /tmp/rustup-init
    /tmp/rustup-init -y --no-modify-path --default-toolchain stable \
        > "$LOG/rustup_init.log" 2>&1 || fail rustup_init
fi
/root/.cargo/bin/rustc --version || fail rustc_missing

HEADROOM_PIN=$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' \
    "$WORK/headroom/rust-toolchain.toml" 2>/dev/null | head -1)
[ -n "$HEADROOM_PIN" ] || fail headroom_pin_unreadable
echo "[inner] headroom pins rust $HEADROOM_PIN"
if ! /root/.cargo/bin/rustc "+$HEADROOM_PIN" --version >/dev/null 2>&1; then
    # The pin's components (rustfmt, clippy) come along because the toolchain
    # file names them; rustup reads it when the toolchain is installed by name.
    /root/.cargo/bin/rustup toolchain install "$HEADROOM_PIN" \
        > "$LOG/rustup_pin.log" 2>&1 || fail rustup_pin_install
fi
/root/.cargo/bin/rustc "+$HEADROOM_PIN" --version > "$LOG/headroom_toolchain.txt" 2>&1 \
    || fail headroom_pin_missing

# --- 5. Node that satisfies openclaw's engine range -------------------------
# alinux4 has no nodejs package at a usable version, so Node is installed from
# the official tarball regardless. The version is pinned rather than discovered:
# a moving Node version would make results non-reproducible. openclaw requires
# >=22.22.3 <23 || >=24.15.0 <25 || >=25.9.0.
step "node $NODE_VERSION"
if [ "$(/usr/local/bin/node -v 2>/dev/null)" != "$NODE_VERSION" ]; then
    cd "$WORK" || fail cd_work
    curl -sSL --max-time 900 -o node.tar.xz \
        "https://nodejs.org/dist/$NODE_VERSION/node-$NODE_VERSION-linux-x64.tar.xz" \
        > "$LOG/node_fetch.log" 2>&1 || fail node_fetch
    tar xJf node.tar.xz || fail node_untar
    rm -rf /opt/node && mv "node-$NODE_VERSION-linux-x64" /opt/node
    rm -f node.tar.xz
    # /usr/local/bin precedes /usr/bin, so these take precedence over anything
    # the distro might later install.
    ln -sf /opt/node/bin/node /usr/local/bin/node
    ln -sf /opt/node/bin/npm /usr/local/bin/npm
    ln -sf /opt/node/bin/npx /usr/local/bin/npx
fi
/usr/local/bin/node -v || fail node_missing

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
step "openclaw"
if ! openclaw --version >/dev/null 2>&1; then
    npm install -g \
        --allow-scripts=openclaw,@google/genai,koffi,tree-sitter-bash,protobufjs \
        openclaw > "$LOG/openclaw_install.log" 2>&1 || fail openclaw_install
fi
# Expose openclaw where the rest of the tooling looks for it.
ln -sf /opt/node/bin/openclaw /usr/local/bin/openclaw
openclaw --version || fail openclaw_missing

# headroom's openclaw plugin keeps headroom-ai as a runtime npm dependency
# rather than bundling it.
npm ls -g headroom-ai >/dev/null 2>&1 || \
    npm install -g headroom-ai > "$LOG/headroom_npm.log" 2>&1 || fail headroom_npm

# --- 7. tokenless + rtk + toon ----------------------------------------------
# rtk sits outside the tokenless workspace (excluded in its Cargo.toml), so it
# needs its own cargo invocation against third_party/rtk/Cargo.toml. toon is a
# separately published crate the pipeline shells out to; the version is pinned
# so the binary under test does not drift between runs.
step "tokenless + rtk + toon"
cd "$WORK/tokenless" || fail cd_tokenless
if [ ! -x target/release/tokenless ]; then
    cargo build --release --locked > "$LOG/build_tokenless.log" 2>&1 \
        || cargo build --release >> "$LOG/build_tokenless.log" 2>&1 \
        || fail build_tokenless
fi
# rtk is not in the synced tree: it is a gitignored clone, and remote_sync.sh
# excludes it so that a developer's local copy cannot silently stand in for the
# pinned one. It therefore has to be provisioned here, and `just setup-rtk` is
# what does it -- the same recipe the component's own build uses, cloning the tag
# named in the justfile and applying the tracked patches under
# third_party/patches. Driving it through just instead of reimplementing the clone
# keeps that tag in one place; a hand-rolled copy here would go on building
# v0.43.0 long after the justfile moved off it.
#
# just is a build driver, not a component under test, so its version is left
# unpinned: rtk's bits come from a git tag plus tracked patches and are identical
# whichever just applies them.
if [ ! -x /root/.cargo/bin/just ]; then
    cargo install just > "$LOG/install_just.log" 2>&1 || fail install_just
fi
just setup-rtk > "$LOG/setup_rtk.log" 2>&1 || fail setup_rtk
# Previously the build below was guarded by this same test and skipped silently
# when it failed, which surfaced eight steps later as `install -p rtk: No such
# file or directory` from an unrelated make target.
[ -f third_party/rtk/Cargo.toml ] || fail rtk_tree_missing
# Recorded so the report can tie its numbers to the exact rtk revision.
git -C third_party/rtk rev-parse HEAD > "$LOG/rtk_commit.txt" 2>&1 || true
if [ ! -x third_party/rtk/target/release/rtk ]; then
    cargo build --release --manifest-path third_party/rtk/Cargo.toml \
        > "$LOG/build_rtk.log" 2>&1 || fail build_rtk
fi
if [ ! -x /root/.cargo/bin/toon ]; then
    cargo install toon-format --version 0.5.0 --locked \
        > "$LOG/install_toon.log" 2>&1 || fail install_toon
fi

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
# Guard on the native extension, not just `import headroom`. The pure-Python
# package imports even when maturin failed to place _core.abi3.so, so guarding on
# `import headroom` let a venv missing the proxy extension pass and skip the
# rebuild -- exactly how the missing-patchelf breakage survived into the dry run.
if ! "$WORK/venv-headroom/bin/python" -c "import headroom._core" >/dev/null 2>&1; then
    rm -rf "$WORK/venv-headroom"
    python3 -m venv "$WORK/venv-headroom" || fail venv_headroom
    pip_bootstrap "$WORK/venv-headroom" \
        > "$LOG/headroom_pip_base.log" 2>&1 || fail headroom_pip_base
    PIP_PROGRESS_BAR=off \
        "$WORK/venv-headroom/bin/pip" install -v -e "$WORK/headroom[proxy]" \
        > "$LOG/headroom_pip.log" 2>&1 || fail headroom_pip
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
    "$WORK/venv-headroom/bin/pip" install "tree-sitter-language-pack==0.13.0" \
        > "$LOG/tree_sitter.log" 2>&1 || fail tree_sitter
fi

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
#   binaries:  /usr/share/tokenless/bin, ~/.local/bin,
#              ~/.openclaw/extensions/tokenless/bin
#   extension: ~/.openclaw/extensions/tokenless
# The Makefile's default INSTALL_PROFILE=user targets $HOME/.local, landing
# binaries in ~/.local/bin, which is already on that probe list.
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
make install-binaries install-helpers install-adapter-resources \
    > "$LOG/plugin_install.log" 2>&1 || fail plugin_install
# Record the exact bytes deployed so the report can tie numbers to a bundle. No
# 2>&1 || true: a failed sha256sum must not leave its error text in the file for
# provenance to mistake for a digest -- fail loudly instead.
sha256sum adapters/tokenless/openclaw/dist/index.js > "$LOG/tokenless_dist.sha256" || fail tokenless_sha256

# `make openclaw-install` runs detect.sh first, and detect.sh exits 1 when
# ~/.openclaw does not exist yet ("not installed (ready to install)"), aborting
# the target before the deploy step on a host where openclaw has never run.
# Creating the profile dir and calling install.sh directly is equivalent —
# install.sh shells out to `openclaw plugins install <path> --force`.
mkdir -p /root/.openclaw/extensions
SHARE=/root/.local/share/anolisa/adapters/tokenless
# Deploy unconditionally: install.sh passes --force, so this replaces an older
# deployed bundle. The old "skip if the extension dir exists" guard meant a
# rerun never picked up freshly built source.
ANOLISA_TARGET=openclaw ANOLISA_COMPONENT=tokenless ANOLISA_ADAPTER_DIR="$SHARE" \
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
# plugins per run. swe-runner copies ~/.openclaw/openclaw.json into each
# per-instance profile and only symlinks an extension into <profile>/extensions
# when the matching flag is set — which means a plugin left enabled *globally*
# would leak into the baseline arm and make every delta meaningless. Disabling
# both here makes the per-profile symlink the only activation path.
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
echo "[setup] launching remote setup under nohup"
remote_ssh "rm -f $L4_REMOTE_WORK/logs/setup.status && \
    chmod +x $L4_REMOTE_WORK/remote_setup_inner.sh && \
    L4_REMOTE_WORK=$L4_REMOTE_WORK L4_NODE_VERSION=$L4_NODE_VERSION \
    L4_PY_VERSION=$L4_PY_VERSION \
    nohup bash $L4_REMOTE_WORK/remote_setup_inner.sh \
    > $L4_REMOTE_WORK/logs/setup.log 2>&1 &" || true

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

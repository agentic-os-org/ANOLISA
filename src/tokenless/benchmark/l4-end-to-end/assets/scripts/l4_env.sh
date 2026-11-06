#!/usr/bin/env bash
# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
#
# Private environment of one L4 workspace, sourced by every script that runs on
# the benchmark host:
#
#   . "$W/anolisa/src/tokenless/benchmark/l4-end-to-end/assets/scripts/l4_env.sh" "$W"
#
# See ISOLATION in remote_setup.sh. One file, so that setup, verify, run, clean
# and evaluate cannot drift apart on which openclaw, tokenless or state
# directory they mean.

L4_WORK_ROOT=${1:?usage: . l4_env.sh <workspace-root>}

export HOME="${L4_HOME:-$L4_WORK_ROOT/home}"
export CARGO_HOME="$L4_WORK_ROOT/cargo" RUSTUP_HOME="$L4_WORK_ROOT/rustup"

# The inherited PATH is replaced, not extended: a host user's ~/.local/bin or
# nvm directory may carry another tokenless or openclaw, and both swe-runner's
# binary injection and the tokenless plugin resolve PATH before anything else.
# The workspace's own directories come first; the system ones only supply the
# base tools (docker, git, python3, curl).
export PATH="$HOME/.local/bin:$L4_WORK_ROOT/node/bin:$CARGO_HOME/bin:$L4_WORK_ROOT/venv-tools/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

# The OpenClaw release swe-runner's profile dialect and transcript reader were
# built against. npm's latest is not followed: 2026.11.1 rejects the agents.list
# profiles the runner writes, failing every instance before its first turn.
# Setup installs this release and verify checks it.
export L4_OPENCLAW_VERSION="${L4_OPENCLAW_VERSION:-2026.8.2}"

# tokenless resolves its defaults from the passwd home rather than $HOME, so a
# private HOME alone would still read the host user's tool-ready spec and env-fix
# script and write stats, stash and RTK receipts into /root/.tokenless. These
# overrides point every one of those at the workspace copy installed by
# remote_setup.sh.
export TOKENLESS_DATA_DIR="$HOME/.tokenless"
L4_TOKENLESS_SHARE="$HOME/.local/share/anolisa/adapters/tokenless"
export TOKENLESS_TOOL_READY_SPEC="$L4_TOKENLESS_SHARE/common/tool-ready-spec.json"
export TOKENLESS_ENV_FIX_SCRIPT="$L4_TOKENLESS_SHARE/common/tokenless-env-fix.sh"
# config.json has no path override and stays at /root/.tokenless/config.json, a
# file the host user owns. Its three toggles do have environment overrides, which
# take precedence, so they are pinned here: a host config with compression off
# would otherwise turn arm B into a dry run. Stats stay on because they are the
# arm's tool-result capture. SLS export appends to a host-owned /var/log file
# when one exists; it never changes what the model sees, so it is off rather than
# mixing benchmark records into the host's own telemetry.
export TOKENLESS_COMPRESSION_ENABLED=1 TOKENLESS_STATS_ENABLED=1 TOKENLESS_SLS_ENABLED=0

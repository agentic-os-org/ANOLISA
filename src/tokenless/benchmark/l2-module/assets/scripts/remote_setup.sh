#!/usr/bin/env bash
# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
#
# Idempotent remote environment setup for the L2 comparison.
# Run AFTER remote_sync.sh. Requires L2_SSH_HOST / L2_SSH_PASS
# (L2_SSH_USER defaults to root, L2_REMOTE_WORK to /root/work).
#
# The account needs write access to L2_REMOTE_WORK. Package installation (uv,
# ripgrep) only runs when the tool is missing and falls back to userspace
# installs, so an ordinary account works when those tools are already present.
#
# The heavy work (toolchain install + cargo/maturin builds) can easily run for
# tens of minutes, so it is NOT executed inside one long-lived ssh session.
# Instead we upload a self-logging inner script, launch it under nohup on the
# remote, and poll a small status file with short ssh calls. This keeps every
# individual ssh command well under the 30-minute ceiling and leaves a full
# per-step log trail under $L2_REMOTE_WORK/logs/ for post-mortem debugging.
#
# Headroom install failures write $L2_REMOTE_WORK/.headroom_unavailable instead
# of aborting: the harness degrades to a one-sided run and reports it, which is
# more useful than no run at all.

set -euo pipefail

: "${L2_SSH_HOST:?L2_SSH_HOST is required (remote host or IP)}"
: "${L2_SSH_PASS:?L2_SSH_PASS is required (ssh password)}"
L2_SSH_USER="${L2_SSH_USER:-root}"
# Remote workspace root, kept in sync with remote_sync.sh / remote_run.sh.
L2_REMOTE_WORK="${L2_REMOTE_WORK:-/root/work}"

SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null)

remote_ssh() {
    SSHPASS="$L2_SSH_PASS" sshpass -e ssh "${SSH_OPTS[@]}" "$L2_SSH_USER@$L2_SSH_HOST" "$@"
}

# --- 1. Upload the idempotent inner build script ----------------------------
# The heredoc is quoted so nothing is expanded locally; every $VAR is resolved
# on the remote when the script actually runs.
echo "[setup] uploading remote build script"
SSHPASS="$L2_SSH_PASS" sshpass -e ssh "${SSH_OPTS[@]}" "$L2_SSH_USER@$L2_SSH_HOST" \
    "mkdir -p $L2_REMOTE_WORK/logs && cat > $L2_REMOTE_WORK/remote_build_inner.sh" <<'INNER'
#!/usr/bin/env bash
# Heavy, idempotent remote build. Runs under nohup; each step streams to its
# own log under $WORK/logs/. Final state is recorded in setup.status as
# either "DONE" or "FAIL:<step>".
set -uo pipefail

source "$HOME/.cargo/env"
export PATH="$HOME/.local/bin:$PATH"

# Injected by the caller through the nohup command line; the default keeps the
# script runnable by hand on the usual root box.
WORK="${L2_REMOTE_WORK:-/root/work}"
LOG="$WORK/logs"
STATUS="$LOG/setup.status"
: > "$STATUS"

fail() {
    echo "FAIL:$1" > "$STATUS"
    echo "[inner] FAILED at step: $1 (see $LOG)" >&2
    exit 1
}

# --- uv + Python 3.12 (idempotent) -----------------------------------------
if ! command -v uv >/dev/null 2>&1; then
    curl -LsSf https://astral.sh/uv/install.sh | sh > "$LOG/uv_install.log" 2>&1 \
        || fail uv_install
fi
export PATH="$HOME/.local/bin:$PATH"
uv python install 3.12 > "$LOG/uv_python.log" 2>&1 || fail uv_python

# --- ripgrep (+ pkg-config for native crate deps) --------------------------
if ! command -v rg >/dev/null 2>&1; then
    ( apt-get update && apt-get install -y ripgrep pkg-config ) > "$LOG/apt.log" 2>&1 \
        || cargo install ripgrep > "$LOG/rg_cargo.log" 2>&1 \
        || fail ripgrep
fi

# --- tokenless + rtk builds -------------------------------------------------
cd "$WORK/anolisa/src/tokenless" || fail cd_tokenless
just setup-rtk > "$LOG/setup_rtk.log" 2>&1 || fail setup_rtk
cargo build --release > "$LOG/build_tokenless.log" 2>&1 || fail build_tokenless

# The vendored rtk crate ships no [workspace] table of its own. If any ancestor
# directory (e.g. a stray /root/Cargo.toml) contains a workspace manifest, cargo
# walks up, tries to attach rtk to it, and aborts. Make rtk self-contained so it
# builds regardless of what lives above the checkout (idempotent, cargo's own
# recommended remedy).
if ! grep -q '^\[workspace\]' third_party/rtk/Cargo.toml; then
    printf '\n[workspace]\n' >> third_party/rtk/Cargo.toml
fi
cargo build --release --manifest-path third_party/rtk/Cargo.toml \
    > "$LOG/build_rtk.log" 2>&1 || fail build_rtk

# --- l2_compare (separate L2 benchmark workspace) --------------------------
cd benchmark/l2-module || fail cd_l2_module
cargo build --release --bin l2_compare > "$LOG/build_l2_compare.log" 2>&1 \
    || fail build_l2_compare

# --- headroom venv (failure degrades, never aborts) -------------------------
# NOTE: `uv venv` does NOT seed pip into the environment, so the venv has no
# bin/pip. Use `uv pip install --python <venv>` instead — uv drives the install
# itself and resolves the maturin (PyO3) build backend via PEP 517 isolation.
#
# Idempotent: reuse a venv that already imports ContentRouter. This step is not
# only first-time bootstrap — remote_run.sh re-invokes setup after each sync so
# the cargo artifacts match the synced SHA — and `uv venv --clear` + reinstall is
# both expensive (PyO3 build) and destructive (a transient failure would delete
# a working venv and degrade the run to one-sided). So only (re)create when the
# import is actually broken; delete the venv by hand to force a rebuild.
rm -f "$WORK/.headroom_unavailable"
HEADROOM_PY="$WORK/headroom-venv/bin/python"
import_check() {
    "$HEADROOM_PY" -c \
        "from headroom.transforms.content_router import ContentRouter" 2>/dev/null
}
if import_check; then
    echo "[inner] headroom venv already usable - reusing" > "$LOG/headroom.log"
else
    {
        uv venv --clear "$WORK/headroom-venv" -p 3.12 \
        && uv pip install --python "$HEADROOM_PY" maturin \
        && uv pip install --python "$HEADROOM_PY" -e "$WORK/headroom" \
        && "$HEADROOM_PY" -c \
            "from headroom.transforms.content_router import ContentRouter; print('IMPORT_OK')"
    } > "$LOG/headroom.log" 2>&1
fi
if import_check; then
    echo "[inner] headroom venv ready"
else
    echo "[inner] headroom unavailable - marking and continuing" >&2
    touch "$WORK/.headroom_unavailable"
fi

echo "DONE" > "$STATUS"
echo "[inner] all build steps complete"
INNER

# --- 2. Launch the build in the background ----------------------------------
# L2_REMOTE_WORK is exported into the nohup environment so the inner script
# (uploaded through a quoted heredoc, hence unexpanded) resolves the same root.
echo "[setup] launching background build on $L2_SSH_HOST"
remote_ssh "chmod +x $L2_REMOTE_WORK/remote_build_inner.sh; \
    : > $L2_REMOTE_WORK/logs/setup.status; \
    L2_REMOTE_WORK=$L2_REMOTE_WORK nohup bash $L2_REMOTE_WORK/remote_build_inner.sh \
        > $L2_REMOTE_WORK/logs/nohup.log 2>&1 & \
    echo \"[setup] launched pid \$!\""

# --- 3. Poll the status file with short ssh calls ---------------------------
# Max wait ~40 min; the maturin/headroom compile alone can take 5-15 min.
echo "[setup] polling for completion (per-step logs in $L2_REMOTE_WORK/logs/)"
deadline=$(( $(date +%s) + 2400 ))
while :; do
    status="$(remote_ssh "cat $L2_REMOTE_WORK/logs/setup.status 2>/dev/null" || true)"
    case "$status" in
        DONE)
            echo "[setup] build complete"
            break
            ;;
        FAIL:*)
            echo "[setup] build FAILED at step: ${status#FAIL:}" >&2
            echo "[setup] inspect $L2_REMOTE_WORK/logs/ on the remote for details" >&2
            exit 1
            ;;
    esac
    if [ "$(date +%s)" -ge "$deadline" ]; then
        echo "[setup] timed out after 40m waiting for build" >&2
        exit 1
    fi
    sleep 20
done

echo "[setup] done"

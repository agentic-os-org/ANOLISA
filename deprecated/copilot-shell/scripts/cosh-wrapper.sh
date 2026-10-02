#!/bin/bash
# copilot-shell wrapper — locates Node.js and launches cli.js
# @@LIBDIR@@ is replaced at install time by `make install`.

LIBDIR="@@LIBDIR@@"

# ── Resolve Node.js when not already in PATH (e.g. nvm-managed installs) ──
if ! command -v node >/dev/null 2>&1; then
  nvm_dir="${NVM_DIR:-$HOME/.nvm}"
  if [ -d "$nvm_dir/versions/node" ]; then
    latest_dir=$(ls -d "$nvm_dir/versions/node/"v* 2>/dev/null | sort -V | tail -1)
    if [ -n "$latest_dir" ] && [ -x "$latest_dir/bin/node" ]; then
      export PATH="$latest_dir/bin:$PATH"
    fi
  fi
fi

if ! command -v node >/dev/null 2>&1; then
  echo "Error: Node.js not found. Install Node.js >= 20 or configure NVM_DIR." >&2
  exit 1
fi

# ── Correlation session id for observability ──
# Export a session id into the environment BEFORE exec so it lands in this
# process's /proc/<pid>/environ snapshot (which AgentSight scrapes for
# per-run attribution) and is inherited by child tool processes. Idempotent:
# an inherited value is preserved so nested/parent runs share one id.
# This is an UNAUTHENTICATED observability hint only — never used for authz.
export COSH_SESSION_ID="${COSH_SESSION_ID:-$(uuidgen 2>/dev/null || cat /proc/sys/kernel/random/uuid 2>/dev/null)}"

exec node "$LIBDIR/cli.js" "$@"

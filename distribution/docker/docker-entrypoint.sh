#!/bin/bash
set -e

echo "[anolisa] Starting ANOLISA Agentic OS container..."

# ---------------------------------------------------------------------------
# AgentSight: start if binary is available and eBPF capabilities are present
# ---------------------------------------------------------------------------
if command -v agentsight &>/dev/null; then
  if capsh --print 2>/dev/null | grep -qE 'cap_bpf|cap_sys_admin'; then
    echo "[anolisa] eBPF capabilities detected, starting agentsight..."
    if command -v agentsight-start &>/dev/null; then
      agentsight-start &
    else
      agentsight serve &
    fi
  else
    echo "[anolisa] WARNING: Missing cap_bpf/cap_perfmon capabilities."
    echo "[anolisa] agentsight will NOT start. To enable, run with:"
    echo "[anolisa]   docker run --cap-add CAP_BPF --cap-add CAP_PERFMON ..."
    echo "[anolisa]   or: docker run --privileged ..."
  fi
fi

# ---------------------------------------------------------------------------
# Agent-sec-core: verify namespace support
# ---------------------------------------------------------------------------
if command -v bwrap &>/dev/null; then
  if bwrap --ro-bind / / true 2>/dev/null; then
    echo "[anolisa] Sandbox (bubblewrap) is functional."
  else
    echo "[anolisa] WARNING: bubblewrap sandbox may not work."
    echo "[anolisa] Ensure the container has appropriate namespace permissions."
  fi
fi

# ---------------------------------------------------------------------------
# Execute the main command
# ---------------------------------------------------------------------------
exec "$@"

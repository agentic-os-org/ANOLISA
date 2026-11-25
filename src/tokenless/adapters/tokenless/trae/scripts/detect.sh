#!/usr/bin/env bash
# detect.sh — Inspect Trae presence and the tokenless hook state.
#
# Read-only, and fail-open: it always exits 0 and reports on stdout, which is
# the detector contract #2525 asks this adapter for. Nothing gates on the exit
# status — `make trae-install` runs it with `|| true`, purely to print the report
# before installing, and no ANOLISA driver claims the trae framework yet (see
# packaging/anolisa/component.toml.in) — so a non-zero status would only ever be
# noise. The verdict therefore lives in the report, not in the exit code:
#   "<agent>: ready"                               installed and usable
#   "<agent>: not installed (ready to install)"     prereqs OK, hooks not merged
#   "<agent>: missing prerequisites (...)"          something has to be fixed
#
# This differs from the tri-state detect.sh of claude-code/qwencode/openclaw/
# hermes. `make openclaw-install` really does gate on its detector's exit
# status, so that convention cannot simply be dropped there; unifying the two
# across the component is a separate change.
#
# Hook registration is decided by lifecycle.py — the same ownership check
# install.sh and uninstall.sh use — so a user hook that merely mentions the
# marker does not make the adapter look registered.
set -euo pipefail

COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
AGENT="${ANOLISA_TARGET:-trae}"
SCRIPT_DIR="$(CDPATH='' cd -- "${BASH_SOURCE[0]%/*}" && pwd -P)"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

ADAPTER_SRC="$ADAPTER_DIR/trae"
LIFECYCLE="$SCRIPT_DIR/lifecycle.py"

TRAE_BIN="${TRAE_BIN:-}"
export PATH="$HOME/.local/bin:/usr/local/bin:$PATH"

line()  { printf '[%s] %s\n' "$COMPONENT" "$*"; }
field() { printf '[%s]   %-26s %s\n' "$COMPONENT" "$1" "$2"; }

PREREQ_MISSING=()
INSTALL_MISSING=()
note_prereq_missing()  { PREREQ_MISSING+=("$1"); }
note_install_missing() { INSTALL_MISSING+=("$1"); }

if [ -z "$TRAE_BIN" ]; then
    TRAE_BIN="$(command -v trae 2>/dev/null || true)"
fi

line "${AGENT} detect"
if [ -n "$TRAE_BIN" ] && [ -x "$TRAE_BIN" ]; then
    field "trae CLI"            "present (${TRAE_BIN})"
else
    # Trae is an IDE; many installations expose no CLI. Informational only —
    # presence is decided by the edition home directories below.
    field "trae CLI"            "missing (informational)"
fi

# Trae editions: CN (~/.trae-cn) and international (~/.trae). Both are
# created on first run; absence of both means Trae is not installed.
TRAE_HOMES=()
for home in "$HOME/.trae-cn" "$HOME/.trae"; do
    if [ -d "$home" ]; then
        TRAE_HOMES+=("$home")
        field "trae home"         "present ($home)"
    fi
done
if [ ${#TRAE_HOMES[@]} -eq 0 ]; then
    field "trae home"           "missing (~/.trae-cn / ~/.trae)"
    note_prereq_missing "Trae (no ~/.trae-cn or ~/.trae)"
fi

# Report tokenless hook registration per detected edition. Registration means
# adapter-owned entries, judged by lifecycle.py's ownership check on the parsed
# hook entries — not by a marker substring, which a user's own hook can contain.
owned_entries() {
    if [ ! -f "$LIFECYCLE" ]; then
        printf 'unknown\n'
        return 0
    fi
    if ! command -v python3 >/dev/null 2>&1; then
        printf 'unknown\n'
        return 0
    fi
    python3 -B "$LIFECYCLE" owned-count --hooks-file "$1" 2>/dev/null || printf 'unknown\n'
}

for home in ${TRAE_HOMES[@]+"${TRAE_HOMES[@]}"}; do
    hooks_file="$home/hooks.json"
    if [ ! -f "$hooks_file" ]; then
        field "hooks ($home)"     "not installed"
        note_install_missing "hooks for $home"
        continue
    fi
    owned="$(owned_entries "$hooks_file")"
    if [ "$owned" = "unknown" ]; then
        field "hooks ($home)"     "unknown (lifecycle.py or python3 unavailable)"
        note_install_missing "hooks for $home"
    elif [ "$owned" -gt 0 ]; then
        field "hooks ($home)"     "installed ($owned adapter-owned entries)"
    else
        field "hooks ($home)"     "not installed (no adapter-owned entries)"
        note_install_missing "hooks for $home"
    fi
done

if [ -f "$ADAPTER_SRC/hooks/hooks.json" ]; then
    field "hooks template"      "present"
else
    field "hooks template"      "missing (trae/hooks/hooks.json)"
    note_prereq_missing "hooks template"
fi

if [ -f "$ADAPTER_SRC/hooks/run-hook.sh" ]; then
    field "hook dispatcher"     "present"
else
    field "hook dispatcher"     "missing (trae/hooks/run-hook.sh)"
    note_prereq_missing "hook dispatcher"
fi

if command -v python3 &>/dev/null; then
    field "python3"             "present ($(command -v python3))"
else
    field "python3"             "missing"
    note_prereq_missing "python3"
fi

# jq is required by tool_ready_hook.sh; absence disables that hook only
# (rewrite + compress-response still work). Treat as informational.
if command -v jq &>/dev/null; then
    field "jq"                  "present ($(command -v jq))"
else
    field "jq"                  "missing (tool-ready hook disabled)"
fi

runtime_bin="$(command -v tokenless 2>/dev/null || true)"
if [ -n "$runtime_bin" ]; then
    field "tokenless binary"    "present (${runtime_bin})"
else
    field "tokenless binary"    "missing"
    note_prereq_missing "tokenless binary"
fi

# RTK is embedded in the tokenless CLI and runs as `tokenless rtk`; packaging
# ships no standalone `rtk` binary (see tokenless-cli rtk_args: "the standalone
# `rtk` target ... is not shipped"). Probe the runtime the hooks actually
# invoke, so the supported install layout — adapter tree staged plus
# `tokenless` on PATH — reports ready instead of a missing prerequisite.
if [ -n "$runtime_bin" ]; then
    # No pipe: under `pipefail` a `head -n 1` that closes early would turn a
    # successful probe into SIGPIPE (141) and report a healthy runtime missing.
    if rtk_probe="$("$runtime_bin" rtk --version 2>&1)"; then
        rtk_version="${rtk_probe%%$'\n'*}"
        field "rtk runtime"         "present (bundled: tokenless rtk${rtk_version:+ ${rtk_version}})"
    else
        field "rtk runtime"         "missing (tokenless rtk --version failed)"
        note_prereq_missing "rtk runtime (tokenless rtk)"
    fi
else
    # Already reported as a missing prerequisite above; do not double-count it.
    field "rtk runtime"         "missing (no tokenless binary)"
fi

# A separately installed upstream rtk is not required and not used by the hooks;
# report it for information only.
standalone_rtk="$(command -v rtk 2>/dev/null || true)"
if [ -n "$standalone_rtk" ]; then
    field "standalone rtk"      "present (${standalone_rtk}, unused)"
fi

# Shared hook scripts live under FHS; warn when missing so user knows to run
# `make install` (or the RPM) before the adapter actually fires.
SHARED_HOOKS_DIR=""
for d in /usr/local/share/anolisa/adapters/tokenless/common/hooks \
         /usr/share/anolisa/adapters/tokenless/common/hooks \
         "$HOME/.local/share/anolisa/adapters/tokenless/common/hooks"; do
    if [ -d "$d" ]; then SHARED_HOOKS_DIR="$d"; break; fi
done
if [ -n "$SHARED_HOOKS_DIR" ]; then
    field "shared hooks dir"    "present ($SHARED_HOOKS_DIR)"
else
    field "shared hooks dir"    "missing (run: make -C src/tokenless install)"
    note_prereq_missing "shared hooks dir"
fi

if [ ${#PREREQ_MISSING[@]} -gt 0 ]; then
    line "${AGENT}: missing prerequisites (${PREREQ_MISSING[*]})"
elif [ ${#INSTALL_MISSING[@]} -gt 0 ]; then
    line "${AGENT}: not installed (ready to install)"
else
    line "${AGENT}: ready"
fi
# Fail-open: the report above is the result, so the exit status is always 0.
exit 0

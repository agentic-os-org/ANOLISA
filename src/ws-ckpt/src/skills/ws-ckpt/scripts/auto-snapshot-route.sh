#!/bin/sh
# Classify the available automatic snapshot route without changing state.
openclaw_plugin_route() {
    python3 - "$@" <<'PY'
import json
import os
from pathlib import Path
import subprocess
import sys

env = os.environ.copy()
home = env.pop("OPENCLAW_HOME", None)
state_dir = Path(env.get("OPENCLAW_STATE_DIR") or home or sys.argv[1] or Path.home() / ".openclaw")
env["OPENCLAW_STATE_DIR"] = str(state_dir)
env["PATH"] = os.pathsep.join(
    [str(Path.home() / ".local/bin"), str(state_dir / "bin"), "/usr/local/bin", env.get("PATH", os.defpath)]
)
try:
    result = subprocess.run(
        [env.get("OPENCLAW_BIN") or "openclaw", "plugins", "list", "--json"],
        env=env, capture_output=True, text=True, timeout=20, check=True,
    )
    data = json.loads(result.stdout)
    plugins = data.get("plugins") if isinstance(data, dict) else None
    if not isinstance(plugins, list) or any(
        not isinstance(plugin, dict) or not isinstance(plugin.get("id"), str)
        for plugin in plugins
    ):
        raise ValueError("unexpected OpenClaw plugin list format")
    present = any(plugin["id"] == "ws-ckpt" for plugin in plugins)
    if not present:
        present = (state_dir / "extensions/ws-ckpt").is_dir()
except (OSError, ValueError, subprocess.SubprocessError) as error:
    print(f"Unable to detect the OpenClaw ws-ckpt plugin: {error}", file=sys.stderr)
    sys.exit(1)

print("openclaw_plugin_present" if present else "openclaw_plugin_missing")
PY
}

hermes_plugin_route() {
    hermes_home=${HERMES_HOME:-${1:-${HOME:?HOME is required to locate the Hermes profile}/.hermes}}
    if [ -f "${hermes_home%/}/plugins/ws-ckpt/plugin.yaml" ]; then
        printf '%s\n' hermes_plugin_present
    else
        printf '%s\n' hermes_plugin_missing
    fi
}

case "${1-}:$#" in
    --model:1) printf '%s\n' model; exit ;;
    --aw:1|--openclaw:1|--hermes:1|:0) ;;
    *) printf '%s\n' 'Usage: auto-snapshot-route.sh [--aw|--model|--openclaw|--hermes]' >&2; exit 2 ;;
esac

openclaw_state_hint=
hermes_home_hint=
case "$0" in
    */.openclaw/skills/ws-ckpt/scripts/auto-snapshot-route.sh)
        openclaw_state_hint=${0%/skills/ws-ckpt/scripts/auto-snapshot-route.sh}
        ;;
    */.hermes/skills/ws-ckpt/scripts/auto-snapshot-route.sh)
        hermes_home_hint=${0%/skills/ws-ckpt/scripts/auto-snapshot-route.sh}
        ;;
esac

if [ "${1-}" = --hermes ]; then
    hermes_plugin_route "$hermes_home_hint"
elif [ "${1-}" = --openclaw ]; then
    openclaw_plugin_route "$openclaw_state_hint"
# AW sessions can inherit either runtime's profile variables.
elif [ -n "${WS_CKPT_AW_TARGET-}" ] || [ -n "${WS_CKPT_AW_PROVIDER-}" ]; then
    if [ -n "${WS_CKPT_AW_TARGET-}" ] &&
        [ -f "${WS_CKPT_AW_PROVIDER-}" ] && [ -x "${WS_CKPT_AW_PROVIDER-}" ]; then
        printf '%s\n' aw_bound
    else
        printf '%s\n' aw_unavailable
    fi
elif [ "${1-}" = --aw ]; then
    printf '%s\n' aw_setup
elif [ -n "$openclaw_state_hint" ] || [ -n "${OPENCLAW_STATE_DIR-}" ] ||
    [ -n "${OPENCLAW_HOME-}" ] || [ -n "${OPENCLAW_CONFIG_PATH-}" ]; then
    openclaw_plugin_route "$openclaw_state_hint"
elif [ -n "${HERMES_HOME-}" ] || [ -n "$hermes_home_hint" ]; then
    hermes_plugin_route "$hermes_home_hint"
else
    printf '%s\n' model
fi

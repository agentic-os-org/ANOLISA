#!/usr/bin/env bash
#
# SkillFS sidecar entrypoint.
#
# Builds the fixed foreground-mount command and hands it to the PID 1
# supervisor. The supervisor performs preflight before every attempt, verifies
# real I/O through the existing mount probe, and remounts a failed session.
#
# Configuration (environment):
#   SKILLFS_SOURCE      absolute path to the writable skill source root
#   SKILLFS_MOUNTPOINT  absolute path where the FUSE view is exposed
#   SKILLFS_DISCOVER_ROOT  reader-visible skills root advertised by discover
#   SKILLFS_EXTRA_ARGS  extra `skillfs mount` arguments, whitespace separated
#   SKILLFS_SKIP_PREFLIGHT  set to 1 to skip preflight (debugging only)
#   RUST_LOG            SkillFS log filter, defaults to info
#
# Any argument passed to the container image is treated as a full command and
# executed instead of the mount, which keeps `kubectl debug`-style overrides
# possible without a second image.

set -uo pipefail

log() {
	printf '[skillfs-entrypoint] %s\n' "$*" >&2
}

# Escape hatch for debugging: `command: ["/usr/local/bin/skillfs-sidecar-entrypoint"]`
# plus `args: ["sleep", "3600"]` runs that command instead of the mount.
if (($# > 0)); then
	log "explicit command override, exec: $*"
	exec "$@"
fi

SKILLFS_BIN="${SKILLFS_BIN:-/usr/local/bin/skillfs}"
SOURCE_DIR="${SKILLFS_SOURCE:-}"
MOUNTPOINT="${SKILLFS_MOUNTPOINT:-}"
DISCOVER_ROOT="${SKILLFS_DISCOVER_ROOT:-}"
EXTRA_ARGS="${SKILLFS_EXTRA_ARGS:-}"
export RUST_LOG="${RUST_LOG:-info}"

if [[ ! -x "$SKILLFS_BIN" ]]; then
	log "FAIL: SkillFS binary '$SKILLFS_BIN' is missing or not executable"
	exit 127
fi

log "skillfs version: $("$SKILLFS_BIN" --version 2>&1 | head -1)"
log "uid=$(id -u) gid=$(id -g) source=${SOURCE_DIR:-<unset>} mountpoint=${MOUNTPOINT:-<unset>}"

# Word splitting on SKILLFS_EXTRA_ARGS is intentional: the value is an argument
# list, not a single argument.
declare -a extra=()
if [[ -n "$EXTRA_ARGS" ]]; then
	read -r -a extra <<<"$EXTRA_ARGS"
fi

# `--foreground` keeps SkillFS as a direct child that this container's PID 1
# supervisor can stop, reap, and restart. The detached `--managed` mode and its
# separate supervisor must not be used here.
# `--allow-other` is what lets the Agent container's UID reach the propagated
# view. Both flags are fixed by the sidecar contract and are not configurable
# through SKILLFS_EXTRA_ARGS.
declare -a cmd=(
	"$SKILLFS_BIN" mount "$SOURCE_DIR" "$MOUNTPOINT"
	--foreground
	--allow-other
)
if [[ -n "$DISCOVER_ROOT" ]]; then
	cmd+=(--skill-discover-root "$DISCOVER_ROOT")
fi
if ((${#extra[@]} > 0)); then
	cmd+=("${extra[@]}")
fi

SUPERVISOR_BIN="${SKILLFS_SUPERVISOR_BIN:-/usr/local/bin/skillfs-supervisor}"
if [[ "$SUPERVISOR_BIN" != /* || ! -x "$SUPERVISOR_BIN" ]]; then
	log "FAIL: supervisor '$SUPERVISOR_BIN' must be an executable absolute path"
	exit 127
fi

log "supervise: ${cmd[*]}"
exec "$SUPERVISOR_BIN" "${cmd[@]}"

#!/usr/bin/env bash
#
# SkillFS mount health probe for the sidecar container.
#
# `skillfs --version` is deliberately NOT a health signal: the binary answers
# it while the FUSE session is absent, hung, or disconnected. This probe asserts
# the two properties the Agent container actually depends on:
#
#   1. the mountpoint is present in /proc/self/mountinfo with a fuse filesystem
#      type, and
#   2. a configured probe file is openable and readable through the FUSE view,
#      within a bounded time.
#
# The read is wrapped in `timeout` so a wedged FUSE session fails the probe
# instead of blocking the kubelet's exec until the probe timeout kills it with
# an ambiguous result.
#
# Usage:
#   skillfs-mount-probe [--mode startup|readiness|liveness] [--timeout SECONDS]
#                       [--mountpoint PATH] [--file RELATIVE_PATH]
#
# Configuration (environment, overridden by the flags above):
#   SKILLFS_MOUNTPOINT    mountpoint to check (required)
#   SKILLFS_PROBE_FILE    probe path relative to the mountpoint (required)
#   SKILLFS_PROBE_TIMEOUT per-read timeout in seconds (default 5)
#
# Exit codes:
#   0  the mount is present and the probe file is readable
#   1  invalid probe configuration
#   2  the mountpoint is not a live fuse mount
#   3  the probe file could not be read through the FUSE view

set -uo pipefail

MODE="readiness"
MOUNTPOINT="${SKILLFS_MOUNTPOINT:-}"
PROBE_FILE="${SKILLFS_PROBE_FILE:-}"
PROBE_TIMEOUT="${SKILLFS_PROBE_TIMEOUT:-5}"

readonly EX_CONFIG=1
readonly EX_NO_MOUNT=2
readonly EX_UNREADABLE=3

log() {
	printf '[skillfs-probe/%s] %s\n' "$MODE" "$*" >&2
}

die() {
	local code="$1"
	shift
	printf '[skillfs-probe/%s] FAIL(%s): %s\n' "$MODE" "$code" "$*" >&2
	exit "$code"
}

while (($# > 0)); do
	case "$1" in
	--mode)
		MODE="${2:-}"
		shift 2
		;;
	--timeout)
		PROBE_TIMEOUT="${2:-}"
		shift 2
		;;
	--mountpoint)
		MOUNTPOINT="${2:-}"
		shift 2
		;;
	--file)
		PROBE_FILE="${2:-}"
		shift 2
		;;
	--startup | --readiness | --liveness)
		MODE="${1#--}"
		shift
		;;
	*)
		die "$EX_CONFIG" "unknown argument '$1'"
		;;
	esac
done

case "$MODE" in
startup | readiness | liveness) ;;
*) die "$EX_CONFIG" "unknown mode '$MODE' (expected startup, readiness, or liveness)" ;;
esac

[[ -n "$MOUNTPOINT" ]] ||
	die "$EX_CONFIG" "no mountpoint: set SKILLFS_MOUNTPOINT or pass --mountpoint"
[[ -n "$PROBE_FILE" ]] ||
	die "$EX_CONFIG" "no probe file: set SKILLFS_PROBE_FILE or pass --file (a path relative to the mountpoint)"
[[ "$PROBE_TIMEOUT" =~ ^[0-9]+$ && "$PROBE_TIMEOUT" -gt 0 ]] ||
	die "$EX_CONFIG" "invalid timeout '$PROBE_TIMEOUT' (expected a positive integer number of seconds)"

# --- 1. the mountpoint must be a live fuse mount -----------------------------
#
# mountinfo field 5 is the mount point and field 9 the filesystem type. Matching
# the type as well as the path prevents a bare shared-volume directory from
# passing as a successful SkillFS mount.
mount_fstype="$(awk -v target="$MOUNTPOINT" '
	$5 == target {
		for (i = 7; i <= NF; i++) {
			if ($i == "-") { print $(i + 1); exit }
		}
	}
' /proc/self/mountinfo 2>/dev/null)"

if [[ -z "$mount_fstype" ]]; then
	die "$EX_NO_MOUNT" "'$MOUNTPOINT' is not present in /proc/self/mountinfo; the SkillFS FUSE session has not been established (or has already been torn down)"
fi
if [[ "$mount_fstype" != fuse* ]]; then
	die "$EX_NO_MOUNT" "'$MOUNTPOINT' is mounted with filesystem type '$mount_fstype', not a fuse filesystem; something other than SkillFS owns this path"
fi

# --- 2. the probe file must be readable through the FUSE view ----------------
probe_path="$MOUNTPOINT/${PROBE_FILE#/}"

read_output="$(timeout "$PROBE_TIMEOUT" cat -- "$probe_path" 2>&1 >/dev/null)"
read_status=$?

if ((read_status == 124)); then
	die "$EX_UNREADABLE" "reading '$probe_path' did not complete within ${PROBE_TIMEOUT}s; the FUSE session ('$mount_fstype' on '$MOUNTPOINT') is not answering requests"
fi
if ((read_status != 0)); then
	die "$EX_UNREADABLE" "reading '$probe_path' failed (exit $read_status): ${read_output:-no error output}"
fi

# A zero-byte read means the mount answered but the probe file is empty, which is
# not a usable view for the Agent container either.
byte_count_output="$(timeout "$PROBE_TIMEOUT" wc -c -- "$probe_path" 2>&1)"
byte_count_status=$?

if ((byte_count_status == 124)); then
	die "$EX_UNREADABLE" "counting bytes in '$probe_path' did not complete within ${PROBE_TIMEOUT}s; the FUSE session ('$mount_fstype' on '$MOUNTPOINT') is not answering requests"
fi
if ((byte_count_status != 0)); then
	die "$EX_UNREADABLE" "counting bytes in '$probe_path' failed (exit $byte_count_status): ${byte_count_output:-no error output}"
fi

read -r byte_count _ <<<"$byte_count_output"
if [[ ! "$byte_count" =~ ^[0-9]+$ ]]; then
	die "$EX_UNREADABLE" "counting bytes in '$probe_path' returned an invalid result: ${byte_count_output:-no output}"
fi
if ((byte_count == 0)); then
	die "$EX_UNREADABLE" "'$probe_path' is readable through the FUSE view but returned 0 bytes; the probe file is empty"
fi

log "ok: $mount_fstype on $MOUNTPOINT, read $byte_count bytes from $probe_path"

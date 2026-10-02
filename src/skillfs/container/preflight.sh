#!/usr/bin/env bash
#
# SkillFS sidecar preflight.
#
# Validates every prerequisite the foreground FUSE worker needs before the
# sidecar supervisor starts it, so a broken attempt fails with a specific
# diagnostic instead of a generic mount error.
#
# Configuration (environment):
#   SKILLFS_SOURCE            absolute path to the writable skill source root
#   SKILLFS_MOUNTPOINT        absolute path where the FUSE view is exposed
#   SKILLFS_FUSE_DEVICE       FUSE character device (default /dev/fuse)
#   SKILLFS_FUSE_CONF         libfuse config file (default /etc/fuse.conf)
#   SKILLFS_RESIDUAL_UNMOUNT  1 (default) to clear a stale mount, 0 to only report
#
# Usage:
#   skillfs-preflight                 validate all prerequisites and clean residue
#   skillfs-preflight --cleanup-only  clean only the configured residual mount
#
# Exit codes are stable so operators can identify the failed prerequisite:
#   0   all prerequisites satisfied
#   10  invalid or missing configuration
#   11  FUSE device missing, wrong type, or not openable
#   12  fusermount3 helper missing or not executable
#   13  source root missing, not a directory, or not readable/writable
#   14  mountpoint missing, not a directory, or not usable
#   15  fuse.conf does not permit allow_other
#   16  mountpoint carries a residual mount that could not be cleared

set -uo pipefail

readonly EX_CONFIG=10
readonly EX_FUSE_DEVICE=11
readonly EX_FUSERMOUNT=12
readonly EX_SOURCE=13
readonly EX_MOUNTPOINT=14
readonly EX_FUSE_CONF=15
readonly EX_RESIDUAL_MOUNT=16

SOURCE_DIR="${SKILLFS_SOURCE:-}"
MOUNTPOINT="${SKILLFS_MOUNTPOINT:-}"
FUSE_DEVICE="${SKILLFS_FUSE_DEVICE:-/dev/fuse}"
FUSE_CONF="${SKILLFS_FUSE_CONF:-/etc/fuse.conf}"
RESIDUAL_UNMOUNT="${SKILLFS_RESIDUAL_UNMOUNT:-1}"
MODE="full"

log() {
	printf '[skillfs-preflight] %s\n' "$*" >&2
}

# Emit an actionable failure and exit with the caller-supplied stable code.
# Every call must name the concrete device, file, path, permission, or setting
# that is missing — "preflight failed" alone is not a usable diagnostic.
die() {
	local code="$1"
	shift
	printf '[skillfs-preflight] FAIL(%s): %s\n' "$code" "$*" >&2
	exit "$code"
}

# ---------------------------------------------------------------------------
# Mount table helpers
# ---------------------------------------------------------------------------

# Print the mount table lines whose mount point is exactly $1.
#
# /proc/self/mountinfo field 5 is the mount point and field 9 the filesystem
# type. It is preferred over /proc/mounts because it also lists the propagation
# flags this deployment depends on.
mountinfo_lines_for() {
	local target="$1"
	awk -v target="$target" '
		{
			# Fields: id parent major:minor root mountpoint opts... - fstype src super
			if ($5 == target) { print $0 }
		}
	' /proc/self/mountinfo 2>/dev/null
}

is_mounted() {
	[[ -n "$(mountinfo_lines_for "$1")" ]]
}

# Print the filesystem type of the mount at $1, or nothing if it is not mounted.
#
# In mountinfo the optional fields sit between the mount options and a literal
# "-" separator, so the filesystem type is the field after that separator rather
# than a fixed column.
mount_fstype_for() {
	local target="$1"
	awk -v target="$target" '
		$5 == target {
			for (i = 7; i <= NF; i++) {
				if ($i == "-") { print $(i + 1); exit }
			}
		}
	' /proc/self/mountinfo 2>/dev/null
}

# Report whether the mountpoint is a dead FUSE endpoint. `stat` on a mountpoint
# whose FUSE daemon is gone fails with ENOTCONN, which is exactly the state a
# restarted sidecar inherits through a Bidirectional shared volume.
is_disconnected_endpoint() {
	local target="$1"
	local err
	err="$(stat -c '%i' "$target" 2>&1 >/dev/null)"
	[[ "$err" == *"Transport endpoint is not connected"* || "$err" == *ENOTCONN* ]]
}

# ---------------------------------------------------------------------------
# Checks
# ---------------------------------------------------------------------------

check_config() {
	if [[ -z "$SOURCE_DIR" ]]; then
		die "$EX_CONFIG" "SKILLFS_SOURCE is unset or empty; set it to the absolute path of the skill source root"
	fi
	if [[ -z "$MOUNTPOINT" ]]; then
		die "$EX_CONFIG" "SKILLFS_MOUNTPOINT is unset or empty; set it to the absolute path where the FUSE view is exposed"
	fi
	if [[ "$SOURCE_DIR" != /* ]]; then
		die "$EX_CONFIG" "SKILLFS_SOURCE must be an absolute path, got '$SOURCE_DIR'"
	fi
	if [[ "$MOUNTPOINT" != /* ]]; then
		die "$EX_CONFIG" "SKILLFS_MOUNTPOINT must be an absolute path, got '$MOUNTPOINT'"
	fi
	# An in-place mount is a supported SkillFS mode but not the sidecar
	# topology: the Agent container must see the FUSE view without the
	# physical source being exposed on the same path.
	if [[ "$(realpath -m -- "$SOURCE_DIR")" == "$(realpath -m -- "$MOUNTPOINT")" ]]; then
		die "$EX_CONFIG" "SKILLFS_SOURCE and SKILLFS_MOUNTPOINT resolve to the same path ('$SOURCE_DIR'); the sidecar topology requires distinct volumes"
	fi
	log "config source=$SOURCE_DIR mountpoint=$MOUNTPOINT"
}

check_fuse_device() {
	if [[ ! -e "$FUSE_DEVICE" ]]; then
		die "$EX_FUSE_DEVICE" "FUSE device '$FUSE_DEVICE' does not exist; the Pod must map the device (securityContext.privileged plus a hostPath/CharDevice volume for '$FUSE_DEVICE')"
	fi
	if [[ ! -c "$FUSE_DEVICE" ]]; then
		die "$EX_FUSE_DEVICE" "FUSE device '$FUSE_DEVICE' exists but is not a character device; check the volume type (expected CharDevice)"
	fi
	# Existence is not enough: a device node can be present while the
	# container lacks the device cgroup permission to open it. The open runs in
	# a subshell so a redirection failure cannot terminate this script and the
	# descriptor is released immediately.
	if ! (: <>"$FUSE_DEVICE") 2>/dev/null; then
		die "$EX_FUSE_DEVICE" "FUSE device '$FUSE_DEVICE' cannot be opened read-write by uid $(id -u); the container needs privileged (or an explicit device allowance) on this cluster"
	fi
	log "fuse device $FUSE_DEVICE is present and openable"
}

check_fusermount() {
	local helper
	helper="$(command -v fusermount3 2>/dev/null)"
	if [[ -z "$helper" ]]; then
		die "$EX_FUSERMOUNT" "'fusermount3' not found in PATH ($PATH); install the fuse3 package in the sidecar image"
	fi
	if [[ ! -x "$helper" ]]; then
		die "$EX_FUSERMOUNT" "'$helper' is not executable; fix the fuse3 package installation or the image file mode"
	fi
	log "fusermount3 helper at $helper"
}

check_source() {
	if [[ ! -e "$SOURCE_DIR" ]]; then
		die "$EX_SOURCE" "source root '$SOURCE_DIR' does not exist; seed it with an init container or a pre-populated volume"
	fi
	if [[ ! -d "$SOURCE_DIR" ]]; then
		die "$EX_SOURCE" "source root '$SOURCE_DIR' exists but is not a directory"
	fi
	if [[ ! -r "$SOURCE_DIR" || ! -x "$SOURCE_DIR" ]]; then
		die "$EX_SOURCE" "source root '$SOURCE_DIR' is not readable/traversable by uid $(id -u) gid $(id -g) (mode $(stat -c '%a owner %u:%g' "$SOURCE_DIR" 2>/dev/null))"
	fi
	# Write-passthrough is part of the delivered contract, so a read-only
	# source volume has to fail here rather than at the first Agent write.
	if [[ ! -w "$SOURCE_DIR" ]]; then
		die "$EX_SOURCE" "source root '$SOURCE_DIR' is not writable by uid $(id -u) gid $(id -g) (mode $(stat -c '%a owner %u:%g' "$SOURCE_DIR" 2>/dev/null)); SkillFS write passthrough requires a writable source volume"
	fi
	# `mktemp` is used rather than a predictable name with `: > file`, because a
	# shell redirection follows an existing symlink and would truncate whatever
	# it points at. `mktemp` creates with O_CREAT|O_EXCL on a random name, so it
	# fails instead of writing through a planted link.
	local probe
	if ! probe="$(mktemp "$SOURCE_DIR/.skillfs-preflight-write-check.XXXXXXXX" 2>/dev/null)"; then
		die "$EX_SOURCE" "source root '$SOURCE_DIR' rejected a test file creation; the volume may be mounted read-only (check volumeMounts[].readOnly) or be out of space"
	fi
	rm -f -- "$probe"
	log "source root $SOURCE_DIR is readable and writable"
}

check_mountpoint() {
	# Residual-mount handling runs first: on a disconnected FUSE endpoint every
	# `test` on the path fails with ENOTCONN, so `-e` and `mkdir` would both
	# misreport the real problem.
	check_residual_mount
	if [[ ! -e "$MOUNTPOINT" ]]; then
		# Create it rather than fail: the shared emptyDir starts empty and
		# the mountpoint subdirectory is ours to own.
		if ! mkdir -p -- "$MOUNTPOINT" 2>/dev/null; then
			die "$EX_MOUNTPOINT" "mountpoint '$MOUNTPOINT' does not exist and could not be created by uid $(id -u); pre-create it in the shared volume or grant write permission on its parent"
		fi
		log "created mountpoint $MOUNTPOINT"
	fi
	if [[ ! -d "$MOUNTPOINT" ]]; then
		die "$EX_MOUNTPOINT" "mountpoint '$MOUNTPOINT' exists but is not a directory"
	fi
	if [[ ! -r "$MOUNTPOINT" || ! -x "$MOUNTPOINT" ]]; then
		die "$EX_MOUNTPOINT" "mountpoint '$MOUNTPOINT' is not readable/traversable by uid $(id -u) gid $(id -g) (mode $(stat -c '%a owner %u:%g' "$MOUNTPOINT" 2>/dev/null))"
	fi
	log "mountpoint $MOUNTPOINT is usable"
}

# Clear a mount left behind by a previous SkillFS process on the same shared
# volume. Bidirectional propagation means a killed sidecar can leave either a
# live mount or a disconnected endpoint visible to the replacement container.
#
# This runs in a privileged container that can unmount anything it can see, so it
# is deliberately narrow: only a FUSE filesystem at exactly the configured
# mountpoint is ever unmounted. A misconfigured SKILLFS_MOUNTPOINT pointing at a
# shared-volume root, a projected volume, or any other legitimate mount must fail
# the container, not get silently torn down.
check_residual_mount() {
	local disconnected=0
	if is_disconnected_endpoint "$MOUNTPOINT"; then
		disconnected=1
	fi
	if ! is_mounted "$MOUNTPOINT" && ((disconnected == 0)); then
		return 0
	fi

	local detail fstype
	detail="$(mountinfo_lines_for "$MOUNTPOINT" | head -1)"
	fstype="$(mount_fstype_for "$MOUNTPOINT")"
	log "residual mount detected on '$MOUNTPOINT' (fstype='${fstype:-unknown}' disconnected=$disconnected): ${detail:-<not in mountinfo>}"

	# Refuse anything that is not FUSE. A disconnected endpoint that is no longer
	# in mountinfo is accepted, because only a dead FUSE mount produces ENOTCONN.
	if [[ -n "$fstype" && "$fstype" != fuse* ]]; then
		die "$EX_RESIDUAL_MOUNT" "refusing to unmount '$MOUNTPOINT': it carries a '$fstype' filesystem, not a FUSE mount. SkillFS only ever clears its own residual FUSE mounts. Check SKILLFS_MOUNTPOINT — it must be a dedicated subdirectory of the shared volume, not the volume root or another volume's mount path. Offending entry: $detail"
	fi
	if [[ -z "$fstype" ]] && ((disconnected == 0)); then
		die "$EX_RESIDUAL_MOUNT" "mountpoint '$MOUNTPOINT' appears mounted but its filesystem type could not be determined from /proc/self/mountinfo; refusing to unmount an unidentified filesystem. Offending entry: $detail"
	fi

	if [[ "$RESIDUAL_UNMOUNT" != "1" ]]; then
		die "$EX_RESIDUAL_MOUNT" "mountpoint '$MOUNTPOINT' already carries a residual FUSE mount and SKILLFS_RESIDUAL_UNMOUNT=0 disables cleanup: ${detail:-disconnected FUSE endpoint}"
	fi

	local attempt
	for attempt in 1 2 3 4 5 6 7 8 9 10; do
		# `umount -l` is the last resort and is only reached for a path already
		# confirmed to be FUSE (or a dead FUSE endpoint) above.
		fusermount3 -u -- "$MOUNTPOINT" >/dev/null 2>&1 ||
			fusermount3 -u -z -- "$MOUNTPOINT" >/dev/null 2>&1 ||
			umount -l -- "$MOUNTPOINT" >/dev/null 2>&1 ||
			true
		if ! is_mounted "$MOUNTPOINT" && ! is_disconnected_endpoint "$MOUNTPOINT"; then
			log "cleared residual mount on '$MOUNTPOINT' after $attempt attempt(s)"
			return 0
		fi
		# A different filesystem appearing mid-loop means something else is
		# managing this path; stop rather than keep unmounting.
		fstype="$(mount_fstype_for "$MOUNTPOINT")"
		if [[ -n "$fstype" && "$fstype" != fuse* ]]; then
			die "$EX_RESIDUAL_MOUNT" "'$MOUNTPOINT' now carries a '$fstype' filesystem after $attempt cleanup attempt(s); stopping instead of unmounting a non-FUSE filesystem"
		fi
		sleep 0.3
	done

	detail="$(mountinfo_lines_for "$MOUNTPOINT" | head -1)"
	die "$EX_RESIDUAL_MOUNT" "could not clear the residual FUSE mount on '$MOUNTPOINT' after 10 attempts (fusermount3 -u, -u -z, umount -l all failed): ${detail:-disconnected FUSE endpoint}"
}

check_fuse_conf() {
	if [[ ! -e "$FUSE_CONF" ]]; then
		die "$EX_FUSE_CONF" "libfuse config '$FUSE_CONF' does not exist; '--allow-other' requires a 'user_allow_other' line (the sidecar image ships one, so an empty path means a volume or ConfigMap overrode it)"
	fi
	if [[ ! -r "$FUSE_CONF" ]]; then
		die "$EX_FUSE_CONF" "libfuse config '$FUSE_CONF' is not readable by uid $(id -u) (mode $(stat -c '%a owner %u:%g' "$FUSE_CONF" 2>/dev/null))"
	fi
	# libfuse requires the option on a line of its own, with no value.
	if ! grep -qE '^[[:space:]]*user_allow_other[[:space:]]*$' "$FUSE_CONF"; then
		die "$EX_FUSE_CONF" "libfuse config '$FUSE_CONF' has no 'user_allow_other' line on its own; '--allow-other' would be refused for a non-root mounter, so cross-container access by a different Agent UID cannot be guaranteed"
	fi
	log "$FUSE_CONF permits allow_other"
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

main() {
	log "starting preflight as uid=$(id -u) gid=$(id -g)"
	check_config
	if [[ "$MODE" == "cleanup-only" ]]; then
		check_residual_mount
		log "residual-mount cleanup complete"
		return 0
	fi
	check_fuse_device
	check_fusermount
	check_fuse_conf
	check_source
	check_mountpoint
	log "all prerequisites satisfied"
}

case "${1:-}" in
"") ;;
--cleanup-only) MODE="cleanup-only" ;;
*) die "$EX_CONFIG" "unknown argument '$1' (expected --cleanup-only or no arguments)" ;;
esac
if (($# > 1)); then
	die "$EX_CONFIG" "unexpected extra arguments: ${*:2}"
fi

main "$@"

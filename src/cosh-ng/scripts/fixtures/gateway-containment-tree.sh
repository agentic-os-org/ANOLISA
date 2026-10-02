#!/usr/bin/env bash
# Process-tree fixture for the isolated Gateway systemd containment test.
set -euo pipefail

usage() {
    printf 'usage: %s {assert-core-filesystem|assert-clean|launch|leaf|spawn-grandchild|double-fork} ...\n' "$0" >&2
    exit 64
}

assert_core_filesystem() {
    local workspace="$1" state="$2" shm_marker="$3"

    python3 - "$workspace" "$state" "$shm_marker" <<'PY'
import ctypes
import errno
import os
import sys

workspace, state, shm_marker = map(os.fsencode, sys.argv[1:])
if os.path.basename(shm_marker) != shm_marker:
    raise RuntimeError(f"invalid /dev/shm marker basename: {shm_marker!r}")
expected_home = state + b"/core-home"
actual_home = os.fsencode(os.environ.get("HOME", ""))
if actual_home != expected_home:
    raise RuntimeError(f"private Core HOME mismatch: {actual_home!r}")

dev_flags = os.statvfs(b"/dev").f_flag
if not dev_flags & os.ST_RDONLY:
    raise RuntimeError("PrivateDevices did not mount /dev read-only")

shm_flags = os.statvfs(b"/dev/shm").f_flag
for flag, label in (
    (os.ST_RDONLY, "read-only"),
    (os.ST_NOSUID, "nosuid"),
    (os.ST_NODEV, "nodev"),
    (os.ST_NOEXEC, "noexec"),
):
    if not shm_flags & flag:
        raise RuntimeError(f"private /dev/shm is not {label}")

shm_probe = b"/dev/shm/" + shm_marker
try:
    fd = os.open(shm_probe, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o6755)
except OSError as error:
    if error.errno not in (errno.EROFS, errno.EACCES, errno.EPERM):
        raise
else:
    os.close(fd)
    os.unlink(shm_probe)
    raise RuntimeError("the private /dev/shm remained writable")

shm_result_path = state + b"/dev-shm-containment-result"
fd = os.open(shm_result_path, os.O_CREAT | os.O_TRUNC | os.O_WRONLY, 0o600)
os.write(fd, b"creation-denied\n")
os.close(fd)


class OpenHow(ctypes.Structure):
    _fields_ = [
        ("flags", ctypes.c_uint64),
        ("mode", ctypes.c_uint64),
        ("resolve", ctypes.c_uint64),
    ]


libc = ctypes.CDLL(None, use_errno=True)
how = OpenHow(os.O_PATH | os.O_DIRECTORY | os.O_CLOEXEC, 0, 0)
fd = libc.syscall(437, -100, workspace, ctypes.byref(how), ctypes.sizeof(how))
if fd < 0:
    error = ctypes.get_errno()
    raise OSError(error, f"openat2 workspace probe failed: {os.strerror(error)}")
os.close(fd)

forbidden = workspace + b"/must-not-create-suid"
try:
    fd = os.open(forbidden, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o4755)
except OSError as error:
    if error.errno not in (errno.EROFS, errno.EACCES, errno.EPERM):
        raise
else:
    os.close(fd)
    os.unlink(forbidden)
    raise RuntimeError("the admitted Core workspace remained writable")

marker = state + b"/private-state-probe"
fd = os.open(marker, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
os.close(fd)
os.unlink(marker)

log_dir = expected_home + b"/.copilot-shell/logs"
os.makedirs(log_dir, mode=0o700, exist_ok=True)
log_file = log_dir + b"/cosh-core.log.probe"
fd = os.open(log_file, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
os.write(fd, b"private Core logging remains writable\n")
os.close(fd)
os.unlink(log_file)
PY
}

process_start_time() {
    local pid="$1" suffix

    [ -r "/proc/$pid/stat" ] || return 1
    IFS= read -r suffix < "/proc/$pid/stat"
    suffix="${suffix##*) }"
    # starttime is field 22, or field 20 after pid and comm are removed.
    set -- $suffix
    [ "$#" -ge 20 ] || return 1
    printf '%s\n' "${20}"
}

same_process_is_alive() {
    local pid="$1" expected="$2" actual

    actual="$(process_start_time "$pid" 2>/dev/null)" || return 1
    [ "$actual" = "$expected" ]
}

current_cgroup() {
    local hierarchy controllers path

    while IFS=: read -r hierarchy controllers path; do
        if [ "$hierarchy" = "0" ] && [ -z "$controllers" ]; then
            case "$path" in
                /*) printf '%s\n' "$path"; return 0 ;;
            esac
        fi
    done < "/proc/$BASHPID/cgroup"
    return 1
}

assert_clean() {
    local state="$1" generation next identity role pid start cgroup member

    generation=0
    if [ -f "$state/generation" ]; then
        IFS= read -r generation < "$state/generation"
    fi
    case "$generation" in
        '' | *[!0-9]*) exit 65 ;;
    esac
    next=$((generation + 1))

    if [ -d "$state/old" ]; then
        for identity in "$state"/old/*.identity; do
            [ -e "$identity" ] || continue
            IFS=: read -r role pid start < "$identity"
            if same_process_is_alive "$pid" "$start"; then
                printf 'stale %s process remains alive: pid=%s\n' "$role" "$pid" >&2
                exit 70
            fi
        done
    fi

    cgroup="$(current_cgroup)" || exit 71
    [ "$cgroup" != "/" ] || exit 71
    while IFS= read -r member; do
        [ -n "$member" ] || continue
        if [ "$member" != "$BASHPID" ]; then
            printf 'old cgroup is not empty before restart: pid=%s\n' "$member" >&2
            exit 72
        fi
    done < "/sys/fs/cgroup${cgroup}/cgroup.procs"

    : > "$state/preclean.$next"
}

leaf() {
    local pid_file="$1" signal_mode="$2"

    printf '%s\n' "$BASHPID" > "$pid_file"
    if [ "$signal_mode" = "ignore-term" ]; then
        trap '' TERM
    fi
    while :; do
        sleep 60 &
        wait "$!" || true
    done
}

spawn_grandchild() {
    local pid_file="$1"

    "$0" leaf "$pid_file" ignore-term &
    wait "$!"
}

double_fork() {
    local pid_file="$1"

    python3 - "$0" "$pid_file" <<'PY'
import os
import sys

fixture, pid_file = sys.argv[1:]
pid = os.fork()
if pid > 0:
    os.waitpid(pid, 0)
    raise SystemExit(0)
os.setsid()
pid = os.fork()
if pid > 0:
    os._exit(0)
os.execv(fixture, [fixture, "leaf", pid_file, "ignore-term"])
PY
}

wait_for_pid_file() {
    local path="$1" attempt

    for attempt in $(seq 1 100); do
        if [ -s "$path" ]; then
            return 0
        fi
        sleep 0.05
    done
    printf 'timed out waiting for pid file: %s\n' "$path" >&2
    return 1
}

record_identity() {
    local state="$1" role="$2" pid start

    IFS= read -r pid < "$state/pids/$role"
    start="$(process_start_time "$pid")"
    printf '%s:%s:%s\n' "$role" "$pid" "$start" > "$state/old/$role.identity"
}

try_escape() {
    local state="$1" token="$2" generation="$3" scope="$4" unit

    unit="cosh-gateway-containment-${token}-escape-${scope}-${generation}"
    if [ "$scope" = "user" ]; then
        if timeout 5s systemd-run --user --quiet --unit "$unit" /bin/sleep 300; then
            : > "$state/escape-succeeded.$scope.$generation"
            return 1
        fi
    elif timeout 5s systemd-run --system --quiet --unit "$unit" /bin/sleep 300; then
        : > "$state/escape-succeeded.$scope.$generation"
        return 1
    fi
}

launch() {
    local state="$1" token="$2" generation role

    mkdir -p "$state/pids" "$state/old"
    generation=0
    if [ -f "$state/generation" ]; then
        IFS= read -r generation < "$state/generation"
    fi
    case "$generation" in
        '' | *[!0-9]*) exit 65 ;;
    esac
    generation=$((generation + 1))
    [ -f "$state/preclean.$generation" ] || exit 73
    printf '%s\n' "$generation" > "$state/generation"
    rm -f -- "$state"/pids/*

    try_escape "$state" "$token" "$generation" user || exit 74
    try_escape "$state" "$token" "$generation" system || exit 75
    : > "$state/escape-denied.$generation"

    printf '%s\n' "$BASHPID" > "$state/pids/main"
    "$0" leaf "$state/pids/direct" normal &
    "$0" spawn-grandchild "$state/pids/grandchild" &
    "$0" double-fork "$state/pids/double-fork"
    setsid -f "$0" leaf "$state/pids/setsid" ignore-term

    for role in direct grandchild double-fork setsid; do
        wait_for_pid_file "$state/pids/$role"
    done
    if [ "$generation" -eq 1 ]; then
        record_identity "$state" main
        for role in direct grandchild double-fork setsid; do
            record_identity "$state" "$role"
        done
    fi
    : > "$state/ready.$generation"
    wait
}

case "${1:-}" in
    assert-core-filesystem)
        [ "$#" -eq 4 ] || usage
        assert_core_filesystem "$2" "$3" "$4"
        ;;
    assert-clean)
        [ "$#" -eq 2 ] || usage
        assert_clean "$2"
        ;;
    launch)
        [ "$#" -eq 3 ] || usage
        launch "$2" "$3"
        ;;
    leaf)
        [ "$#" -eq 3 ] || usage
        leaf "$2" "$3"
        ;;
    spawn-grandchild)
        [ "$#" -eq 2 ] || usage
        spawn_grandchild "$2"
        ;;
    double-fork)
        [ "$#" -eq 2 ] || usage
        double_fork "$2"
        ;;
    *) usage ;;
esac

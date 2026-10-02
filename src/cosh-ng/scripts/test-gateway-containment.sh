#!/usr/bin/env bash
# Isolated destructive acceptance test for systemd-owned Gateway descendants.
set -euo pipefail

readonly RUN_SWITCH="COSH_RUN_GATEWAY_CONTAINMENT_TEST"
readonly DISPOSABLE_ATTESTATION="COSH_DISPOSABLE_SYSTEMD_CONTAINER"
readonly SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly SOURCE_FIXTURE="$SCRIPT_DIR/fixtures/gateway-containment-tree.sh"
readonly PACKAGED_UNIT="$SCRIPT_DIR/../packaging/systemd/cosh-gateway@.service.in"

skip() {
    printf 'SKIP gateway containment: %s\n' "$1"
    exit 0
}

fail() {
    printf 'FAIL gateway containment: %s\n' "$1" >&2
    exit 1
}

if [ "${!RUN_SWITCH:-}" != "1" ]; then
    skip "set $RUN_SWITCH=1 only inside a disposable systemd container"
fi
if [ "${!DISPOSABLE_ATTESTATION:-}" != "I_UNDERSTAND_THIS_MUTATES_SYSTEMD" ]; then
    skip "set $DISPOSABLE_ATTESTATION=I_UNDERSTAND_THIS_MUTATES_SYSTEMD to attest disposal"
fi
if [ "$(id -u)" -ne 0 ]; then
    skip "the isolated system manager test requires root inside the container"
fi
if [ ! -r /proc/1/comm ] || [ "$(tr -d '\n' < /proc/1/comm)" != "systemd" ]; then
    skip "PID 1 is not systemd"
fi
if ! command -v systemd-detect-virt >/dev/null 2>&1 \
    || ! systemd-detect-virt --container --quiet; then
    skip "no disposable container boundary was detected"
fi
if [ ! -r /sys/fs/cgroup/cgroup.controllers ] || ! command -v findmnt >/dev/null 2>&1; then
    skip "a writable unified cgroup v2 systemd hierarchy is required"
fi
CGROUP_MOUNT_OPTIONS="$(findmnt -n -o OPTIONS --target /sys/fs/cgroup 2>/dev/null || true)"
case ",$CGROUP_MOUNT_OPTIONS," in
    *,rw,*) ;;
    *) skip "the container cgroup v2 hierarchy is read-only" ;;
esac
if [ ! -d /run/systemd/system ] || [ ! -w /run/systemd/system ] || [ ! -w /run ]; then
    skip "the disposable system manager runtime directory is not writable"
fi

for command in systemctl systemd-analyze systemd-run setpriv setsid python3 timeout seq grep sed; do
    command -v "$command" >/dev/null 2>&1 || skip "required command is unavailable: $command"
done
[ -x "$SOURCE_FIXTURE" ] || fail "fixture is not executable: $SOURCE_FIXTURE"
[ -r "$PACKAGED_UNIT" ] || fail "packaged Gateway unit is unavailable: $PACKAGED_UNIT"

for property in \
    "Type=exec" \
    "KillMode=control-group" \
    "SendSIGKILL=yes" \
    "FinalKillSignal=SIGKILL" \
    "Delegate=no" \
    "TimeoutStopSec=15" \
    "Restart=on-failure" \
    "NoNewPrivileges=true" \
    "PrivateTmp=true" \
    "PrivateDevices=true" \
    "TemporaryFileSystem=/dev/shm:ro,nosuid,nodev,noexec" \
    "ProtectSystem=strict" \
    "ProtectControlGroups=true" \
    "InaccessiblePaths=/run/user" \
    "RestrictSUIDSGID=false"; do
    grep -Fqx -- "$property" "$PACKAGED_UNIT" \
        || fail "packaged Gateway unit is missing containment property: $property"
done

WORK_DIR="$(mktemp -d /run/cosh-gateway-containment.XXXXXX)"
chmod 0755 "$WORK_DIR"
TOKEN="$(basename "$WORK_DIR" | tr -cd 'A-Za-z0-9')"
BASE="cosh-gateway-containment-${TOKEN}"
INSTANCE="containment${TOKEN}"
UNIT="cosh-gateway@${INSTANCE}.service"
UNIT_FILE="/run/systemd/system/cosh-gateway@.service"
DROP_IN_DIR="/run/systemd/system/${UNIT}.d"
DROP_IN="$DROP_IN_DIR/fixture.conf"
WS_CKPT_UNIT_FILE="/run/systemd/system/ws-ckpt.service"
ENVIRONMENT_FILE="/etc/cosh/gateway-${INSTANCE}.env"
USER_RUNTIME_DIR="/run/user/65534"
POSITIVE_USER_UNIT="${BASE}-positive-user.service"
FIXTURE="$WORK_DIR/tree-fixture.sh"
WORKSPACE_ROOT="/opt/${BASE}"
WORKSPACE="$WORKSPACE_ROOT/workspace"
STATE="/var/lib/cosh-gateway-${INSTANCE}"
SHM_MARKER="${BASE}-suid-probe"
HOST_SHM_PROBE="/dev/shm/${SHM_MARKER}"
CONTROL_GROUP=""

[ ! -e "$UNIT_FILE" ] || fail "refusing to replace an existing runtime Gateway template"
[ ! -e "$DROP_IN_DIR" ] || fail "refusing to replace an existing Gateway drop-in"
[ ! -e "$WS_CKPT_UNIT_FILE" ] || fail "refusing to replace an existing runtime ws-ckpt unit"
[ ! -e "$ENVIRONMENT_FILE" ] || fail "refusing to replace an existing Gateway environment"
[ ! -e "$WORKSPACE_ROOT" ] || fail "refusing to reuse an existing workspace root"
[ ! -e "$STATE" ] || fail "refusing to reuse an existing Gateway state directory"
[ ! -e "$HOST_SHM_PROBE" ] || fail "refusing to reuse an existing /dev/shm probe"
! systemctl is-active --quiet user@65534.service \
    || fail "refusing to reuse an active fixture user manager"

run_as_fixture_user() {
    setpriv --reuid=65534 --regid=65534 --clear-groups \
        env XDG_RUNTIME_DIR="$USER_RUNTIME_DIR" "$@"
}

cleanup() {
    local generation

    set +e
    systemctl stop "$UNIT" >/dev/null 2>&1
    systemctl reset-failed "$UNIT" >/dev/null 2>&1
    for generation in $(seq 1 8); do
        systemctl stop \
            "${BASE}-escape-system-${generation}.service" >/dev/null 2>&1
        run_as_fixture_user systemctl --user stop \
            "${BASE}-escape-user-${generation}.service" >/dev/null 2>&1
    done
    run_as_fixture_user systemctl --user stop "$POSITIVE_USER_UNIT" >/dev/null 2>&1
    systemctl stop user@65534.service >/dev/null 2>&1
    systemctl stop ws-ckpt.service >/dev/null 2>&1
    systemctl reset-failed ws-ckpt.service >/dev/null 2>&1
    if [ "$UNIT_FILE" = "/run/systemd/system/cosh-gateway@.service" ]; then
        rm -f -- "$UNIT_FILE"
    fi
    rm -rf -- "$DROP_IN_DIR"
    rm -f -- "$WS_CKPT_UNIT_FILE" "$ENVIRONMENT_FILE"
    rm -f -- "$HOST_SHM_PROBE"
    systemctl daemon-reload >/dev/null 2>&1
    case "$WORK_DIR" in
        /run/cosh-gateway-containment.*) rm -rf -- "$WORK_DIR" ;;
    esac
    case "$WORKSPACE_ROOT" in
        /opt/cosh-gateway-containment-*) rm -rf -- "$WORKSPACE_ROOT" ;;
    esac
    case "$STATE" in
        /var/lib/cosh-gateway-containment*) rm -rf -- "$STATE" ;;
    esac
}
trap cleanup EXIT INT TERM

install -m 0755 "$SOURCE_FIXTURE" "$FIXTURE"
install -d -m 0755 -o 65534 -g 65534 "$WORKSPACE"
install -d -m 0755 /etc/cosh "$DROP_IN_DIR"
printf 'COSH_GATEWAY_WORKSPACE=%s\n' "$WORKSPACE" > "$ENVIRONMENT_FILE"
chmod 0600 "$ENVIRONMENT_FILE"

sed 's|{libexecdir}|/usr/libexec|g' "$PACKAGED_UNIT" > "$UNIT_FILE"
cat > "$WS_CKPT_UNIT_FILE" <<'EOF'
[Unit]
Description=Disposable ws-ckpt dependency
[Service]
Type=exec
ExecStart=/bin/sleep infinity
EOF
cat > "$DROP_IN" <<EOF
[Service]
User=65534
Group=65534
Environment=XDG_RUNTIME_DIR=$USER_RUNTIME_DIR
ExecStart=
ExecStartPre="$FIXTURE" assert-core-filesystem "$WORKSPACE" "$STATE" "$SHM_MARKER"
ExecStartPre="$FIXTURE" assert-clean "$STATE"
ExecStart="$FIXTURE" launch "$STATE" "$TOKEN"
EOF

systemctl daemon-reload
systemd-analyze verify "$UNIT" >/dev/null

systemctl start user@65534.service \
    || fail "the fixture user manager could not be started"
for attempt in $(seq 1 100); do
    [ -S "$USER_RUNTIME_DIR/systemd/private" ] && break
    sleep 0.05
done
[ -S "$USER_RUNTIME_DIR/systemd/private" ] \
    || fail "the fixture user manager control socket is unavailable"
run_as_fixture_user systemd-run --user --quiet --unit "$POSITIVE_USER_UNIT" /bin/sleep 300 \
    || fail "positive control could not create a same-UID sibling user unit"
POSITIVE_CONTROL_GROUP="$(run_as_fixture_user systemctl --user show \
    "$POSITIVE_USER_UNIT" --property=ControlGroup --value)"
case "$POSITIVE_CONTROL_GROUP" in
    '' | /) fail "positive control returned an unsafe user control group" ;;
esac
run_as_fixture_user systemctl --user stop "$POSITIVE_USER_UNIT"

systemctl start "$UNIT"

EFFECTIVE_PROPERTIES="$(systemctl show "$UNIT" \
    --property=Type,KillMode,SendSIGKILL,FinalKillSignal,Delegate,Restart,FragmentPath,DropInPaths,ProtectSystem,RestrictSUIDSGID,NoNewPrivileges,PrivateDevices,TemporaryFileSystem)"
for property in \
    "Type=exec" \
    "KillMode=control-group" \
    "SendSIGKILL=yes" \
    "FinalKillSignal=9" \
    "Delegate=no" \
    "Restart=on-failure" \
    "ProtectSystem=strict" \
    "RestrictSUIDSGID=no" \
    "NoNewPrivileges=yes" \
    "PrivateDevices=yes" \
    "TemporaryFileSystem=/dev/shm:ro,nosuid,nodev,noexec" \
    "FragmentPath=$UNIT_FILE"; do
    printf '%s\n' "$EFFECTIVE_PROPERTIES" | grep -Fqx -- "$property" \
        || fail "effective Gateway unit property mismatch: $property"
done
printf '%s\n' "$EFFECTIVE_PROPERTIES" | grep -F -- "$DROP_IN" >/dev/null \
    || fail "systemd did not load the fixture-only ExecStart drop-in"
systemctl show "$UNIT" --property=Environment --value \
    | grep -F -- "HOME=/var/lib/cosh-gateway-${INSTANCE}/core-home" >/dev/null \
    || fail "systemd did not isolate brokered Core HOME below private state"
[ -s "$STATE/dev-shm-containment-result" ] \
    || fail "the private /dev/shm containment probe did not complete"
[ ! -e "$HOST_SHM_PROBE" ] \
    || fail "the Core unit created a host-visible SUID/SGID /dev/shm artifact"
case "$(systemctl show "$UNIT" --property=TimeoutStopUSec --value)" in
    15s | 15000000us) ;;
    *) fail "effective Gateway stop timeout differs from the packaged 15 seconds" ;;
esac

wait_for_file() {
    local path="$1" label="$2" attempt

    # The packaged unit intentionally allows 15 seconds for graceful stop,
    # followed by a two-second restart delay before the replacement preflight.
    for attempt in $(seq 1 800); do
        if [ -e "$path" ]; then
            return 0
        fi
        if systemctl is-failed --quiet "$UNIT"; then
            systemctl status "$UNIT" --no-pager >&2 || true
            fail "$label failed before becoming ready"
        fi
        sleep 0.05
    done
    systemctl status "$UNIT" --no-pager >&2 || true
    fail "timed out waiting for $label"
}

process_start_time() {
    local pid="$1" suffix

    [ -r "/proc/$pid/stat" ] || return 1
    IFS= read -r suffix < "/proc/$pid/stat"
    suffix="${suffix##*) }"
    set -- $suffix
    [ "$#" -ge 20 ] || return 1
    printf '%s\n' "${20}"
}

same_process_is_alive() {
    local pid="$1" expected="$2" actual

    actual="$(process_start_time "$pid" 2>/dev/null)" || return 1
    [ "$actual" = "$expected" ]
}

process_cgroup() {
    local pid="$1" hierarchy controllers path

    while IFS=: read -r hierarchy controllers path; do
        if [ "$hierarchy" = "0" ] && [ -z "$controllers" ]; then
            printf '%s\n' "$path"
            return 0
        fi
    done < "/proc/$pid/cgroup"
    return 1
}

assert_generation_membership() {
    local role pid actual

    for role in main direct grandchild double-fork setsid; do
        IFS= read -r pid < "$STATE/pids/$role"
        actual="$(process_cgroup "$pid")" || fail "$role process is not alive"
        [ "$actual" = "$CONTROL_GROUP" ] \
            || fail "$role escaped the service cgroup before the kill point"
    done
}

wait_for_file "$STATE/ready.1" "first generation"
[ -e "$STATE/escape-denied.1" ] || fail "escape attempts were not observed"
if find "$STATE" -name 'escape-succeeded.*' -print -quit | grep -q .; then
    fail "systemd transient-unit escape unexpectedly succeeded"
fi

CONTROL_GROUP="$(systemctl show "$UNIT" --property=ControlGroup --value)"
case "$CONTROL_GROUP" in
    '' | /) fail "systemd returned an unsafe control group" ;;
esac
[ "$POSITIVE_CONTROL_GROUP" != "$CONTROL_GROUP" ] \
    || fail "positive control did not leave the Gateway service cgroup"
assert_generation_membership

OLD_MAIN_PID="$(systemctl show "$UNIT" --property=MainPID --value)"
IFS= read -r FIXTURE_MAIN_PID < "$STATE/pids/main"
[ "$OLD_MAIN_PID" = "$FIXTURE_MAIN_PID" ] \
    || fail "fixture is not the systemd-tracked main process"

kill -KILL "$OLD_MAIN_PID"
wait_for_file "$STATE/preclean.2" "old-cgroup cleanup checkpoint"
wait_for_file "$STATE/ready.2" "replacement generation"
[ -e "$STATE/escape-denied.2" ] || fail "replacement escape attempts were not observed"

while IFS=: read -r role pid start; do
    if same_process_is_alive "$pid" "$start"; then
        fail "old $role process survived Gateway SIGKILL: pid=$pid"
    fi
done < <(cat "$STATE"/old/*.identity)

NEW_MAIN_PID="$(systemctl show "$UNIT" --property=MainPID --value)"
[ "$NEW_MAIN_PID" != "$OLD_MAIN_PID" ] || fail "systemd did not launch a replacement main process"
[ "$(systemctl show "$UNIT" --property=ActiveState --value)" = "active" ] \
    || fail "replacement unit is not active"
assert_generation_membership

systemctl stop "$UNIT"
for attempt in $(seq 1 100); do
    CGROUP_PROCS="/sys/fs/cgroup${CONTROL_GROUP}/cgroup.procs"
    if [ ! -e "$CGROUP_PROCS" ] || ! IFS= read -r _ < "$CGROUP_PROCS"; then
        printf 'PASS gateway containment: SIGKILL descendants reaped before restart readiness\n'
        exit 0
    fi
    sleep 0.05
done
fail "service cgroup remained populated after final stop"

#!/bin/sh
# Lightweight health probe for agent-sec-daemon.

set -eu

readonly DEFAULT_TIMEOUT_SECONDS="1"

fail() {
    printf 'agent-sec-daemon health check failed: %s\n' "$1" >&2
    exit 1
}

usage() {
    printf '%s\n' \
        "Usage: ${0##*/}" \
        "" \
        "Checks daemon.health status over its Unix socket."
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        -h|--help)
            usage
            exit 0
            ;;
        *)
            fail "unknown argument: $1"
            ;;
    esac
    shift
done

for command_name in curl jq; do
    command -v "$command_name" >/dev/null 2>&1 || fail "missing command: $command_name"
done

if [ -n "${AGENT_SEC_DAEMON_SOCKET:-}" ]; then
    socket_path=$AGENT_SEC_DAEMON_SOCKET
elif [ -n "${XDG_RUNTIME_DIR:-}" ]; then
    socket_path="$XDG_RUNTIME_DIR/agent-sec-core/daemon.sock"
else
    current_uid=$(id -u)
    if [ -d "/run/user/$current_uid" ]; then
        socket_path="/run/user/$current_uid/agent-sec-core/daemon.sock"
    else
        fail "XDG_RUNTIME_DIR is required when AGENT_SEC_DAEMON_SOCKET is unset"
    fi
fi
timeout_seconds=${AGENT_SEC_HEALTH_TIMEOUT_SECONDS:-$DEFAULT_TIMEOUT_SECONDS}

case "$socket_path" in
    /*) ;;
    *) fail "socket path must be absolute: $socket_path" ;;
esac
[ -S "$socket_path" ] || fail "socket is missing or is not a Unix socket: $socket_path"

request='{"method":"daemon.health","params":{},"trace_context":{},"caller":"container-health","timeout_ms":1000}'
response=$(
    printf '%s\n' "$request" |
        curl \
            --silent \
            --show-error \
            --no-buffer \
            --proto '=telnet' \
            --connect-timeout "$timeout_seconds" \
            --max-time "$timeout_seconds" \
            --unix-socket "$socket_path" \
            --upload-file - \
            telnet://localhost
) || fail "daemon.health request failed (socket: $socket_path, timeout: ${timeout_seconds}s)"

printf '%s\n' "$response" |
    jq --exit-status --slurp \
        'length == 1
            and .[0].ok == true
            and .[0].data.status == "ok"' >/dev/null ||
    fail "daemon returned an invalid or unhealthy response"

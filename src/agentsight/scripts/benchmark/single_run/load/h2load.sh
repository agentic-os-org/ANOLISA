#!/usr/bin/env bash
set -euo pipefail

if ! command -v h2load >/dev/null 2>&1; then
    echo "h2load is required for the HTTP/2 benchmark" >&2
    exit 127
fi

: "${BASE_URL:?set BASE_URL, for example https://127.0.0.1:8443}"
: "${QPS:?set QPS}"
: "${DURATION:?set DURATION in seconds}"
: "${PAYLOAD_FILE:?set PAYLOAD_FILE}"

requests=$((QPS * DURATION))
h2load -n"$requests" \
    --clients="${CLIENTS:-100}" --max-concurrent-streams="${STREAMS:-10}" \
    --threads="${THREADS:-1}" --header="content-type: application/json" \
    --data="$PAYLOAD_FILE" "$BASE_URL/v1/chat/completions"

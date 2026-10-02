#!/bin/bash
# Supervises the trace and serve workers. SIGHUP (systemctl reload) restarts
# both workers so they re-read config.json; SIGTERM/SIGINT stops them and
# exits. Deliberately no `set -e`: wait returns non-zero whenever a signal
# arrives or a worker fails, and both cases are handled explicitly below.

start_workers() {
    agentsight trace &
    TRACE_PID=$!

    agentsight serve --host 0.0.0.0 &
    SERVE_PID=$!
}

stop_workers() {
    kill "$TRACE_PID" "$SERVE_PID" 2>/dev/null || true
    wait "$TRACE_PID" "$SERVE_PID" 2>/dev/null
}

RELOAD=0
TERMINATE=0
trap 'RELOAD=1' SIGHUP
trap 'TERMINATE=1' SIGTERM SIGINT

while true; do
    RELOAD=0
    start_workers

    # Wait for any worker to exit; a trapped signal also interrupts wait.
    wait -n
    exit_code=$?

    if [ "$TERMINATE" -eq 1 ]; then
        stop_workers
        exit 0
    fi

    if [ "$RELOAD" -eq 1 ]; then
        # Reload requested: restart workers so they pick up config changes.
        stop_workers
        continue
    fi

    # A worker exited on its own: stop the sibling and propagate the code.
    stop_workers
    exit "$exit_code"
done

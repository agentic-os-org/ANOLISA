#!/bin/sh
set -eu

ready=/run/asc-egress.ready
pidfile=/run/asc-egress.gost.pid
gost_pid=''
cleanup() {
  rm -f "$ready" "$pidfile"
  if [ -n "$gost_pid" ]; then
    kill "$gost_pid" 2>/dev/null || true
    wait "$gost_pid" 2>/dev/null || true
  fi
  # Main can retain this network namespace after sidecar exit. Never open it.
}
trap cleanup EXIT
trap 'exit 143' TERM
trap 'exit 130' INT

rm -f "$ready" "$pidfile"
test -s /etc/asc-egress/policy.json
/usr/local/bin/asc-egress-install
/bin/gost -C /etc/asc-egress/policy.json &
gost_pid="$!"
printf '%s\n' "$gost_pid" > "$pidfile"

# The public Compose dependency waits for this exact readiness contract.
attempt=0
while [ "$attempt" -lt 10 ]; do
  kill -0 "$gost_pid" 2>/dev/null || exit 1
  for table in /proc/net/tcp /proc/net/tcp6; do
    if [ -r "$table" ] && awk '$2 ~ /:3039$/ && $4 == "0A" {found=1} END {exit !found}' "$table"; then
      touch "$ready"
      echo 'ASC egress ready'
      wait "$gost_pid"
      exit 1
    fi
  done
  attempt=$((attempt + 1))
  sleep 1
done
echo 'GOST did not open the transparent listener' >&2
exit 1

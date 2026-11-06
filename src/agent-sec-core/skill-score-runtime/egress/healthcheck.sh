#!/bin/sh
set -eu
test -f /run/asc-egress.ready
test -r /run/asc-egress.gost.pid
kill -0 "$(cat /run/asc-egress.gost.pid)" 2>/dev/null
for table in /proc/net/tcp /proc/net/tcp6; do
  if [ -r "$table" ] && awk '$2 ~ /:3039$/ && $4 == "0A" {found=1} END {exit !found}' "$table"; then
    exit 0
  fi
done
exit 1

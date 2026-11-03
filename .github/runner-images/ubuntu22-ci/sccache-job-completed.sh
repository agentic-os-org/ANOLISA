#!/usr/bin/env bash
# Statistics and cleanup are best-effort, including cancelled jobs.
if [[ -n "${SCCACHE_REDIS_ENDPOINT:-}" ]]; then
  printf '::add-mask::%s\n' "$SCCACHE_REDIS_ENDPOINT"
fi
if [[ -n "${SCCACHE_REDIS_PASSWORD:-}" ]]; then
  printf '::add-mask::%s\n' "$SCCACHE_REDIS_PASSWORD"
fi
timeout 5s sccache --stop-server || true
exit 0

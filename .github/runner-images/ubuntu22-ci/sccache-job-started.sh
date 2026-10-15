#!/usr/bin/env bash
# Cache setup must not prevent a job from running.
if [[ -n "${SCCACHE_REDIS_ENDPOINT:-}" ]]; then
  printf '::add-mask::%s\n' "$SCCACHE_REDIS_ENDPOINT"
fi
if [[ -n "${SCCACHE_REDIS_PASSWORD:-}" ]]; then
  printf '::add-mask::%s\n' "$SCCACHE_REDIS_PASSWORD"
fi
# Retain compilation when a later daemon restart encounters a cache outage.
export SCCACHE_SKIP_CACHE_CHECK=1
if [[ -n "${GITHUB_ENV:-}" ]]; then
  echo 'SCCACHE_SKIP_CACHE_CHECK=1' >> "$GITHUB_ENV"
fi
if [[ -n "${SCCACHE_REDIS_ENDPOINT:-}" && -n "${SCCACHE_REDIS_PASSWORD:-}" ]] \
    && timeout 20s sccache --start-server; then
  echo 'Shared sccache ready (runner default: read-only)'
else
  echo '::warning::Shared sccache unavailable; compiling normally'
  if [[ -n "${GITHUB_ENV:-}" ]]; then
    echo 'RUSTC_WRAPPER=' >> "$GITHUB_ENV"
  fi
fi
exit 0

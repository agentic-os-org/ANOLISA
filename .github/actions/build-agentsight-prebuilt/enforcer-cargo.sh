#!/usr/bin/env bash
# Adapt the attested component builder's Cargo invocation to the fixed Cross profile.
set -euo pipefail
exec python3 "$AGENTSIGHT_COMMON_DIR/reproducible-build.py" \
    --source-root "$PWD" --source-date-epoch "$SOURCE_DATE_EPOCH" \
    -- "$AGENTSIGHT_COMMON_DIR/cross-profile.sh" "$AGENTSIGHT_CROSS_PROFILE" "$@"

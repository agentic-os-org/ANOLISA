#!/usr/bin/env bash
# tool-ready-kimi-wrapper.sh — Translate tokenless tool-ready output to Kimi Code hook protocol.
#
# Kimi Code's PreToolUse runner only blocks when the hook exits with code 2 or
# emits `permissionDecision == "deny"`. The shared tool_ready_hook.sh uses a
# generic protocol (`decision=block`, exit 0) that Kimi ignores, so this
# wrapper converts a block decision into the Kimi-recognized format.
set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

# Fail open: any wrapper/internal error emits empty JSON and exits 0 so the
# host tool call is never accidentally blocked by our plumbing.
fail_open() { printf '%s\n' '{}'; exit 0; }

# Locate the shared hook dispatcher. Same search order as run-hook.sh plus
# the wrapper's own directory tree.
DISPATCHER="${SCRIPT_DIR}/run-hook.sh"
[ -f "$DISPATCHER" ] || DISPATCHER="${SCRIPT_DIR}/../../common/hooks/run-hook.sh"
[ -f "$DISPATCHER" ] || DISPATCHER="/usr/local/share/anolisa/adapters/tokenless/kimicode/hooks/run-hook.sh"
[ -f "$DISPATCHER" ] || DISPATCHER="/usr/share/anolisa/adapters/tokenless/kimicode/hooks/run-hook.sh"
[ -f "$DISPATCHER" ] || fail_open

command -v jq >/dev/null 2>&1 || fail_open

# Capture stdin once and forward it to the shared hook.
INPUT="$(cat)"
[ -n "$INPUT" ] || INPUT='{}'

HOOK_OUTPUT="$(bash "$DISPATCHER" tool_ready_hook.sh <<<"$INPUT")" || fail_open
[ -n "$HOOK_OUTPUT" ] || fail_open

# Parse the shared hook decision.
DECISION="$(jq -r '.decision // empty' <<<"$HOOK_OUTPUT" 2>/dev/null || true)"

if [ "$DECISION" != "block" ]; then
    # Not a blocking decision: Kimi expects either empty JSON or the original
    # pass-through shape. Emit empty JSON to stay neutral.
    printf '%s\n' '{}'
    exit 0
fi

# Build the Kimi-compatible block response.
REASON="$(jq -r '.reason // .systemMessage // empty' <<<"$HOOK_OUTPUT" 2>/dev/null || true)"
CONTEXT="$(jq -r '.hookSpecificOutput.additionalContext // .systemMessage // empty' <<<"$HOOK_OUTPUT" 2>/dev/null || true)"
[ -n "$REASON" ] || REASON="tokenless: tool not ready"
[ -n "$CONTEXT" ] || CONTEXT="$REASON"

# Kimi runner generates the Agent-visible reason from stderr, not from the
# JSON stdout field. Write the diagnostic there so the Agent sees the actual
# missing-dependency detail instead of a generic "Blocked by PreToolUse hook".
echo "$REASON" >&2

jq -n --arg reason "$REASON" --arg context "$CONTEXT" '{
  "permissionDecision": "deny",
  "reason": $reason,
  "hookSpecificOutput": {
    "additionalContext": $context
  }
}'

# Exit code 2 is what Kimi Code uses to block a PreToolUse hook.
exit 2

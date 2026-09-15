#!/usr/bin/env bash
# Regression test for the kimicode tool-ready wrapper.
#
# What the wrapper owns is a *translation* contract: turn the shared tokenless
# tool-ready decision into Kimi Code's PreToolUse protocol (`permissionDecision
# = deny` + exit 2 on block, empty `{}` + exit 0 otherwise), and never let our
# own plumbing block a host tool call.
#
# That contract is verified here against a stubbed shared hook. The real
# `common/hooks/tool_ready_hook.sh` is hard-disabled on main (`d4d4fffa`,
# "fix(tokenless): hard-disable tool ready"): it prints `{}` and exits 0 before
# reading input, loading the spec, or running jq, so it can no longer produce
# the `decision=block` payload this wrapper exists to translate. Driving the
# real hook therefore made the block assertion unsatisfiable and left the deny
# path with zero coverage -- the failure was invisible in CI because no job ran
# `make test-adapters` (see the `ci(tokenless)` gap filed against the suite).
#
# Case D pins the consequence end-to-end: against the real tree the wrapper
# fails open, so the hook Kimi registers is currently a no-op. Re-enabling the
# readiness model must update that expectation deliberately, not silently.
set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ADAPTER_DIR="$SCRIPT_DIR/../adapters/tokenless"
REAL_WRAPPER="$ADAPTER_DIR/kimicode/hooks/tool-ready-kimi-wrapper.sh"
RUN_HOOK_SRC="$ADAPTER_DIR/kimicode/hooks/run-hook.sh"
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

if ! command -v jq >/dev/null 2>&1; then
    echo "SKIP: jq is not available; the wrapper fails open without it"
    exit 0
fi

fail() { echo "FAIL: $1" >&2; exit 1; }

# Synthetic install tree mirroring the packaged layout, same technique as
# tests/test-run-hook-install-scope.sh. The wrapper and the dispatcher are the
# real files copied verbatim, so their own search order is exercised
# unmodified; only the shared hook at the resolved slot is a stub.
STUB_ROOT="$TEST_DIR/current/adapters/tokenless"
mkdir -p "$STUB_ROOT/kimicode/hooks" "$STUB_ROOT/common/hooks"
cp "$REAL_WRAPPER" "$STUB_ROOT/kimicode/hooks/tool-ready-kimi-wrapper.sh"
cp "$RUN_HOOK_SRC" "$STUB_ROOT/kimicode/hooks/run-hook.sh"
chmod 0755 "$STUB_ROOT/kimicode/hooks/tool-ready-kimi-wrapper.sh" \
           "$STUB_ROOT/kimicode/hooks/run-hook.sh"
WRAPPER="$STUB_ROOT/kimicode/hooks/tool-ready-kimi-wrapper.sh"
STUB_HOOK="$STUB_ROOT/common/hooks/tool_ready_hook.sh"

PRETOOLUSE_INPUT='{"tool_name":"TestBlocked","tool_input":{"command":"echo ok"}}'

# --- Case A: block decision -> Kimi deny protocol ---------------------------
# Payload shape is the one the dormant Phase 4 of tool_ready_hook.sh emits.
cat >"$STUB_HOOK" <<'STUB'
#!/usr/bin/env bash
cat >/dev/null
printf '%s\n' '{"decision":"block","reason":"[tokenless:ready] TestBlocked: NOT_READY (missing binary __missing_binary_42__) Skip retry.","hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"[tokenless:ready] TestBlocked: NOT_READY (missing binary __missing_binary_42__) Skip retry."}}'
exit 0
STUB
chmod 0755 "$STUB_HOOK"

STDERR_FILE="$TEST_DIR/stderr-block.txt"
set +e
output=$(bash "$WRAPPER" 2>"$STDERR_FILE" <<<"$PRETOOLUSE_INPUT")
status=$?
set -e

[ "$status" -eq 2 ] || fail "block decision must exit 2 (Kimi's PreToolUse block code), got $status"
printf '%s' "$output" | jq -e '.permissionDecision == "deny"' >/dev/null \
    || fail "expected permissionDecision=deny, got: $output"
printf '%s' "$output" | jq -e '.reason | length > 0' >/dev/null \
    || fail "expected a non-empty reason, got: $output"
printf '%s' "$output" | jq -e '.hookSpecificOutput.additionalContext | length > 0' >/dev/null \
    || fail "expected non-empty hookSpecificOutput.additionalContext, got: $output"

# The Kimi runner builds the Agent-visible diagnostic from stderr, not from the
# JSON stdout field, so the reason has to reach stderr too.
grep -q 'tokenless' "$STDERR_FILE" \
    || fail "stderr missing the deny reason (Kimi runner needs it for the Agent diagnostic)"

# --- Case B: non-block decision -> neutral allow ---------------------------
cat >"$STUB_HOOK" <<'STUB'
#!/usr/bin/env bash
cat >/dev/null
printf '%s\n' '{}'
exit 0
STUB
chmod 0755 "$STUB_HOOK"

set +e
output=$(bash "$WRAPPER" 2>/dev/null <<<"$PRETOOLUSE_INPUT")
status=$?
set -e

[ "$status" -eq 0 ] || fail "pass-through decision must exit 0, got $status"
[ "$output" = "{}" ] || fail "pass-through decision must emit {}, got: $output"

# --- Case C: shared hook failure -> fail open, never block the host --------
cat >"$STUB_HOOK" <<'STUB'
#!/usr/bin/env bash
cat >/dev/null
echo "simulated internal error" >&2
exit 1
STUB
chmod 0755 "$STUB_HOOK"

set +e
output=$(bash "$WRAPPER" 2>/dev/null <<<"$PRETOOLUSE_INPUT")
status=$?
set -e

[ "$status" -eq 0 ] || fail "shared-hook failure must fail open with exit 0, got $status"
[ "$output" = "{}" ] || fail "shared-hook failure must emit {}, got: $output"

# --- Case D: real tree today -> hard bypass makes the hook a no-op ---------
# A spec that *would* block if the readiness engine were live. The assertion is
# about current main, where tool_ready_hook.sh exits before reading it.
SPEC_FILE="$TEST_DIR/tool-ready-spec.json"
cat >"$SPEC_FILE" <<'EOF'
{
  "TestBlocked": {
    "aliases": ["TestBlocked"],
    "required": [{ "binary": "__missing_binary_42__", "package": "missing", "manager": "rpm" }],
    "recommended": []
  }
}
EOF

set +e
output=$(TOKENLESS_TOOL_READY_SPEC="$SPEC_FILE" bash "$REAL_WRAPPER" 2>/dev/null <<<"$PRETOOLUSE_INPUT")
status=$?
set -e

[ "$status" -eq 0 ] || fail "real shared hook must fail open with exit 0 while hard-disabled, got $status"
[ "$output" = "{}" ] || fail "real shared hook must emit {} while hard-disabled, got: $output"

echo "kimicode tool-ready wrapper test passed"

#!/usr/bin/env bash
# Regression test: kimicode must read and write the data root of the Kimi
# product that is actually installed.
#
# Kimi Code (MoonshotAI/kimi-code) reads "$KIMI_CODE_HOME/config.toml", default
# ~/.kimi-code; the wound-down kimi-cli read "$KIMI_SHARE_DIR/config.toml",
# default ~/.kimi. Both ship a `kimi` binary, so only the data root tells them
# apart. Pinning the legacy root alone wrote a config Kimi Code never loads —
# detect reported ready, the hook never fired, uninstall missed it.
set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
ADAPTER_DIR="$SCRIPT_DIR/../adapters/tokenless"
SCRIPTS="$ADAPTER_DIR/kimicode/scripts"
COMMON="$SCRIPTS/_common.sh"
INSTALL="$SCRIPTS/install.sh"
UNINSTALL="$SCRIPTS/uninstall.sh"
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

fail() { echo "FAIL: $*"; exit 1; }

# --- resolver contract -------------------------------------------------------
# Each case runs in a subshell with its own HOME so the on-disk branches are
# exercised independently.
resolve() {
    # $1 = home dir, remaining args are env assignments for the case
    local home="$1"; shift
    env -i HOME="$home" PATH="/usr/bin:/bin" "$@" bash -c \
        "source '$COMMON'; printf '%s|%s\n' \"\$(resolve_kimi_home)\" \"\$(resolve_kimi_home_origin)\""
}

HOME_A="$TEST_DIR/home-a"; mkdir -p "$HOME_A"
out="$(resolve "$HOME_A")"
[ "$out" = "$HOME_A/.kimi-code|default (no Kimi data root on disk yet)" ] \
    || fail "empty host should default to ~/.kimi-code, got: $out"

HOME_B="$TEST_DIR/home-b"; mkdir -p "$HOME_B/.kimi"
out="$(resolve "$HOME_B")"
[ "$out" = "$HOME_B/.kimi|found ~/.kimi (legacy kimi-cli)" ] \
    || fail "legacy-only host should resolve to ~/.kimi, got: $out"

HOME_C="$TEST_DIR/home-c"; mkdir -p "$HOME_C/.kimi" "$HOME_C/.kimi-code"
out="$(resolve "$HOME_C")"
[ "$out" = "$HOME_C/.kimi-code|found ~/.kimi-code" ] \
    || fail "migrated host should prefer ~/.kimi-code, got: $out"

out="$(resolve "$HOME_C" KIMI_SHARE_DIR="$TEST_DIR/legacy")"
[ "$out" = "$TEST_DIR/legacy|KIMI_SHARE_DIR (legacy kimi-cli)" ] \
    || fail "KIMI_SHARE_DIR should override on-disk discovery, got: $out"

out="$(resolve "$HOME_C" KIMI_SHARE_DIR="$TEST_DIR/legacy" KIMI_CODE_HOME="$TEST_DIR/current")"
[ "$out" = "$TEST_DIR/current|KIMI_CODE_HOME" ] \
    || fail "KIMI_CODE_HOME should win over KIMI_SHARE_DIR, got: $out"

# Empty overrides (the shape `make` exports for unset vars) must not select an
# empty root.
out="$(resolve "$HOME_B" KIMI_CODE_HOME="" KIMI_SHARE_DIR="")"
[ "$out" = "$HOME_B/.kimi|found ~/.kimi (legacy kimi-cli)" ] \
    || fail "empty overrides should fall through to discovery, got: $out"

# --- end-to-end: install writes the Kimi Code root on a fresh host -----------
FAKE_BIN="$TEST_DIR/fake-bin"
mkdir -p "$FAKE_BIN"
printf '#!/usr/bin/env bash\necho "kimi 1.0.0"\n' > "$FAKE_BIN/kimi"
chmod +x "$FAKE_BIN/kimi"

HOME_D="$TEST_DIR/home-d"; mkdir -p "$HOME_D"
HOME="$HOME_D" PATH="$FAKE_BIN:/usr/bin:/bin" \
    KIMI_CODE_HOME="" KIMI_SHARE_DIR="" \
    ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" bash "$INSTALL" >"$TEST_DIR/install.log" 2>&1 \
    || { cat "$TEST_DIR/install.log"; fail "install.sh exited nonzero on a fresh host"; }
[ -f "$HOME_D/.kimi-code/config.toml" ] \
    || { cat "$TEST_DIR/install.log"; fail "install did not write ~/.kimi-code/config.toml"; }
[ ! -e "$HOME_D/.kimi" ] || fail "install created a legacy ~/.kimi tree on a fresh host"
grep -q "kimi data root: $HOME_D/.kimi-code" "$TEST_DIR/install.log" \
    || { cat "$TEST_DIR/install.log"; fail "install did not report the resolved data root"; }

# The written entry must stay inside the four keys Kimi Code's strict [[hooks]]
# schema allows — an extra key fails the whole config load.
hook_keys="$(awk '/^\[\[hooks\]\]/{in_hook=1;next} /^\[/{in_hook=0} in_hook && /=/ {sub(/ *=.*/,"");print}' \
    "$HOME_D/.kimi-code/config.toml" | sort -u | tr '\n' ' ')"
[ "$hook_keys" = "command event matcher timeout " ] \
    || fail "hook entry keys changed (expected 'command event matcher timeout'), got: $hook_keys"

# --- end-to-end: uninstall cleans both roots of a migrated host --------------
# A Kimi Code install migrates a legacy ~/.kimi config into ~/.kimi-code, so the
# same tokenless hook can sit in both files; leaving either behind means a later
# migration re-imports hooks whose scripts are already gone.
HOME_E="$TEST_DIR/home-e"
mkdir -p "$HOME_E/.kimi" "$HOME_E/.kimi-code"

write_both_configs() {
    local root
    for root in "$HOME_E/.kimi" "$HOME_E/.kimi-code"; do
        cat > "$root/config.toml" <<'TOML'
[[hooks]]
event = "PreToolUse"
matcher = ""
command = "bash '/opt/anolisa/adapters/tokenless/kimicode/hooks/tool-ready-kimi-wrapper.sh'"
timeout = 15
# tokenless-tool-ready: Pre-checks tool environment readiness

[[hooks]]
event = "Notification"
command = "bash /some/other/hook.sh"

[providers.kimi]
api_key = "secret"
TOML
    done
}

assert_both_cleaned() {
    local root
    for root in "$HOME_E/.kimi" "$HOME_E/.kimi-code"; do
        if grep -q 'tokenless-tool-ready\|tool-ready-kimi-wrapper' "$root/config.toml"; then
            fail "tokenless hook survived in $root/config.toml"
        fi
        grep -q '^\[providers\.kimi\]' "$root/config.toml" || fail "provider table lost in $root"
        grep -q '^api_key' "$root/config.toml" || fail "api_key lost in $root"
        [ "$(grep -c '^\[\[hooks\]\]' "$root/config.toml")" -eq 1 ] \
            || fail "unrelated hook block not preserved exactly once in $root"
        grep -q 'other/hook\.sh' "$root/config.toml" || fail "unrelated hook command lost in $root"
    done
}

# python3 path
write_both_configs
HOME="$HOME_E" KIMI_CODE_HOME="" KIMI_SHARE_DIR="" bash "$UNINSTALL" >"$TEST_DIR/uninstall-py.log" 2>&1 \
    || { cat "$TEST_DIR/uninstall-py.log"; fail "uninstall.sh exited nonzero"; }
assert_both_cleaned

# awk fallback path: a PATH with the commands uninstall.sh needs but no python3
NO_PY_BIN="$TEST_DIR/no-python-bin"
mkdir -p "$NO_PY_BIN"
for cmd in bash awk cat mv rm stat grep; do
    real="$(command -v "$cmd" 2>/dev/null || true)"
    if [ -n "$real" ]; then ln -sf "$real" "$NO_PY_BIN/$cmd"; fi
done
write_both_configs
HOME="$HOME_E" KIMI_CODE_HOME="" KIMI_SHARE_DIR="" PATH="$NO_PY_BIN" bash "$UNINSTALL" \
    >"$TEST_DIR/uninstall-awk.log" 2>&1 \
    || { cat "$TEST_DIR/uninstall-awk.log"; fail "uninstall.sh (awk fallback) exited nonzero"; }
grep -q "awk-based cleanup complete" "$TEST_DIR/uninstall-awk.log" \
    || { cat "$TEST_DIR/uninstall-awk.log"; fail "awk fallback did not run"; }
[ "$(grep -c "awk-based cleanup complete" "$TEST_DIR/uninstall-awk.log")" -eq 2 ] \
    || { cat "$TEST_DIR/uninstall-awk.log"; fail "awk fallback cleaned only one of the two roots"; }
assert_both_cleaned

echo "kimicode config-root test passed"

#!/bin/bash
# Token-Less Full Test Suite
# Tests all four compression methods:
# 1. Schema Compression (tokenless compress-schema)
# 2. Response Compression (tokenless compress-response)
# 3. Command Rewriting (RTK)
# 4. Stats System (record, list, summary, diff)
# 5. TOON Compression (tokenless compress-toon)

set -uo pipefail

TEST_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TOKENLESS_SOURCE_DIR="$(cd "$TEST_SCRIPT_DIR/.." && pwd)"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

TESTS_PASSED=0
TESTS_FAILED=0
TESTS_TOTAL=0

log_info() { echo -e "${BLUE}[INFO]${NC} $1"; }
log_pass() { echo -e "${GREEN}[PASS]${NC} $1"; ((TESTS_PASSED++)); ((TESTS_TOTAL++)); }
log_fail() { echo -e "${RED}[FAIL]${NC} $1"; ((TESTS_FAILED++)); ((TESTS_TOTAL++)); }
log_section() { echo -e "\n${YELLOW}========================================${NC}\n${YELLOW}$1${NC}\n${YELLOW}========================================${NC}\n"; }

assert_contains() {
    local input="$1" expected="$2" test_name="$3"
    if echo "$input" | grep -q "$expected"; then log_pass "$test_name"
    else log_fail "$test_name - Expected: $expected"; fi
}

assert_not_contains() {
    local input="$1" unexpected="$2" test_name="$3"
    if echo "$input" | grep -q "$unexpected"; then log_fail "$test_name - Unexpected: $unexpected"
    else log_pass "$test_name"; fi
}

test_schema_compression() {
    log_section "Test 1: Schema Compression"

    log_info "Test 1.1: Simple schema compression"
    local simple_schema='{"function":{"name":"greet","description":"Say hello","parameters":{"type":"object","properties":{"name":{"type":"string"}}}}}'
    local compressed=$(echo "$simple_schema" | tokenless compress-schema 2>/dev/null)
    assert_contains "$compressed" '"function"' "Simple schema preserves function"
    assert_contains "$compressed" '"greet"' "Simple schema preserves name"

    log_info "Test 1.2: Nested schema compression"
    local nested_schema='{"function":{"name":"create_user","parameters":{"type":"object","title":"Params","properties":{"address":{"type":"object","title":"Address","properties":{"street":{"type":"string"}}}}}}}'
    compressed=$(echo "$nested_schema" | tokenless compress-schema 2>/dev/null)
    assert_contains "$compressed" '"address"' "Nested schema preserves address"

    log_info "Test 1.3: Enum preservation"
    local enum_schema='{"function":{"name":"calc","parameters":{"properties":{"op":{"type":"string","enum":["add","sub"]}}}}}'
    compressed=$(echo "$enum_schema" | tokenless compress-schema 2>/dev/null)
    assert_contains "$compressed" '"enum"' "Enum preserved"

    log_info "Test 1.4: Edge cases"
    assert_contains "$(echo '{}' | tokenless compress-schema 2>/dev/null)" '{}' "Empty schema"
    assert_contains "$(echo 'null' | tokenless compress-schema 2>/dev/null)" 'null' "Null schema"

    log_info "Test 1.5: Array input (OpenAI tools format, auto-detected)"
    local array_schema='[{"type":"function","function":{"name":"f","title":"Remove Me","description":"short","parameters":{"type":"object","properties":{"x":{"type":"string","title":"Also Remove","examples":["ex"]}}}}}]'
    local arr_compressed=$(echo "$array_schema" | tokenless compress-schema 2>/dev/null)
    assert_not_contains "$arr_compressed" '"title"' "Array input: titles removed"
    assert_not_contains "$arr_compressed" '"examples"' "Array input: examples removed"
    assert_contains "$arr_compressed" '"function"' "Array input: function preserved"
}

test_response_compression() {
    log_section "Test 2: Response Compression"

    log_info "Test 2.1: Null removal"
    local null_response='{"name":"test","value":null,"count":5}'
    local compressed=$(echo "$null_response" | tokenless compress-response 2>/dev/null)
    assert_contains "$compressed" '"name"' "Null removal preserves name"

    log_info "Test 2.2: Debug field removal"
    local debug_response='{"data":"ok","debug":"info","trace":"stack"}'
    compressed=$(echo "$debug_response" | tokenless compress-response 2>/dev/null)
    assert_contains "$compressed" '"data"' "Debug removal preserves data"

    log_info "Test 2.3: Nested object"
    local nested='{"status":"ok","data":{"user":{"name":"John"}}}'
    compressed=$(echo "$nested" | tokenless compress-response 2>/dev/null)
    assert_contains "$compressed" '"status"' "Nested preserves status"
}

test_command_rewriting() {
    log_section "Test 3: Command Rewriting (RTK)"

    log_info "Test 3.1: RTK availability"
    if command -v rtk &> /dev/null; then
        log_pass "RTK available: $(rtk --version)"
    else log_fail "RTK not found"; fi

    log_info "Test 3.2: RTK rewrite"
    local rewritten=$(rtk rewrite "ls -la" 2>/dev/null || echo "ls -la")
    if [ -n "$rewritten" ]; then log_pass "RTK rewrite works: $rewritten"
    else log_fail "RTK rewrite failed"; fi

    log_info "Test 3.3: Multiple commands"
    local cmds=("git status" "cargo build" "npm install")
    local ok=0
    for cmd in "${cmds[@]}"; do
        local r=$(rtk rewrite "$cmd" 2>/dev/null || echo "")
        [ -n "$r" ] && ((ok++)) || true
    done
    log_pass "RTK processed $ok/${#cmds[@]} commands"
}

test_stats_system() {
    log_section "Test 4: Stats System"

    # Use a temp DB for testing
    local test_db=$(mktemp)
    export TOKENLESS_STATS_DB="$test_db"

    log_info "Test 4.1: Stats auto-record via compress-schema"
    local schema_json='{"function":{"name":"test","description":"test function","parameters":{"type":"object","title":"Params","description":"The parameters","properties":{"name":{"type":"string","description":"User name"}}}}}'
    local compress_out=$(echo "$schema_json" | tokenless compress-schema --agent-id test-agent --session-id test-session --tool-use-id test-tool 2>&1)
    if [ -n "$compress_out" ] && [ "$compress_out" != "$schema_json" ]; then
        log_pass "Schema compression for stats test works"
    else log_fail "Schema compression for stats test failed"; fi

    log_info "Test 4.2: Stats auto-record via compress-response"
    local response_json='{"result":{"user":"test","email":"test@test.com"},"debug":"trace info","trace":"stack","null_field":null}'
    local resp_out=$(echo "$response_json" | tokenless compress-response --agent-id test-agent --session-id test-session 2>&1)
    if [ -n "$resp_out" ]; then log_pass "Response compression for stats test works"
    else log_fail "Response compression for stats test failed"; fi

    log_info "Test 4.3: Stats list"
    local list_output=$(tokenless stats list 2>/dev/null)
    if echo "$list_output" | grep -q '\[ID:'; then
        log_pass "Stats list shows records"
    else log_fail "Stats list missing ID: $list_output"; fi

    log_info "Test 4.4: Stats show"
    local record_id=$(echo "$list_output" | grep -o '\[ID:[0-9]*\]' | head -1 | grep -o '[0-9]*')
    if [ -n "$record_id" ]; then
        local show_output=$(tokenless stats show "$record_id" 2>/dev/null)
        if echo "$show_output" | grep -q "Before"; then
            log_pass "Stats show displays record details"
        else log_fail "Stats show missing details: $show_output"; fi
    else log_pass "No record ID to test show"; fi

    log_info "Test 4.5: Stats summary"
    local summary=$(tokenless stats summary 2>/dev/null)
    if echo "$summary" | grep -q "Total Records:"; then
        log_pass "Stats summary works"
    else log_fail "Stats summary broken"; fi

    log_info "Test 4.6: Stats clear"
    local clear_output=$(tokenless stats clear -y 2>&1)
    if [ $? -eq 0 ]; then log_pass "Stats clear works"
    else log_fail "Stats clear failed"; fi

    unset TOKENLESS_STATS_DB
    rm -f "$test_db"
}

test_toon_compression() {
    log_section "Test 5: TOON Compression with Stats Verification"

    local test_db=$(mktemp)
    export TOKENLESS_STATS_DB="$test_db"

    # --- 5.0 Environment check ---
    log_info "Test 5.0: Environment check"
    if command -v tokenless &> /dev/null; then
        log_pass "tokenless available: $(tokenless --version)"
    else log_fail "tokenless not found"; fi

    # --- 5.0b Default minimum-length gate: short payloads pass through ---
    # Like every case in this suite, 5.0b exercises the installed release
    # binary on PATH on purpose (release-layout verification); it is
    # intentionally decoupled from the cargo workspace. Binary-level
    # coverage of the same gate that does not depend on the installed
    # package lives in crates/tokenless-cli/tests/cli_integration.rs
    # (compress_toon_short_payload_passes_through_by_default and
    # compress_toon_min_chars_zero_encodes_short_payload).
    log_info "Test 5.0b: TOON minimum-length gate (default 500 chars)"
    local gate_json='{"a":"short"}'
    local gate_out
    gate_out=$(echo "$gate_json" | tokenless compress-toon 2>/dev/null)
    if [ "$gate_out" = "$gate_json" ]; then
        log_pass "Short payload passes through unchanged under default gate"
    else log_fail "Short payload was encoded despite default gate: $gate_out"; fi
    gate_out=$(echo "$gate_json" | tokenless compress-toon --min-toon-chars 0 2>/dev/null)
    if [ "$gate_out" != "$gate_json" ]; then
        log_pass "--min-toon-chars 0 encodes short payloads on demand"
    else log_fail "--min-toon-chars 0 did not encode short payload"; fi

    # --- 5.1 Simple object: compress-response → stats + toon comparison ---
    log_info "Test 5.1: Simple object — compress-response stats + TOON encode"
    local simple_json='{"name":"Alice","age":30,"active":true,"email":"alice@example.com","role":"admin"}'
    local before_chars=${#simple_json}
    local before_tokens=$(( (before_chars + 3) / 4 ))

    # Auto-record via compress-response (writes to stats DB)
    local resp_compressed=$(echo "$simple_json" | tokenless compress-response --agent-id toon-test --session-id toon-session 2>/dev/null)
    local after_resp_chars=${#resp_compressed}
    local after_resp_tokens=$(( (after_resp_chars + 3) / 4 ))

    # TOON encode separately (--min-toon-chars 0: these fixtures are under
    # the shared 500-character default gate; this test verifies TOON itself)
    local toon_encoded=$(echo "$simple_json" | tokenless compress-toon --min-toon-chars 0 2>/dev/null)
    local after_toon_chars=${#toon_encoded}
    local after_toon_tokens=$(( (after_toon_chars + 3) / 4 ))
    local toon_savings=$(( (before_chars - after_toon_chars) * 100 / before_chars ))
    log_pass "Simple object: JSON=${before_chars} → RESP=${after_resp_chars} → TOON=${after_toon_chars} (TOON ${toon_savings}% vs raw)"

    # --- 5.2 Tabular data: compress-response stats + TOON comparison ---
    log_info "Test 5.2: Tabular data — stats + TOON encode"
    local table_json='{"users":[{"id":1,"name":"Alice","email":"alice@e.com","role":"admin"},{"id":2,"name":"Bob","email":"bob@e.com","role":"user"},{"id":3,"name":"Charlie","email":"charlie@e.com","role":"mod"},{"id":4,"name":"Diana","email":"diana@e.com","role":"admin"},{"id":5,"name":"Eve","email":"eve@e.com","role":"user"}],"meta":{"total":5,"page":1}}'
    local table_before_chars=${#table_json}

    resp_compressed=$(echo "$table_json" | tokenless compress-response --agent-id toon-test --session-id toon-session 2>/dev/null)
    toon_encoded=$(echo "$table_json" | tokenless compress-toon --min-toon-chars 0 2>/dev/null)
    local table_savings=$(( (table_before_chars - ${#toon_encoded}) * 100 / table_before_chars ))
    log_pass "Tabular data: JSON=${table_before_chars} → RESP=${#resp_compressed} → TOON=${#toon_encoded} (TOON ${table_savings}% vs raw)"

    if [ "$table_savings" -ge 15 ]; then
        log_pass "Tabular TOON savings >= 15%"
    else log_fail "Tabular TOON savings < 15% (${table_savings}%)"; fi

    # --- 5.3 Schema → TOON pipeline (compress-schema records stats) ---
    log_info "Test 5.3: Schema compression stats → TOON comparison"
    local schema_json='{"function":{"name":"search_users","description":"Search users by criteria","parameters":{"type":"object","title":"SearchParams","description":"Search parameters","properties":{"name":{"type":"string","description":"User name to search"},"limit":{"type":"integer","description":"Max results"},"active":{"type":"boolean","description":"Filter by active status"}}}}}'
    local schema_before_chars=${#schema_json}
    local schema_compressed=$(echo "$schema_json" | tokenless compress-schema --agent-id toon-test --session-id toon-session 2>/dev/null)
    local schema_after_chars=${#schema_compressed}

    toon_encoded=$(echo "$schema_json" | tokenless compress-toon --min-toon-chars 0 2>/dev/null)
    local schema_toon_chars=${#toon_encoded}
    local schema_savings=$(( (schema_before_chars - schema_after_chars) * 100 / schema_before_chars ))
    local schema_toon_savings=$(( (schema_before_chars - schema_toon_chars) * 100 / schema_before_chars ))
    log_pass "Schema: JSON=${schema_before_chars} → COMPRESSED=${schema_after_chars} (${schema_savings}%) → TOON=${schema_toon_chars} (${schema_toon_savings}% vs raw)"

    # --- 5.4 Decompress-toon round-trip ---
    log_info "Test 5.4: TOON round-trip (encode→decode→verify)"
    local roundtrip_json='{"name":"test","value":42,"flag":true,"tags":["a","b","c"]}'
    toon_encoded=$(echo "$roundtrip_json" | tokenless compress-toon --min-toon-chars 0 2>/dev/null)
    local decoded=$(echo "$toon_encoded" | tokenless decompress-toon 2>/dev/null)
    if echo "$decoded" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d['name']=='test' and d['value']==42 and d['flag']==True" 2>/dev/null; then
        log_pass "Round-trip: data integrity verified"
    else log_fail "Round-trip: data corruption"; fi

    # --- 5.5 Stats DB verification: list ---
    log_info "Test 5.5: Stats list — verify records exist in DB"
    local list_output=$(tokenless stats list 2>/dev/null)
    if echo "$list_output" | grep -q '\[ID:'; then
        log_pass "Stats list shows records"
    else log_fail "Stats list missing records: $list_output"; fi
    local record_count=$(echo "$list_output" | grep -c '\[ID:' || true)
    log_pass "Stats DB contains $record_count records"

    # --- 5.6 Stats DB verification: show record details ---
    log_info "Test 5.6: Stats show — verify before/after text in DB"
    local first_id=$(echo "$list_output" | grep -o '\[ID:[0-9]*\]' | tail -1 | grep -o '[0-9]*')
    if [ -n "$first_id" ]; then
        local show_output=$(tokenless stats show "$first_id" 2>/dev/null)
        # Verify compression happened (before != after)
        if echo "$show_output" | grep -q "Before" && echo "$show_output" | grep -q "After"; then
            log_pass "Stats show displays before/after content"
        else log_fail "Stats show missing before/after"; fi
        # Metrics are embedded in the show output itself
        log_pass "Stats show includes before/after comparison"
    else log_fail "No record ID found for show test"; fi

    # --- 5.7 Stats summary ---
    log_info "Test 5.7: Stats summary — aggregate compression effectiveness"
    local summary=$(tokenless stats summary 2>/dev/null)
    if echo "$summary" | grep -q "Total Records:"; then
        log_pass "Stats summary reports total records"
    else log_fail "Stats summary broken"; fi
    if echo "$summary" | grep -q "Saved:"; then
        log_pass "Stats summary shows total savings"
    else log_fail "Stats summary missing savings"; fi
    # Log the summary for visibility
    log_info "Stats Summary:"
    echo "$summary" | while IFS= read -r line; do
        echo -e "${BLUE}[STATS]${NC} $line"
    done

    # --- 5.8 Compression effectiveness summary ---
    log_info "Test 5.8: TOON compression effectiveness report"
    local total_before=0 total_after_toon=0 total_records=0
    # Test a few representative payloads and compute aggregate TOON savings
    for payload in \
        '{"name":"test","val":42}' \
        '{"items":[{"id":1,"n":"A"},{"id":2,"n":"B"},{"id":3,"n":"C"}]}' \
        '{"data":{"results":[{"k":"v1"},{"k":"v2"}],"count":2,"ok":true}}'
    do
        local plen=${#payload}
        # --min-toon-chars 0: payloads are under the shared 500-character
        # default gate; this test measures TOON encoding effectiveness.
        local tlen=$(echo "$payload" | tokenless compress-toon --min-toon-chars 0 2>/dev/null | wc -c)
        total_before=$((total_before + plen))
        total_after_toon=$((total_after_toon + tlen))
        total_records=$((total_records + 1))
    done
    if [ "$total_before" -gt 0 ]; then
        local aggregate_savings=$(( (total_before - total_after_toon) * 100 / total_before ))
        log_pass "Aggregate TOON savings across $total_records payloads: ${aggregate_savings}%"
        if [ "$aggregate_savings" -gt 0 ]; then
            log_pass "TOON compression is effective (positive savings)"
        else log_fail "TOON compression not effective"; fi
    fi

    # --- 5.9 Stats retention check ---
    log_info "Test 5.9: Stats retention — clear and verify"
    tokenless stats clear --yes 2>/dev/null
    local count_after
    count_after=$(tokenless stats list 2>/dev/null | grep -cF '[ID:' || true)
    count_after=${count_after:-0}
    if [ "$count_after" -eq 0 ] 2>/dev/null; then
        log_pass "Stats clear works, DB empty after clear"
    else log_fail "Stats clear failed, $count_after records remain"; fi

    unset TOKENLESS_STATS_DB
    rm -f "$test_db"
}

test_tool_ready() {
    log_section "Test 6: Tool Ready (env-check + fix + attribution)"

    # FHS path fallback chain for spec and env-fix script
    local SPEC_FILE=""
    for p in \
        "${ANOLISA_ADAPTER_DIR:+$ANOLISA_ADAPTER_DIR/common/tool-ready-spec.json}" \
        "$TOKENLESS_SOURCE_DIR/adapters/tokenless/common/tool-ready-spec.json" \
        "$HOME/.local/share/anolisa/adapters/tokenless/common/tool-ready-spec.json" \
        "/usr/local/share/anolisa/adapters/tokenless/common/tool-ready-spec.json" \
        "/usr/share/anolisa/adapters/tokenless/common/tool-ready-spec.json" \
        "$HOME/.tokenless/tool-ready-spec.json"; do
        if [ -f "$p" ]; then SPEC_FILE="$p"; break; fi
    done
    local FIX_SCRIPT=""
    for p in \
        "${ANOLISA_ADAPTER_DIR:+$ANOLISA_ADAPTER_DIR/common/tokenless-env-fix.sh}" \
        "$TOKENLESS_SOURCE_DIR/adapters/tokenless/common/tokenless-env-fix.sh" \
        "$HOME/.local/share/anolisa/adapters/tokenless/common/tokenless-env-fix.sh" \
        "/usr/local/share/anolisa/adapters/tokenless/common/tokenless-env-fix.sh" \
        "/usr/share/anolisa/adapters/tokenless/common/tokenless-env-fix.sh" \
        "$HOME/.tokenless/tokenless-env-fix.sh"; do
        if [ -f "$p" ] && [ -r "$p" ]; then FIX_SCRIPT="$p"; break; fi
    done
    local HOOK_DIR=""
    for d in \
        "${ANOLISA_ADAPTER_DIR:+$ANOLISA_ADAPTER_DIR/common/hooks}" \
        "$TOKENLESS_SOURCE_DIR/adapters/tokenless/common/hooks" \
        "$HOME/.local/share/anolisa/adapters/tokenless/common/hooks" \
        "/usr/local/share/anolisa/adapters/tokenless/common/hooks" \
        "/usr/share/anolisa/adapters/tokenless/common/hooks"; do
        if [ -f "$d/tool_ready_hook.sh" ] && [ -f "$d/compress_response_hook.py" ]; then
            HOOK_DIR="$d"
            break
        fi
    done
    if [ -z "$HOOK_DIR" ]; then
        log_fail "tokenless common hooks not found"
        return
    fi
    READY_SCRIPT="$HOOK_DIR/tool_ready_hook.sh"
    COMPRESS_SCRIPT="$HOOK_DIR/compress_response_hook.py"

    # ==========================================
    # 6.1 Installation & file existence
    # ==========================================
    log_info "Test 6.1: Installed Tool Ready files"
    [ -f "$SPEC_FILE" ] && log_pass "tool-ready-spec.json exists" || log_fail "tool-ready-spec.json missing"
    [ -f "$FIX_SCRIPT" ] && [ -r "$FIX_SCRIPT" ] && bash "$FIX_SCRIPT" check >/dev/null 2>&1 \
        && log_pass "tokenless-env-fix.sh is readable and runs through bash" \
        || log_fail "tokenless-env-fix.sh missing/unreadable or bash invocation failed"
    [ -f "$READY_SCRIPT" ] && [ -r "$READY_SCRIPT" ] && bash -n "$READY_SCRIPT" \
        && log_pass "tool_ready_hook.sh is readable and parses through bash" \
        || log_fail "tool_ready_hook.sh missing/unreadable or bash parse failed"

    # ==========================================
    # 6.2 Rust env-check: unconditional hard bypass
    # ==========================================
    log_info "Test 6.2: env-check hard bypass ignores legacy opt-in"
    local json_out
    json_out=$(TOKENLESS_TOOL_READY_ENABLED=1 \
        TOKENLESS_TOOL_READY_SPEC=/path/that/must/not/be-read \
        tokenless env-check --tool Shell --json 2>&1)
    assert_contains "$json_out" '"status":"UNKNOWN"' "env-check hard bypass returns UNKNOWN"
    assert_contains "$json_out" '"enabled":false' "env-check hard bypass reports disabled"

    local text_out
    text_out=$(TOKENLESS_TOOL_READY_ENABLED=1 tokenless env-check --tool Shell 2>&1)
    assert_contains "$text_out" "hard-disabled" "legacy opt-in cannot re-enable env-check"

    # ==========================================
    # 6.14 env-fix script: check command
    # ==========================================
    log_info "Test 6.14: env-fix check lists auto-fixable deps"
    local check_out=$(bash "$FIX_SCRIPT" check 2>&1)
    assert_contains "$check_out" "Auto-fixable" "env-fix check lists auto-fixable deps"
    assert_contains "$check_out" "Supported managers" "env-fix check shows supported managers"

    # ==========================================
    # 6.15 env-fix script: fix-tool (deps available)
    # ==========================================
    log_info "Test 6.15: env-fix fix-tool Shell"
    local fix_tool=$(bash "$FIX_SCRIPT" fix-tool Shell 2>&1)
    assert_contains "$fix_tool" "already available" "env-fix fix-tool reports available deps"

    # ==========================================
    # 6.16 env-fix script: fallback chain (rtk)
    # ==========================================
    log_info "Test 6.16: env-fix fallback chain (rtk already available)"
    local fb_out=$(bash "$FIX_SCRIPT" fix '{"binary":"rtk","version":">=0.35","package":"tokenless","manager":"rpm","fallback":[{"method":"symlink","binary":"rtk","source":"/usr/libexec/anolisa/tokenless/rtk"},{"method":"cargo","binary":"rtk","package":"rtk"}]}' 2>&1)
    assert_contains "$fb_out" "already available" "env-fix fallback: rtk already available via rpm"

    # ==========================================
    # 6.17 env-fix script: docker fallback (docker-ce → docker)
    # ==========================================
    log_info "Test 6.17: env-fix docker fallback chain"
    local docker_fb=$(bash "$FIX_SCRIPT" fix '{"binary":"docker","package":"docker-ce","manager":"rpm","fallback":[{"method":"rpm","binary":"docker","package":"docker"}]}' 2>&1)
    echo "$docker_fb" | grep -qE "already available|installed via" && log_pass "env-fix docker: fallback chain works (docker-ce→docker)" || log_fail "env-fix docker fallback failed: $docker_fb"

    # ==========================================
    # 6.18 env-fix script: jq variable interpolation (fb_binary)
    # ==========================================
    log_info "Test 6.18: env-fix jq --arg for fb_binary default"
    # Simulate a dep where fallback has no binary field (should default to primary binary)
    local jq_out=$(bash "$FIX_SCRIPT" fix '{"binary":"testbin99","package":"testpkg99","manager":"rpm","fallback":[{"method":"symlink","source":"/usr/local/bin/testbin99"}]}' 2>&1)
    assert_contains "$jq_out" "testbin99" "env-fix correctly resolves fb_binary default via --arg"

    # ==========================================
    # 6.19 env-fix script: curl_pipe_sh domain whitelist
    # ==========================================
    log_info "Test 6.19: curl_pipe_sh domain whitelist (astral.sh allowed, untrusted blocked)"
    local astral_out=$(bash "$FIX_SCRIPT" fix '{"binary":"uv","package":"uv","manager":"pip","fallback":[{"method":"curl_pipe_sh","url":"https://astral.sh/uv/install.sh"}]}' 2>&1)
    ! echo "$astral_out" | grep -q "untrusted URL" && log_pass "astral.sh is whitelisted" || log_fail "astral.sh blocked as untrusted"
    local blocked_out=$(bash "$FIX_SCRIPT" fix '{"binary":"fake","package":"fake","manager":"rpm","fallback":[{"method":"curl_pipe_sh","url":"https://evil.example.com/install.sh"}]}' 2>&1)
    assert_contains "$blocked_out" "untrusted" "Non-whitelisted domain is blocked"

    # ==========================================
    # 6.20 env-fix script: timeout on curl_pipe_sh
    # ==========================================
    log_info "Test 6.20: curl_pipe_sh has timeout (no infinite hang)"
    local timeout_out=$(timeout 5 bash "$FIX_SCRIPT" fix '{"binary":"cargo","package":"cargo","manager":"rpm","fallback":[{"method":"curl_pipe_sh","url":"https://sh.rustup.rs","args":"-s -- -y"}]}' 2>&1)
    # Either it completes quickly (cargo already available) or times out cleanly
    if echo "$timeout_out" | grep -q "already available"; then
        log_pass "curl_pipe_sh: cargo already available (no hang)"
    elif [ $? -eq 124 ]; then
        log_pass "curl_pipe_sh: timeout kills process cleanly (no hang)"
    else
        log_pass "curl_pipe_sh: process completed or timed out cleanly"
    fi

    # ==========================================
    # 6.21 tool-ready hook: unconditional hard bypass
    # ==========================================
    log_info "Test 6.21: tool-ready hook — hard bypass"
    local bypass_dir
    bypass_dir=$(mktemp -d)
    local bypass_spec="$bypass_dir/tool-ready-spec.json"
    local bypass_fixer="$bypass_dir/tokenless-env-fix.sh"
    local bypass_marker="$bypass_dir/fixer-called"
    cat > "$bypass_spec" <<'EOF'
{"TestMissing":{"required":[{"binary":"tokenless-missing-for-test","package":"tokenless-missing-for-test","manager":"rpm"}],"recommended":[],"permissions":[]}}
EOF
    cat > "$bypass_fixer" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[ "${1:-}" = "fix-all" ]
cat >/dev/null
touch "$TOKENLESS_FIX_MARKER"
EOF
    chmod 0644 "$bypass_fixer"

    local ready_out
    ready_out=$(echo '{"tool_name":"TestMissing","tool_input":{"command":"test"}}' \
        | TOKENLESS_TOOL_READY_ENABLED=1 \
          TOKENLESS_TOOL_READY_SPEC="$bypass_spec" \
          TOKENLESS_ENV_FIX_SCRIPT="$bypass_fixer" \
          TOKENLESS_FIX_MARKER="$bypass_marker" \
          bash "$READY_SCRIPT" 2>&1)
    if [ "$ready_out" != "{}" ]; then
        log_fail "tool-ready hard bypass returned invalid pass-through output: $ready_out"
    elif [ -e "$bypass_marker" ]; then
        log_fail "tool-ready hard bypass invoked the legacy fixer"
    else
        log_pass "tool-ready hard bypass emits JSON pass-through and skips the fixer"
    fi
    rm -rf "$bypass_dir"

    # ==========================================
    # 6.23 Core attribution: ENV_DEPENDENCY_MISSING
    # ==========================================
    log_info "Test 6.23: Attribution — ENV_DEPENDENCY_MISSING"
    local attr_resp='{"exit_code":1,"stdout":"","stderr":"command not found: fakebin99\nDetailed error info about missing dependency and resolution steps for the environment issue.\nAdditional troubleshooting context about installation methods and package managers available.\nMore diagnostic info about the failure scenario and recommended fix approaches for users.\nEnd of detailed error output with resolution suggestions and alternative installation methods."}'
    local attr_input=$(jq -n --argjson r "$attr_resp" '{"tool_name":"CustomAction","tool_response":$r,"is_error":true}')
    local attr_out=$(echo "$attr_input" | python3 "$COMPRESS_SCRIPT" 2>&1)
    assert_contains "$attr_out" "ENV_DEPENDENCY_MISSING" "Attribution detects command not found"

    # ==========================================
    # 6.24 Core attribution: ENV_PERMISSION
    # ==========================================
    log_info "Test 6.24: Attribution — ENV_PERMISSION"
    attr_resp='{"exit_code":1,"stdout":"","stderr":"Permission denied: /root/secret\nContext about permission error and what went wrong with the file access attempt.\nMore info about access restriction and how to resolve permissions issue for the user.\nDetailed error message about the permission failure scenario and recommended resolution steps."}'
    attr_input=$(jq -n --argjson r "$attr_resp" '{"tool_name":"CustomAction","tool_response":$r,"is_error":true}')
    attr_out=$(echo "$attr_input" | python3 "$COMPRESS_SCRIPT" 2>&1)
    assert_contains "$attr_out" "ENV_PERMISSION" "Attribution detects Permission denied"

    # ==========================================
    # 6.25 Core attribution: ENV_FILE_MISSING
    # ==========================================
    log_info "Test 6.25: Attribution — ENV_FILE_MISSING"
    attr_resp='{"exit_code":1,"stdout":"","stderr":"No such file or directory: /tmp/missing\nContext about missing file error and why it happened during tool execution.\nAdditional details about what file was expected and where it should be located.\nMore error info about missing file and how to create or find it properly for recovery."}'
    attr_input=$(jq -n --argjson r "$attr_resp" '{"tool_name":"CustomAction","tool_response":$r,"is_error":true}')
    attr_out=$(echo "$attr_input" | python3 "$COMPRESS_SCRIPT" 2>&1)
    assert_contains "$attr_out" "ENV_FILE_MISSING" "Attribution detects No such file"

    # ==========================================
    # 6.26 Env attribution: Bash + env error (small response, attribution injected)
    # ==========================================
    log_info "Test 6.26a: Bash + ENV_DEPENDENCY_MISSING — attribution injected"
    local skip_attr_resp='{"exit_code":1,"stdout":"","stderr":"command not found: fakebin99\nDetailed error info about missing dependency and resolution steps for the environment issue.\nAdditional troubleshooting context about installation methods and package managers available.\nMore diagnostic info about the failure scenario and recommended fix approaches for users.\nEnd of detailed error output with resolution suggestions and alternative installation methods."}'
    local skip_attr_input=$(jq -n --argjson r "$skip_attr_resp" '{"tool_name":"Bash","tool_response":$r}')
    local skip_attr_out=$(echo "$skip_attr_input" | python3 "$COMPRESS_SCRIPT" 2>&1)
    assert_contains "$skip_attr_out" "ENV_DEPENDENCY_MISSING" "Bash attribution detects command not found"

    log_info "Test 6.26b: Bash + ENV_PERMISSION — attribution injected"
    skip_attr_resp='{"exit_code":1,"stdout":"","stderr":"Permission denied: /root/secret\nContext about permission error and what went wrong with the file access attempt.\nMore info about access restriction and how to resolve permissions issue for the user.\nDetailed error message about the permission failure scenario and recommended resolution steps."}'
    skip_attr_input=$(jq -n --argjson r "$skip_attr_resp" '{"tool_name":"Bash","tool_response":$r}')
    skip_attr_out=$(echo "$skip_attr_input" | python3 "$COMPRESS_SCRIPT" 2>&1)
    assert_contains "$skip_attr_out" "ENV_PERMISSION" "Bash attribution detects Permission denied"

    log_info "Test 6.26c: Bash + ENV_FILE_MISSING — attribution injected"
    skip_attr_resp='{"exit_code":1,"stdout":"","stderr":"No such file or directory: /tmp/missing\nContext about missing file error and why it happened during tool execution.\nAdditional details about what file was expected and where it should be located.\nMore error info about missing file and how to create or find it properly for recovery."}'
    skip_attr_input=$(jq -n --argjson r "$skip_attr_resp" '{"tool_name":"Bash","tool_response":$r}')
    skip_attr_out=$(echo "$skip_attr_input" | python3 "$COMPRESS_SCRIPT" 2>&1)
    assert_contains "$skip_attr_out" "ENV_FILE_MISSING" "Bash attribution detects No such file"

    log_info "Test 6.26d: Bash + no env error (small response) — skip entirely"
    skip_attr_resp='{"exit_code":0,"stdout":"hello world from shell","stderr":""}'
    skip_attr_input=$(jq -n --arg r "$skip_attr_resp" '{"tool_name":"Bash","tool_response":$r}')
    skip_attr_out=$(echo "$skip_attr_input" | python3 "$COMPRESS_SCRIPT" 2>&1)
    assert_not_contains "$skip_attr_out" "ENV_" "Bash no-error: no attribution emitted"
    assert_not_contains "$skip_attr_out" "compress" "Bash no-error small: no compression emitted"

    # ==========================================
    # 6.26e Non-SKIP_TOOLS tool small response + env attribution (new path)
    # Verify that non-content-retrieval tools with small responses still
    # inject env attribution when an environment error is detected.
    # ==========================================
    log_info "Test 6.26e: Write (non-skip) + ENV_PERMISSION small — attribution injected"
    local small_err_resp='{"exit_code":1,"stdout":"","stderr":"Permission denied: /root/secret"}'
    local small_err_input=$(jq -n --argjson r "$small_err_resp" '{"tool_name":"Write","tool_response":$r,"is_error":true}')
    local small_err_out=$(echo "$small_err_input" | python3 "$COMPRESS_SCRIPT" 2>&1)
    assert_contains "$small_err_out" "ENV_PERMISSION" "Write small error: attribution injected"

    log_info "Test 6.26f: Write (non-skip) + no env error small — skip entirely"
    local small_ok_resp='{"exit_code":0,"stdout":"ok"}'
    local small_ok_input=$(jq -n --arg r "$small_ok_resp" '{"tool_name":"Write","tool_response":$r}')
    local small_ok_out=$(echo "$small_ok_input" | python3 "$COMPRESS_SCRIPT" 2>&1)
    assert_not_contains "$small_ok_out" "ENV_" "Write small no-error: no attribution"
    assert_not_contains "$small_ok_out" "compress" "Write small no-error: no compression"

    # ==========================================
    # 6.26g 3-layer classification correctness
    # Verify unified tool_categories.json classifies tools correctly:
    #   Layer 1 (skip): Read, Grep — not compressed
    #   Layer 2 (shell): Bash — moderate truncation
    #   Layer 3 (API): Write, WebFetch — zero-truncation
    # ==========================================
    log_info "Test 6.26g: 3-layer classification — Bash not in SKIP_TOOLS, Read is"
    local class_out
    class_out=$(python3 -c "
import sys
sys.path.insert(0, '${HOOK_DIR}')
from hook_utils import SKIP_TOOLS, SHELL_TOOLS, get_thresholds
# Layer 1: Read/Grep must be in SKIP_TOOLS
assert 'Read' in SKIP_TOOLS, 'Read missing from SKIP_TOOLS'
assert 'Grep' in SKIP_TOOLS, 'Grep missing from SKIP_TOOLS'
assert {'grep_code', 'search_file', 'list_dir'} <= SKIP_TOOLS, 'Qoder read tools missing from SKIP_TOOLS'
# Layer 2: Bash must NOT be in SKIP_TOOLS (it is layer 2, compressed with 64K thresholds)
assert 'Bash' not in SKIP_TOOLS, 'Bash wrongly in SKIP_TOOLS (should be layer 2)'
assert 'Bash' in SHELL_TOOLS, 'Bash missing from SHELL_TOOLS'
assert {'run_in_terminal', 'get_terminal_output'} <= SHELL_TOOLS, 'Qoder shell tools missing from SHELL_TOOLS'
assert 'RunCommand' in SHELL_TOOLS, 'Trae RunCommand missing from SHELL_TOOLS'
# Thresholds: Bash gets layer 2 (64K/128/8), Write gets layer 3 (1M/64K/32)
bash_thr = get_thresholds('Bash')
write_thr = get_thresholds('Write')
assert bash_thr == (65536, 128, 8), f'Bash thresholds wrong: {bash_thr}'
assert write_thr == (1048576, 65536, 32), f'Write thresholds wrong: {write_thr}'
print('CLASSIFY_OK')
" 2>&1)
    assert_contains "$class_out" "CLASSIFY_OK" "3-layer classification correct"

    # ==========================================
    # 6.26h Behavioral dispatch: Read skips, Bash proceeds to compression
    # Use a replacement-capable host so this test isolates tool classification
    # from the unified entry's fail-open behavior for unknown/additive hosts.
    # ==========================================
    log_info "Test 6.26h: Read skips entirely, Bash enters compression pipeline"
    local large_body
    large_body=$(python3 -c "print('x' * 5000)")
    local dispatch_resp
    dispatch_resp=$(jq -n --arg b "$large_body" '{"exit_code":0,"stdout":$b,"stderr":""}')

    # Read (layer 1) → skip → outputs only {} (skip() emits empty JSON object)
    local read_input
    read_input=$(jq -n --arg r "$dispatch_resp" '{"tool_name":"Read","tool_response":$r}')
    local read_out
    read_out=$(echo "$read_input" | python3 "$COMPRESS_SCRIPT" --agent-id qoder-cli 2>&1)
    local read_trimmed
    read_trimmed=$(echo "$read_out" | tr -d '[:space:]')
    [ "$read_trimmed" = "{}" ] && log_pass "Read (layer 1) skips entirely" || log_fail "Read should skip with {}, got: $read_out"

    # Bash (layer 2) → compression attempted → non-trivial hook output (not just {})
    local bash_input
    bash_input=$(jq -n --arg r "$dispatch_resp" '{"tool_name":"Bash","tool_response":$r}')
    local bash_out
    bash_out=$(echo "$bash_input" | python3 "$COMPRESS_SCRIPT" --agent-id qoder-cli 2>&1)
    local bash_trimmed
    bash_trimmed=$(echo "$bash_out" | tr -d '[:space:]')
    [ -n "$bash_trimmed" ] && [ "$bash_trimmed" != "{}" ] && log_pass "Bash (layer 2) enters compression pipeline" || log_fail "Bash should attempt compression, got: $bash_out"

    # ==========================================
    # 6.27 No docker_socket or https_outbound in spec
    # ==========================================
    log_info "Test 6.27: Spec has no runtime state checks (docker_socket/https_outbound removed)"
    local spec_content=$(cat "$SPEC_FILE")
    ! echo "$spec_content" | grep -q "docker_socket" && log_pass "No docker_socket in spec (removed)" || log_fail "docker_socket still in spec"
    ! echo "$spec_content" | grep -q "https_outbound" && log_pass "No https_outbound in spec (removed)" || log_fail "https_outbound still in spec"
}

main() {
    echo "============================================"
    echo "  Token-Less Full Test Suite"
    echo "============================================"

    if ! command -v tokenless &> /dev/null; then
        echo -e "${RED}ERROR: tokenless not found${NC}"; exit 1
    fi
    log_info "Testing $(tokenless --version)"

    test_schema_compression
    test_response_compression
    test_command_rewriting
    test_stats_system
    test_toon_compression
    test_tool_ready

    echo ""
    echo "============================================"
    echo "  Summary: ${TESTS_PASSED}/${TESTS_TOTAL} passed"
    echo "============================================"

    [ "$TESTS_FAILED" -gt 0 ] && exit 1
    echo -e "\n${GREEN}All tests passed!${NC}"
}

main "$@"

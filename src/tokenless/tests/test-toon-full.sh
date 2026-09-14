#!/usr/bin/env bash
# 全量 TOON 功能验证测试
# 覆盖三种应用场景：Tokenless CLI、Cosh-NG、OpenClaw
#
# 手动执行（不是 make 目标）：
#
#   PATH="src/tokenless/target/debug:$PATH" bash src/tokenless/tests/test-toon-full.sh
#
# 前置条件：
#   必需    PATH 上的 tokenless 与本 checkout 同版本（TOKENLESS_ALLOW_VERSION_SKEW=1 放行）、jq、python3
#   场景 2  common hooks 目录，默认取仓库树，可用 TOKENLESS_HOOK_DIR 覆盖
#   场景 3  已安装并启用 OpenClaw tokenless 插件，且显式设置 TOKENLESS_TOON_FULL_LIVE=1
#           （3.x 会真实调用模型，既慢又花额度，默认不跑）
#
# 可选前置条件缺失记为 SKIP 而不是 FAIL：本脚本要在开发机、CI 容器和装好的宿主机上
# 都能给出可信结论；恒红的用例只会被忽略，最后连同它覆盖的契约一起烂掉。

set -uo pipefail

TEST_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TOKENLESS_SOURCE_DIR="$(cd "$TEST_SCRIPT_DIR/.." && pwd)"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

PASS=0
FAIL=0
SKIP=0
TOTAL=0
SCENARIOS=0

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

pass() { echo -e "${GREEN}[PASS]${NC} $1"; ((PASS++)); ((TOTAL++)); }
fail() { echo -e "${RED}[FAIL]${NC} $1"; ((FAIL++)); ((TOTAL++)); }
skip() { echo -e "${YELLOW}[SKIP]${NC} $1"; ((SKIP++)); }
info() { echo -e "${BLUE}[INFO]${NC} $1"; }
section() { echo -e "\n${YELLOW}========== $1 ==========${NC}\n"; ((SCENARIOS++)); }
scenario() { echo -e "\n${CYAN}▸ $1${NC}"; }

assert_contains() {
    local input="$1" expected="$2" test_name="$3"
    if echo "$input" | grep -qF "$expected"; then pass "$test_name"
    else fail "$test_name - expected to contain: '$expected'"; fi
}

assert_not_empty() {
    local input="$1" test_name="$2"
    if [ -n "$input" ]; then pass "$test_name"
    else fail "$test_name - empty output"; fi
}

assert_same() {
    local actual="$1" expected="$2" test_name="$3"
    if [ "$actual" = "$expected" ]; then pass "$test_name"
    else fail "$test_name - expected '$expected', got '$actual'"; fi
}

# ========== 前置条件 ==========

# 本脚本驱动 PATH 上的已安装二进制，断言却跟随 checkout。版本错位时失败信息会指向
# 错误的文件（0.7.x 连 compress-toon --min-toon-chars 都没有），所以先比一次版本，
# 把原因说清楚，而不是丢一堆无关失败给人猜。
require_matching_cli() {
    local cmd missing=0
    for cmd in tokenless jq python3; do
        if ! command -v "$cmd" >/dev/null 2>&1; then
            echo -e "${RED}ERROR: $cmd 未安装${NC}"
            missing=1
        fi
    done
    [ "$missing" -eq 0 ] || exit 1

    local cli_version workspace_version
    cli_version=$(tokenless --version 2>/dev/null | awk '{print $2}')
    workspace_version=$(sed -n 's/^version = "\(.*\)"$/\1/p' \
        "$TOKENLESS_SOURCE_DIR/Cargo.toml" | head -1)
    if [ -n "$workspace_version" ] && [ "$cli_version" != "$workspace_version" ]; then
        if [ "${TOKENLESS_ALLOW_VERSION_SKEW:-0}" = "1" ]; then
            info "WARNING: PATH 上的 tokenless 是 ${cli_version:-unknown}，本 checkout 是 $workspace_version（TOKENLESS_ALLOW_VERSION_SKEW=1）"
            return 0
        fi
        echo -e "${RED}ERROR: PATH 上的 tokenless 是 ${cli_version:-unknown}，但本 checkout 是 $workspace_version${NC}"
        echo "断言跟随 checkout，被测二进制却不跟随，版本错位会报出一堆无关失败。"
        echo "把当前构建放到 PATH 前面（或安装后再跑）："
        echo ""
        echo "    PATH=\"src/tokenless/target/debug:\$PATH\" bash src/tokenless/tests/test-toon-full.sh"
        echo "    make -C src/tokenless build && make -C src/tokenless install"
        echo ""
        echo "确实要测已安装的 ${cli_version:-unknown}，设置 TOKENLESS_ALLOW_VERSION_SKEW=1。"
        exit 1
    fi
}

# 场景 2 用哪一份 hooks：默认取仓库树（测的就是本 checkout 的实现），其次已安装副本。
# TOKENLESS_HOOK_DIR 一旦显式指定就只用它 —— 指错了要当场说出来，不能悄悄退回仓库树，
# 否则"我测的是装好的那份"会变成一句假话。
resolve_hook_dir() {
    local candidate
    HOOK_DIR=""
    HOOK_DIR_DETAIL=""
    if [ -n "${TOKENLESS_HOOK_DIR:-}" ]; then
        if [ -f "$TOKENLESS_HOOK_DIR/compress_response_hook.py" ]; then
            HOOK_DIR="$TOKENLESS_HOOK_DIR"
            return 0
        fi
        HOOK_DIR_DETAIL="TOKENLESS_HOOK_DIR=$TOKENLESS_HOOK_DIR 下没有 compress_response_hook.py"
        return 1
    fi
    for candidate in \
        "$TOKENLESS_SOURCE_DIR/adapters/tokenless/common/hooks" \
        "/usr/share/anolisa/adapters/tokenless/common/hooks" \
        "$HOME/.local/share/anolisa/adapters/tokenless/common/hooks"; do
        if [ -f "$candidate/compress_response_hook.py" ]; then
            HOOK_DIR="$candidate"
            return 0
        fi
    done
    HOOK_DIR_DETAIL="仓库树与已安装路径下都没有 compress_response_hook.py"
    return 1
}

# 场景 3 需要一个装好并启用了 tokenless 插件的 OpenClaw。
probe_openclaw() {
    OPENCLAW_STATE="absent"
    if ! command -v openclaw >/dev/null 2>&1; then
        OPENCLAW_DETAIL="openclaw 未安装"
        return 1
    fi
    OPENCLAW_STATE="installed"
    local plugin_file="$HOME/.openclaw/extensions/tokenless/index.js"
    if [ ! -f "$plugin_file" ]; then
        OPENCLAW_DETAIL="插件文件缺失（$plugin_file）"
        return 1
    fi
    local reason
    reason=$(python3 - <<'PYCHECK' 2>/dev/null
import json, os
try:
    cfg = json.load(open(os.path.expanduser('~/.openclaw/openclaw.json')))
except Exception as exc:
    print('openclaw.json 读取失败: %s' % exc)
    raise SystemExit
entries = cfg.get('plugins', {}).get('entries', {})
entry = entries.get('tokenless')
if entry is None:
    print('openclaw.json 没有 plugins.entries.tokenless（现有条目: %s）'
          % (', '.join(sorted(entries)) or '无'))
elif not entry.get('enabled'):
    print('plugins.entries.tokenless.enabled 为 false')
elif not entry.get('config', {}).get('post_tool_enabled', True):
    print('plugins.entries.tokenless.config.post_tool_enabled 为 false')
PYCHECK
)
    if [ -n "$reason" ]; then
        OPENCLAW_STATE="disabled"
        OPENCLAW_DETAIL="$reason"
        return 1
    fi
    OPENCLAW_STATE="ready"
    OPENCLAW_DETAIL="插件已启用且 PostTool 配置正确"
    return 0
}

require_matching_cli
resolve_hook_dir || true
probe_openclaw || true

# ========== 环境检查 ==========
section "环境检查"

info "tokenless $(tokenless --version 2>/dev/null | awk '{print $2}') @ $(command -v tokenless)"
pass "tokenless 可用且与本 checkout 同版本"
pass "jq 可用（$(jq --version 2>/dev/null)）"

if [ -n "$HOOK_DIR" ]; then
    pass "common hooks 目录可用（$HOOK_DIR）"
else
    skip "common hooks 目录不可用：$HOOK_DIR_DETAIL — 场景 2 将跳过"
fi

if [ "$OPENCLAW_STATE" = "ready" ]; then
    pass "OpenClaw $OPENCLAW_DETAIL"
    if [ "${TOKENLESS_TOON_FULL_LIVE:-0}" = "1" ]; then
        info "TOKENLESS_TOON_FULL_LIVE=1 — 场景 3 会真实调用模型"
    else
        skip "未设置 TOKENLESS_TOON_FULL_LIVE=1 — 场景 3 的真实模型调用不执行"
    fi
else
    skip "OpenClaw 未就绪：$OPENCLAW_DETAIL — 场景 3 将跳过"
fi

# ========== 场景 1: Tokenless CLI ==========
section "场景 1: Tokenless CLI"

# compress-toon 默认有 500 字符门槛（MIN_TOON_CHARS，与 Hook 层一致），短负载逐字节
# 透传；另外即使关掉门槛，估算 token 没有下降的负载同样透传。所以下面的小样本一律
# 显式带 --min-toon-chars 0，断言才真的打在编码器上；门槛与透传契约本身由 1.8 覆盖。
TOON_FORCE=(--min-toon-chars 0)

scenario "1.1 基础编码/解码"

# 简单对象
simple='{"name":"Alice","age":30,"active":true}'
result=$(printf '%s' "$simple" | tokenless compress-toon "${TOON_FORCE[@]}" 2>/dev/null)
assert_not_empty "$result" "简单对象编码"
assert_contains "$result" "name: Alice" "简单对象 - name"
assert_contains "$result" "age: 30" "简单对象 - age"

# 解码往返
roundtrip=$(printf '%s' "$result" | tokenless decompress-toon 2>/dev/null)
assert_not_empty "$roundtrip" "简单对象解码"
if printf '%s' "$roundtrip" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d['name']=='Alice' and d['age']==30" 2>/dev/null; then
    pass "往返转换数据一致"
else
    fail "往返转换数据不一致"
fi

scenario "1.2 表格数据压缩"

json='{"users":[{"id":1,"name":"Alice","email":"alice@example.com","role":"admin"},{"id":2,"name":"Bob","email":"bob@example.com","role":"user"},{"id":3,"name":"Charlie","email":"charlie@example.com","role":"moderator"},{"id":4,"name":"Diana","email":"diana@example.com","role":"admin"},{"id":5,"name":"Eve","email":"eve@example.com","role":"user"}]}'
toon_out=$(printf '%s' "$json" | tokenless compress-toon "${TOON_FORCE[@]}" 2>/dev/null)
json_len=${#json}
toon_len=${#toon_out}
savings=$(( (json_len - toon_len) * 100 / json_len ))
info "  JSON: $json_len chars → TOON: $toon_len chars (${savings}% 压缩率)"
if [ "$savings" -ge 15 ]; then
    pass "表格数据压缩率 >= 15%"
else
    fail "表格数据压缩率 < 15% (${savings}%)"
fi
assert_contains "$toon_out" "users[5]" "表格数组头部正确"

scenario "1.3 深度嵌套数据"

nested='{"data":{"users":[{"id":1,"profile":{"name":"Alice","age":30,"address":{"city":"Beijing","country":"CN"}}},{"id":2,"profile":{"name":"Bob","age":25,"address":{"city":"Shanghai","country":"CN"}}}],"meta":{"total":2,"page":1,"hasNext":false}}}'
toon_out=$(printf '%s' "$nested" | tokenless compress-toon "${TOON_FORCE[@]}" 2>/dev/null)
json_len=${#nested}
toon_len=${#toon_out}
info "  JSON: $json_len chars → TOON: $toon_len chars"
assert_not_empty "$toon_out" "深度嵌套数据编码完成"
if [ "$toon_out" = "$nested" ]; then
    # 非表格结构估不出 token 收益 → 按契约逐字节透传，此时不能再喂给
    # decompress-toon（它只接受 TOON 输入），往返由 1.7 的透传分支覆盖。
    pass "深度嵌套无 token 收益 → 逐字节透传（非表格结构不保证压缩）"
else
    nested_rt=$(printf '%s' "$toon_out" | tokenless decompress-toon 2>/dev/null)
    assert_contains "$nested_rt" '"city"' "深度嵌套往返解码"
fi

scenario "1.4 大体积 JSON 压缩"

# 生成一个较大的 JSON（模拟 API 响应），体积超过默认门槛，走的是默认参数路径
python3 -c "
import json, sys
data = {
    'results': [{'id': i, 'name': f'Item_{i}', 'value': i * 3.14, 'active': i % 2 == 0, 'tags': [f'tag_{j}' for j in range(5)]} for i in range(50)],
    'meta': {'total': 50, 'page': 1, 'per_page': 50},
    'debug_info': {'query_time': 0.123, 'cache_hit': False}
}
json.dump(data, sys.stdout)
" > "$TMP_DIR/large_test.json"

large_json=$(cat "$TMP_DIR/large_test.json")
large_json_len=${#large_json}
toon_out=$(printf '%s' "$large_json" | tokenless compress-toon 2>/dev/null)
toon_len=${#toon_out}
savings=$(( (large_json_len - toon_len) * 100 / large_json_len ))
info "  JSON: $large_json_len chars → TOON: $toon_len chars (${savings}% 压缩率)"
if [ "$savings" -ge 10 ]; then
    pass "大体积 JSON 压缩率 >= 10%"
else
    fail "大体积 JSON 压缩率 < 10% (${savings}%)"
fi
assert_contains "$toon_out" "results[50]" "大体积 JSON 表格头部正确"

scenario "1.5 特殊类型处理"

# 布尔值
result=$(printf '%s' '{"t":true,"f":false}' | tokenless compress-toon "${TOON_FORCE[@]}" 2>/dev/null)
assert_contains "$result" "t: true" "true 编码"
assert_contains "$result" "f: false" "false 编码"

# Null（单键样本估不出 token 收益会透传，加一个键让编码器真正跑起来）
result=$(printf '%s' '{"val":null,"keep":1}' | tokenless compress-toon "${TOON_FORCE[@]}" 2>/dev/null)
assert_contains "$result" "val: null" "null 编码"

# 浮点数
result=$(printf '%s' '{"pi":3.14159,"neg":-42}' | tokenless compress-toon "${TOON_FORCE[@]}" 2>/dev/null)
assert_contains "$result" "pi: 3.14159" "浮点数编码"
assert_contains "$result" "neg: -42" "负数编码"

# 空数组
result=$(printf '%s' '{"items":[],"keep":1}' | tokenless compress-toon "${TOON_FORCE[@]}" 2>/dev/null)
assert_contains "$result" "items[0]" "空数组编码"

scenario "1.6 文件输入/输出"

echo '{"from":"file","value":42}' > "$TMP_DIR/toon_file_test.json"
result=$(tokenless compress-toon -f "$TMP_DIR/toon_file_test.json" "${TOON_FORCE[@]}" 2>/dev/null)
assert_contains "$result" "from: file" "文件输入编码"

tokenless compress-toon -f "$TMP_DIR/toon_file_test.json" "${TOON_FORCE[@]}" > "$TMP_DIR/toon_file_output.toon" 2>/dev/null
result=$(cat "$TMP_DIR/toon_file_output.toon" 2>/dev/null)
assert_contains "$result" "from: file" "文件输出编码"

scenario "1.7 往返转换完整性"

if python3 -c "
import json, subprocess, sys

test_cases = [
    {'name': 'Alice', 'age': 30, 'active': True},
    {'users': [{'id': 1, 'name': 'Alice'}, {'id': 2, 'name': 'Bob'}]},
    {'data': {'users': [{'id': 1, 'name': 'test', 'tags': ['a', 'b']}], 'count': 1, 'active': True, 'meta': None}},
    {'a': {'b': {'c': {'d': {'e': 'deep'}}}}},
    # Note: TOON normalizes integer-valued floats (3.0 -> 3), so use 3.14
    {'mixed': [1, 'two', 3.14, True, None, [4, 5]]}
]

all_passed = True
for i, case in enumerate(test_cases):
    original = json.dumps(case, sort_keys=True)
    # Encode. --min-toon-chars 0 keeps the 500-character gate out of the way so
    # every fixture actually reaches the encoder.
    p1 = subprocess.run(['tokenless', 'compress-toon', '--min-toon-chars', '0'],
                        input=original, capture_output=True, text=True)
    toon_out = p1.stdout.strip()
    if p1.returncode != 0 or not toon_out:
        print(f'Case {i} ENCODE FAILED (exit {p1.returncode}): {p1.stderr.strip()}', file=sys.stderr)
        all_passed = False
        continue
    # compress-toon falls back to the original JSON when TOON offers no
    # savings; that passthrough still round-trips by definition.
    try:
        roundtrip = json.loads(toon_out)
    except ValueError:
        p2 = subprocess.run(['tokenless', 'decompress-toon'], input=toon_out, capture_output=True, text=True)
        if p2.returncode != 0 or not p2.stdout.strip():
            print(f'Case {i} DECODE FAILED (exit {p2.returncode}): {p2.stderr.strip()}', file=sys.stderr)
            all_passed = False
            continue
        roundtrip = json.loads(p2.stdout)
    roundtrip_json = json.dumps(roundtrip, sort_keys=True)
    if original != roundtrip_json:
        print(f'Case {i} MISMATCH: {original} vs {roundtrip_json}', file=sys.stderr)
        all_passed = False

sys.exit(0 if all_passed else 1)
" 2>&1; then
    pass "5 种数据结构往返转换全部一致"
else
    fail "往返转换存在数据不一致"
fi

scenario "1.8 默认门槛与无收益透传契约"

# 短于 500 字符：exit 0 且 stdout 与输入逐字节相同（不加也不去末尾换行）
short='{"name":"Alice","age":30,"active":true}'
short_out=$(printf '%s' "$short" | tokenless compress-toon 2>/dev/null)
short_rc=$?
assert_same "$short_rc" "0" "短负载 exit 0"
assert_same "$short_out" "$short" "短负载（${#short} 字符 < 500）逐字节透传"

# 关掉门槛但估算 token 没有下降：同样透传
no_savings='{"val":null}'
no_savings_out=$(printf '%s' "$no_savings" | tokenless compress-toon "${TOON_FORCE[@]}" 2>/dev/null)
assert_same "$no_savings_out" "$no_savings" "无 token 收益时即使 --min-toon-chars 0 也透传"

# 超过门槛：默认参数下就会编码（样本沿用 1.4 生成的大 JSON）
long_json=$(cat "$TMP_DIR/large_test.json")
long_out=$(printf '%s' "$long_json" | tokenless compress-toon 2>/dev/null)
if [ "${#long_json}" -ge 500 ] && [ -n "$long_out" ] && [ "$long_out" != "$long_json" ]; then
    pass "长负载（${#long_json} 字符 >= 500）默认编码"
else
    fail "长负载默认未编码"
fi

# ========== 场景 2: Cosh-NG ==========
section "场景 2: Cosh-NG Hooks"

scenario "2.1 响应压缩 → TOON 流水线"

if [ -z "$HOOK_DIR" ]; then
    skip "common hooks 目录不可用（$HOOK_DIR_DETAIL），场景 2 跳过"
else
    payload=$(cat <<'EOF'
{
  "tool_name": "web_fetch",
  "tool_response": {
    "title": "Test API Response",
    "data": [
      {"id": 1, "name": "Item A", "price": 29.99, "in_stock": true, "category": "electronics"},
      {"id": 2, "name": "Item B", "price": 49.99, "in_stock": false, "category": "clothing"},
      {"id": 3, "name": "Item C", "price": 99.99, "in_stock": true, "category": "electronics"},
      {"id": 4, "name": "Item D", "price": 19.99, "in_stock": true, "category": "food"},
      {"id": 5, "name": "Item E", "price": 149.99, "in_stock": true, "category": "electronics"},
      {"id": 6, "name": "Item F", "price": 39.99, "in_stock": true, "category": "clothing"},
      {"id": 7, "name": "Item G", "price": 59.99, "in_stock": true, "category": "electronics"},
      {"id": 8, "name": "Item H", "price": 79.99, "in_stock": false, "category": "food"},
      {"id": 9, "name": "Item I", "price": 89.99, "in_stock": true, "category": "electronics"},
      {"id": 10, "name": "Item J", "price": 109.99, "in_stock": true, "category": "clothing"}
    ],
    "meta": {"total": 10, "page": 1, "has_next": false},
    "null_field": null,
    "empty_obj": {},
    "empty_arr": []
  }
}
EOF
)

    result=$(
        printf '%s' "$payload" |
            COSH_NG_VERSION=0.5.0 python3 "$HOOK_DIR/compress_response_hook.py" \
                --agent-id copilot-shell 2>/dev/null
    )
    assert_not_empty "$result" "Response→TOON 流水线输出"
    context=$(printf '%s' "$result" | jq -r '.hookSpecificOutput.updatedToolResponse')
    assert_contains "$context" "data[10]" "流水线产出 TOON 表格内容"
    if printf '%s' "$context" | grep -qE "\[tokenless\]|TOON format"; then
        fail "流水线 updatedToolResponse 仍包含已废弃的标签前缀"
    else
        pass "流水线 updatedToolResponse 已去除标签前缀"
    fi
    # 验证 JSON 清理移除空值字段
    if printf '%s' "$context" | grep -qE "null_field|empty_obj|empty_arr"; then
        fail "Response 压缩未移除空值字段"
    else
        pass "Response 压缩移除了空值字段"
    fi
fi

# ========== 场景 3: OpenClaw ==========
section "场景 3: OpenClaw Agent"

scenario "3.1 OpenClaw 插件状态验证"

if [ "$OPENCLAW_STATE" != "ready" ]; then
    skip "OpenClaw 未就绪，场景 3 全部跳过（原因见环境检查）"
elif [ "${TOKENLESS_TOON_FULL_LIVE:-0}" != "1" ]; then
    skip "场景 3 会真实调用模型，需显式设置 TOKENLESS_TOON_FULL_LIVE=1 才执行"
else
    # 获取最新 session ID
    SESSION_ID=$(timeout 60 openclaw sessions --json 2>/dev/null | python3 -c "
import json, sys
data = json.load(sys.stdin)
sessions = data.get('sessions', [])
# Find first active session
for s in sessions:
    print(s['sessionId'])
    break
" 2>/dev/null || echo "")

    if [ -z "$SESSION_ID" ]; then
        skip "拿不到 OpenClaw session ID（先用 openclaw 建立一个会话再跑）"
    else
        info "  使用 session: $SESSION_ID"

        # 检查插件 active features
        result=$(timeout 180 openclaw agent --session-id "$SESSION_ID" --message "ping" --timeout 60 2>&1 || true)
        if echo "$result" | grep -q "toon-compression"; then
            pass "OpenClaw 插件 TOON 压缩功能已激活"
        else
            fail "OpenClaw 插件 TOON 压缩功能未激活"
        fi

        # 检查所有 4 个功能
        for feature in rtk-rewrite schema-compression response-compression toon-compression; do
            if echo "$result" | grep -q "$feature"; then
                pass "功能已激活: $feature"
            else
                fail "功能未激活: $feature"
            fi
        done

        scenario "3.2 OpenClaw 实际调用 — 结构化数据 TOON 压缩"

        # 让 agent 执行返回结构化 JSON 数据的命令
        result=$(timeout 300 openclaw agent --session-id "$SESSION_ID" --message "请执行 'hostname' 命令，返回 JSON" --json --timeout 120 2>&1 || true)

        if echo "$result" | grep -q '"runId"'; then
            pass "OpenClaw agent 调用已执行"
        else
            fail "OpenClaw agent 调用未执行"
        fi

        # 验证插件日志输出
        if echo "$result" | grep -q "\[tokenless"; then
            pass "OpenClaw 插件日志输出正常"
        else
            info "  (OpenClaw 插件日志未在当前输出中显示)"
        fi

        scenario "3.3 OpenClaw 调用 — 命令重写 + 响应压缩/TOON 链路"

        result=$(timeout 300 openclaw agent --session-id "$SESSION_ID" --message "请执行 'ls /tmp' 命令，返回结果" --json --timeout 120 2>&1 || true)

        if echo "$result" | grep -q '"runId"'; then
            pass "多工具链路测试成功"
        else
            fail "多工具链路测试失败"
        fi
    fi
fi

# ========== 汇总 ==========
echo ""
echo "============================================"
echo -e "  测试汇总: ${GREEN}${PASS}/${TOTAL} 通过${NC}, ${RED}${FAIL} 失败${NC}, ${YELLOW}${SKIP} 跳过${NC}"
echo -e "  覆盖场景: ${SCENARIOS} 个"
echo "============================================"
echo ""
echo -e "  ${CYAN}场景 1: Tokenless CLI${NC} — 编码/解码/往返/大JSON/门槛与透传契约"
echo -e "  ${CYAN}场景 2: COSH Hooks${NC} — Response→TOON 流水线/标签前缀/空值清理"
echo -e "  ${CYAN}场景 3: OpenClaw${NC} — 插件状态/agent调用/多工具链路（需 TOKENLESS_TOON_FULL_LIVE=1）"
echo ""

[ "$FAIL" -gt 0 ] && exit 1
if [ "$SKIP" -gt 0 ]; then
    echo -e "${GREEN}已执行用例全部通过（${SKIP} 项因前置条件缺失跳过）${NC}"
else
    echo -e "${GREEN}所有测试通过！${NC}"
fi

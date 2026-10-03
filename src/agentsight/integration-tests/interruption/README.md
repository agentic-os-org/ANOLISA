# AgentSight 中断检测场景测试

对 AgentSight 的中断检测、分类、logtail 导出进行端到端验证。通过向 LLM API 发送构造好的请求（正常 / 错误），检查 AgentSight 是否正确识别中断类型并写入数据库和 logtail 文件。

## 部署形态说明

测试默认目标为 **sysak 集成部署**：

- 二进制路径：`/usr/local/sysak/.sysak_components/tools/agentsight`
- 运行方式：daemon（`agentsight trace --daemon`），**不依赖 systemd**
- 数据流：trace 模式直接通过 SLS logtail 上传，**无需 `agentsight serve`**（serve 主要用于 dashboard UI + HealthChecker 备份路径，本测试链路用不到）
- 配置文件：`/etc/agentsight/config.json`（生产环境通常已预置 OpenClaw/Hermes/Cosh 等规则）
- SLS 输出文件：`/var/sysom/ilog/agentsight`（由 `SLS_LOGTAIL_FILE` 环境变量指定，iLogtail 采集后上传 SLS）

## 前置条件

**远程机器上需要：**

1. AgentSight daemon 运行中：

   ```bash
   pgrep -af "agentsight trace"
   # 若未运行：
   SLS_LOGTAIL_FILE=/var/sysom/ilog/agentsight \
     /usr/local/sysak/.sysak_components/tools/agentsight trace --daemon
   ```

2. **配置 AgentSight 监控测试进程** — 编辑 `/etc/agentsight/config.json`，在 `cmdline.allow` 数组中**追加**以下规则（不要覆盖已有 OpenClaw / Hermes / Cosh 等规则）：

   ```json
   {"rule": ["*python3*"], "agent_name": "TestAgent"}
   ```

   说明：
   - `cmdline.allow` 规则按 argv 位置匹配。`["*python3*"]` 单元素规则只匹配 `argv[0]`，要求进程以 `python3` 或绝对路径 `/usr/bin/python3` 启动
   - 修改前先备份：`cp /etc/agentsight/config.json /etc/agentsight/config.json.bak`
   - 修改后需重启 daemon：

   ```bash
   pkill -9 -f "agentsight trace"
   sleep 2
   SLS_LOGTAIL_FILE=/var/sysom/ilog/agentsight \
     /usr/local/sysak/.sysak_components/tools/agentsight trace --daemon
   ```

3. **SSL probe attach 时序**：测试脚本进程启动后，AgentSight 通过 procmon eBPF tracepoint 发现进程 → 匹配 cmdline 规则 → attach SSL uprobe，全过程需 ~8-15s。**进程必须在发起首个 HTTPS 请求前 sleep 至少 10 秒**，否则 SSL 握手已完成、uprobe 无法捕获。`scenario_test.py` 已内置初始等待。

4. 一个有效的 dashscope API key（用于发起合法的对照请求）

5. 验证 SLS logtail 文件可写：

   ```bash
   ls -la /var/sysom/ilog/agentsight
   # 应该是常规文件，由 iLogtail 进程采集后上传 SLS
   ```

## 快速开始

```bash
# 1. 上传脚本到远程机器
scp integration-tests/interruption/scenario_test.py root@<HOST>:/tmp/

# 2. 跑一个场景
ssh root@<HOST> "python3 /tmp/scenario_test.py auth_single --api-key sk-your-key"

# 3. 跑所有场景
ssh root@<HOST> "python3 /tmp/scenario_test.py all --api-key sk-your-key"

# API key 也可以通过环境变量传入
ssh root@<HOST> "DASHSCOPE_API_KEY=sk-your-key python3 /tmp/scenario_test.py all"
```

## 场景说明

### auth_single — 单次认证错误

发送 1 个请求，使用无效 API key。

- 预期 HTTP 状态码：`401`
- 预期中断类型：`auth_error`（severity: high）
- 用途：验证 401 + `invalid_api_key` 关键字被正确分类为 `auth_error`

### auth_storm — 认证错误风暴

用同一个无效 key 快速发送 5 个请求（模拟重试风暴）。

- 预期：5 个 `auth_error` 中断
- 用途：验证同一根因的重复错误在健康分计算中受到 per-session penalty cap 限制（cap=10，等于 1 次 critical）

### mixed_light — 轻度混合

10 个请求：8 个正常 + 2 个认证错误。

- 预期：2 个 `auth_error`，8 个正常 LLMCall
- 用途：验证正常请求和错误请求混合时的检测准确性

### mixed_heavy — 重度混合

10 个请求：5 个正常 + 5 个认证错误（交替发送）。

- 预期：5 个 `auth_error`，5 个正常 LLMCall
- 用途：验证高错误率场景下的健康分计算

### multi_type — 多种错误类型

5 个请求：3 个正常 + 1 个认证错误 + 1 个不存在模型（404）。

- 预期：1 个 `auth_error` + 1 个 `llm_error`
- 用途：验证不同类型中断的正确分类

### healthy — 健康基线

10 个正常请求。

- 预期：0 个中断
- 用途：建立正常对话基线，验证无误报

### agent_crash — Agent 进程崩溃（非 OOM）

模拟 agent 进程在 SSE 流接收过程中被 SIGKILL 杀掉（例如手动 kill、systemd stop、segfault 等非 OOM 原因），验证 AgentSight 能捕获 crash 但不会误报为 OOM。

- 预期：1 个 `agent_crash`（severity: critical），detail 中 **不含** `"oom": true`
- 触发条件：trace 模式 procmon eBPF tracepoint 实时检测到进程退出 → 检查 dmesg 无 OOM 记录 → 标记普通 crash
- 等待时间：~1-2s（实时路径）
- 用途：验证进程异常退出的检测路径，且 OOM 归因不会误报

**手动验证方式（SIGKILL fake openclaw-gateway）：**

```bash
# 1. 准备模拟 agent 进程（symlink 到真实 node 让进程名匹配 OpenClaw 规则）
ln -sf $(which node) /tmp/openclaw-gateway

# 2. 编写测试脚本 /tmp/crash_agent_test.js
cat > /tmp/crash_agent_test.js << 'EOF'
const https = require('https');

setTimeout(() => {
    const data = JSON.stringify({
        model: "qwen3.5-plus",
        messages: [{ role: "user", content: "写一个5000字的故事" }],
        stream: true
    });
    const opts = {
        hostname: 'dashscope.aliyuncs.com',
        path: '/compatible-mode/v1/chat/completions',
        method: 'POST',
        headers: {
            'Content-Type': 'application/json',
            'Authorization': 'Bearer sk-your-api-key',
            'Content-Length': Buffer.byteLength(data)
        }
    };
    const req = https.request(opts, (res) => {
        // 持续接收 SSE，等待外部 SIGKILL
        res.on('data', () => {});
    });
    req.write(data);
    req.end();
}, 8000);  // 8s 延迟确保 SSL probe 已 attach
EOF

# 3. 启动测试进程
/tmp/openclaw-gateway /tmp/crash_agent_test.js &
TEST_PID=$!
echo "Test PID: $TEST_PID"

# 4. 等 SSL probe attach + 首次请求发出 + 收到部分 SSE 内容
sleep 12

# 5. 在请求进行中 SIGKILL 杀掉
kill -9 $TEST_PID
echo "killed PID $TEST_PID"

# 6. 等 ~2s 让 trace 模式 procmon 触发并写入
sleep 3

# 7. 验证 dmesg 中没有该 PID 的 OOM 记录（区分 OOM）
dmesg -T | grep -E "Killed process $TEST_PID" || echo "no OOM for PID $TEST_PID (expected)"

# 8. 检查 AgentSight 中断事件
/usr/local/sysak/.sysak_components/tools/agentsight interruption list \
  --last 1 --type agent_crash --json
# 预期: detail 中 source="trace_procmon_exit"，不含 "oom":true 字段

# 9. 清理
rm -f /tmp/openclaw-gateway /tmp/crash_agent_test.js
```

**与 OOM 场景的区别：** 唯一差异是 detail 里 `oom` 字段是否存在。SIGKILL/segfault/systemd stop 等非 OOM 原因导致 dmesg 没有 `Killed process <pid>` 记录，agentsight 就不会标记 `oom:true`。

### agent_crash_oom — Agent 进程 OOM 崩溃

通过 cgroup v2 内存限制触发 OOM kill，验证 AgentSight 能区分 OOM 崩溃与普通崩溃。

- 预期：1 个 `agent_crash`（severity: critical），detail 中包含 `"oom": true`
- 触发条件：trace 模式 procmon eBPF tracepoint 实时检测到进程退出 → 查询 dmesg 确认 OOM → 标记 `oom:true`
- 检测路径：
  - **实时路径**（~1s，主路径）：`ProcMon::Exit` eBPF tracepoint 触发后，drain pending 连接并查询 dmesg，detail 中 `source="trace_procmon_exit"`
  - **备份路径**（~30s，仅 serve 模式）：HealthChecker 周期性扫描，检测到进程消失后查询 dmesg
  - **启动恢复**：AgentSight 重启时扫描 dmesg 历史
- 用途：验证 OOM 归因的正确性

**手动验证方式（cgroup v2 内存限制）：**

```bash
# 1. 创建测试用 cgroup（限制 100MB 内存，禁止 swap）
mkdir -p /sys/fs/cgroup/agentsight-oom-test
echo "100M" > /sys/fs/cgroup/agentsight-oom-test/memory.max
echo "0"    > /sys/fs/cgroup/agentsight-oom-test/memory.swap.max

# 2. 准备模拟 agent 进程
#    创建 symlink 让进程名匹配 AgentSight 的默认 OpenClaw 规则
#    （注意 sysak 部署机器上 node 通常在 /usr/local/bin/node，按实际路径调整）
ln -sf $(which node) /tmp/openclaw-gateway

# 3. 编写测试脚本 /tmp/oom_agent_test.js
cat > /tmp/oom_agent_test.js << 'EOF'
const https = require('https');

// 等待 AgentSight attach SSL 探针（进程启动后需要几秒）
setTimeout(() => {
    const data = JSON.stringify({
        model: "qwen3.5-plus",
        messages: [{ role: "user", content: "写一个2000字的故事" }],
        stream: true
    });
    const opts = {
        hostname: 'dashscope.aliyuncs.com',
        path: '/compatible-mode/v1/chat/completions',
        method: 'POST',
        headers: {
            'Content-Type': 'application/json',
            'Authorization': 'Bearer sk-your-api-key',
            'Content-Length': Buffer.byteLength(data)
        }
    };
    const req = https.request(opts, (res) => {
        let n = 0;
        res.on('data', () => {
            // 收到 3 个 chunk 后开始分配大内存触发 OOM
            if (++n === 3) {
                const arrays = [];
                while (true) {
                    arrays.push(Buffer.alloc(10 * 1024 * 1024)); // 每次 10MB
                }
            }
        });
    });
    req.write(data);
    req.end();
}, 8000);  // 8s 延迟确保 SSL probe 已 attach
EOF

# 4. 在 cgroup 中运行测试进程
bash -c 'echo $$ > /sys/fs/cgroup/agentsight-oom-test/cgroup.procs; \
  exec /tmp/openclaw-gateway /tmp/oom_agent_test.js' &
TEST_PID=$!
echo "Test PID: $TEST_PID"

# 5. 等待 OOM kill 发生（通常 10-15s 内）
sleep 20

# 6. 验证 dmesg 中有 OOM kill 记录
dmesg -T | grep "Killed process" | tail -3

# 7. 检查 AgentSight 中断事件（通过 CLI 而非直接查 SQLite）
/usr/local/sysak/.sysak_components/tools/agentsight interruption list \
  --last 1 --type agent_crash --json
# 预期: detail 中含 "oom":true, "source":"trace_procmon_exit"

# 8. 清理（务必执行，否则 cgroup 残留）
rmdir /sys/fs/cgroup/agentsight-oom-test 2>/dev/null || true
rm -f /tmp/openclaw-gateway /tmp/oom_agent_test.js
```

**关键注意事项：**

| 项目 | 说明 |
|------|------|
| 进程名匹配 | `/tmp/openclaw-gateway` 的 comm 字段为 `openclaw-gatewa`（15 字符截断），匹配 `/etc/agentsight/config.json` 中已有的默认规则 `["*openclaw-gatewa*"]`，无需额外配置 cmdline |
| node 路径 | sysak 部署机通常 node 在 `/usr/local/bin/node`，可用 `which node` 自适应 |
| API 端点 | dashscope OpenAI 兼容端点：`https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions` |
| SSL probe 时序 | 进程启动后需等待 ~8s 让 AgentSight attach SSL uprobe，否则无法捕获 HTTPS 流量 |
| dmesg 权限 | AgentSight 需要 root 权限读取 dmesg（`dmesg -T`） |
| cgroup v2 | 需要系统使用 cgroup v2（检查 `mount \| grep cgroup2`） |
| source 字段 | trace 模式实时路径下 `detail.source = "trace_procmon_exit"`（serve 模式 HealthChecker 备份路径才会是 `"drain+dmesg"`） |
| 重复记录 | 同一进程若有多个 pending 连接，每条连接 drain 时都会触发一次记录（去重逻辑保证 1s 窗口内同 PID 不重复） |

### all — 全部场景

按顺序运行：healthy → auth_single → auth_storm → mixed_light → multi_type。

> 注：`agent_crash` 和 `agent_crash_oom` 不包含在 `all` 中（等待时间较长），需单独运行。

## 输出说明

脚本运行后会自动等待 AgentSight 处理事件，然后输出：

```
  === Results for 'multi_type' ===
  Calls made: 5
    normal qwen-max -> 200           # 正常请求
    auth_error qwen-max -> 401       # 认证错误
    normal qwen-max -> 200
    model_not_found nonexistent-model-xyz-999 -> 404  # 模型不存在
    normal qwen-max -> 200
  Logtail: 5 chat records, 2 interruption records    # logtail 导出记录
    INT: type=auth_error severity=high agent=TestAgent
    INT: type=llm_error severity=high agent=TestAgent
  DB interruption_events: 2 new                       # DB 中断记录
    type=auth_error severity=high agent=TestAgent
    type=llm_error severity=high agent=TestAgent
```

**验证要点：**

| 检查项 | 说明 |
|--------|------|
| HTTP 状态码 | 401/404 等错误码正确返回 |
| Logtail chat records | 每个请求产生 1 条 chat 记录 |
| Logtail interruption records | 错误请求产生对应 interruption 记录 |
| 中断类型分类 | auth_error / llm_error / network_timeout 正确匹配 |
| DB 记录 | interruption_events 表新增对应记录 |
| 无误报 | 正常请求不产生 interruption 记录 |

## 中断类型对照表

| 类型 | 触发条件 | 严重级别 |
|------|----------|----------|
| `auth_error` | 401/403，或错误信息含 `invalid_api_key` / `unauthorized` | high |
| `rate_limit` | 429，或错误信息含 `rate_limit` | medium |
| `network_timeout` | 408/504，或错误信息含 `timeout` | high |
| `service_unavailable` | 502/503，或错误信息含 `overloaded` | high |
| `safety_filter` | finish_reason == `content_filter` | medium |
| `context_overflow` | 错误信息含 `context_length_exceeded` 等 | high |
| `token_limit` | finish_reason == `length` 且 output_tokens >= max_tokens * 0.95 | medium |
| `llm_error` | HTTP >= 400 通用兜底（优先级最低） | high |
| `sse_truncated` | SSE 流未正常结束 | high |
| `agent_crash` | Agent 进程中途消失 | critical |

## 健康分计算

测试后可手动验证健康分：

```bash
ssh root@<HOST> 'python3 -c "
import json
from collections import defaultdict
with open(\"/var/sysom/ilog/agentsight\") as f:
    logs = [json.loads(l) for l in f]
chat = [l for l in logs if l.get(\"gen_ai.operation.name\") != \"interruption\"]
ints = [l for l in logs if l.get(\"gen_ai.operation.name\") == \"interruption\"]
convs = set(l.get(\"gen_ai.conversation.id\") for l in chat if l.get(\"gen_ai.conversation.id\"))
w = {\"critical\": 10, \"high\": 5, \"medium\": 2, \"low\": 1}
ss = defaultdict(int)
for il in ints:
    sid = il.get(\"gen_ai.session.id\", \"none\")
    ss[sid] += w.get(il.get(\"agentsight.interruption.severity\", \"medium\"), 1)
capped = sum(min(s, 10) for s in ss.values())
tc = len(convs)
score = round(max(0, 100 - min(100, capped / max(1, tc) * 100)), 1) if tc else 100.0
print(\"conversations={} interruptions={} capped_penalty={} health_score={}\".format(tc, len(ints), capped, score))
"'
```

公式：`score = 100 - min(100, capped_penalty / total_conversations * 100)`

- 分母用 conversation 数（不用 session 数），避免长生命周期 agent 被单次错误过度惩罚
- 同一 session 内的罚分上限 = 10（等于 1 次 critical），避免重试风暴放大惩罚

## 踩雷记录（实战经验）

以下是在 sysak 部署机器上实际验证时踩过的雷，按发生顺序记录。

### 1. 直接 curl/wget 不会被监控

**现象：** 用 `curl https://dashscope.aliyuncs.com/...` 直接发请求，agentsight 完全捕获不到流量，logtail/DB 里没记录。

**原因：** AgentSight 通过 cmdline 规则匹配 agent 进程后，对该进程的 SSL 库 attach uprobe（`SSL_read`/`SSL_write`），**不是按目标域名抓包**。curl 的 cmdline 通常不在 `cmdline.allow` 规则里。

**正确做法：** 必须通过被规则识别的 agent 进程（OpenClaw / Hermes / 配了 `*python3*` 的 TestAgent 等）发起请求。

### 2. python3 进程刚启动就发请求 → 漏掉首个请求

**现象：** 测试脚本一启动就调 `urllib.request.urlopen(...)`，401 在终端正常返回，但 logtail 里完全找不到这条记录。

**原因：** AgentSight 的处理链路是：procmon eBPF 检测到 exec → AgentScanner 匹配 cmdline → 解析 ELF 找 SSL 符号 → attach uprobe，整条链路 ~8-15s。如果进程在 attach 完成前就发出请求，SSL 握手已经走完，uprobe 才挂上，握手期间和首个 request body 的明文都漏掉。

**正确做法：** 测试脚本进程启动后**至少 sleep 10 秒再发首个请求**。`scenario_test.py` 已内置初始等待。

### 3. `discover` 命令显示不全 ≠ agentsight 没识别

**现象：** 改完 `/etc/agentsight/config.json` 加了 `*python3*` 规则，跑 `agentsight discover` 仍然只看到 OpenClaw，怀疑配置没生效。

**原因：** `discover` 命令用的是**编译时嵌入的默认规则** (`agentsight::default_cmdline_rules()`)，而不是 `/etc/agentsight/config.json`。daemon 进程读的才是配置文件。

**正确验证方式：** 不要用 `discover` 来验证规则是否生效，而是直接发请求看 logtail 有没有 `agent_name=TestAgent` 的记录。

### 4. cmdline 规则按 argv **位置**匹配，不是整体子串匹配

**现象：** 想加规则匹配 "包含 hermes 关键字的 python 进程"，写成 `["*python*hermes*"]` 不工作。

**原因：** matcher 把 patterns 数组按下标对齐到 argv 数组，每个 pattern 单独 glob 匹配对应位置的 arg。`["*python*", "*hermes*"]` 才是要 `argv[0]` 含 python 且 `argv[1]` 含 hermes。`["*python3*"]` 单元素只匹配 `argv[0]`，对 `argv[1]+` 不约束（前缀匹配）。

**正确做法：** 写规则时心里要清楚每个元素对齐 argv 的哪一位。

### 5. 修改配置后 daemon 不会自动 reload

**现象：** 改完 `/etc/agentsight/config.json`，新进程仍然没被识别为 TestAgent。

**原因：** 当前版本不监听配置文件 inotify，只在启动时读一次。

**正确做法：** 必须 `pkill -9 -f "agentsight trace"` 然后重新 daemon 启动。重启时一定要带回 `SLS_LOGTAIL_FILE` 等环境变量，否则丢失 SLS 上传能力。

### 6. SSH 调用 `kill $(pgrep ...)` exit 255

**现象：** `ssh root@host 'kill $(pgrep -f xxx)'` 返回 exit 255 / signal 127，看似失败。

**原因：** 当 `kill` 的目标是 ssh 自己启动的子进程链时，shell 会被 signal 一起带掉，导致 ssh 端报错。**实际上 kill 已经成功执行**。

**正确做法：** 用 `pgrep` 拿到 PID 单独 ssh 一次再 kill；或者直接忽略这个 exit code，下一步用 `pgrep` 再检查一次确认。

### 7. node 路径在不同发行版不一致

**现象：** 文档示例里的 `ln -sf /usr/bin/node /tmp/openclaw-gateway` 在 sysak 部署机上不存在该文件。

**原因：** sysak 镜像装的是 nvm 管理的 node，路径在 `/usr/local/bin/node` 或 `~/.nvm/versions/node/vXX/bin/node`。

**正确做法：** 用 `$(which node)` 自适应。

### 8. `dashscope.aliyuncs.com` 有两套不兼容的 API

**现象：** 文档示例用 `/api/v1/services/aigc/text-generation/generation` + payload `{"input":{"messages":[...]}, "parameters":{...}}` 发请求 200，但消息字段对不上 OpenAI 兼容格式。

**原因：** dashscope 同时提供两套接口：
- 原生 API：`/api/v1/services/aigc/text-generation/generation`，需要 `X-DashScope-SSE: enable` 头开启流式
- OpenAI 兼容：`/compatible-mode/v1/chat/completions`，payload `{"model":"...", "messages":[...], "stream":true}`

**正确做法：** 测试脚本统一用 OpenAI 兼容端点，跟 OpenClaw 实际行为一致，agentsight 解析器路径也是同一条。

### 9. cgroup v2 OOM 触发不稳定

**现象：** 限制 50MB 内存，python 持续分配，进程没被 kill 反而卡住。

**原因：**
- 没禁用 swap：`memory.swap.max` 默认无限，进程会被 swap 而不是 kill
- 把当前 shell 自己也写进了 cgroup：`echo $$ > cgroup.procs` 后 exec 子进程，shell 继承 cgroup 没问题；但如果用 `bash -c '...' &` 形式启动，cgroup 限制可能没传给子进程

**正确做法：**
```bash
echo "100M" > .../memory.max
echo "0"    > .../memory.swap.max          # 必须禁 swap
bash -c 'echo $$ > .../cgroup.procs; exec /tmp/agent script.js'  # 用 exec 避免多余 shell
```

### 10. 同一进程产生多条 agent_crash 记录

**现象：** 一次 OOM kill，DB 里出现 2 条 `agent_crash` 记录，PID 相同。

**原因：** 进程崩溃时如果有多个 pending HTTP 连接（例如同时跑两个 stream 请求），drain 路径会按连接逐个写记录。设计上有 1s 窗口去重，但若两条连接的 drain 跨越窗口边界仍可能各写一条。

**判断方式：** 看 `detail.call_ids` 数组长度，多条记录的 call_ids 通常不重复，每条对应不同的 pending call。这是符合预期的——**一个 crash 影响的所有 pending call 都该被报出来**，不是 bug。

### 11. logtail 文件由 iLogtail 采集，不要直接 truncate

**现象：** 想清空测试数据重新跑，`> /var/sysom/ilog/agentsight` 之后 iLogtail 报告偏移异常，后续记录漏传 SLS。

**原因：** iLogtail 维护文件 inode + 偏移的 checkpoint，truncate 会导致 checkpoint 失效但不被发现。

**正确做法：** 不要直接清空 logtail 文件。要清理测试数据，应该删除 SQLite DB 或在 SLS 侧按时间区间过滤。

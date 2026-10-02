# AgentSight Agent 诊断功能指南

## 概述

AgentSight 是一款基于 eBPF 的 AI Agent 可观测性工具，提供零侵入式的 Agent 运行时诊断能力。无需修改 Agent 代码或配置，即可实现对 LLM API 调用、进程行为、异常中断的全方位监控和诊断。

**核心价值：**

- **零侵入** — 基于 eBPF 内核探针采集，Agent 无感知
- **实时诊断** — 毫秒级检测异常中断、进程崩溃、死循环等问题
- **智能归因** — 自动分类中断类型并给出严重级别
- **可视化** — 内置 Dashboard 提供实时健康视图和历史回溯

---

## 功能一览

| 诊断能力 | 说明 |
|----------|------|
| Agent 自动发现 | 扫描 /proc 和监控 execve 事件，自动识别运行中的 AI Agent |
| 进程健康监控 | 周期性探测 Agent 存活状态与响应延迟 |
| 中断事件检测 | 12 种中断类型的实时检测与自动分类 |
| 死循环检测 | 跨调用分析识别 Agent 陷入逻辑循环 |
| OOM 崩溃归因 | 通过 dmesg 分析确认进程是否因内存不足被杀 |


---

## 1. Agent 自动发现

AgentSight 支持两种 Agent 发现机制：

### 1.1 命令行规则匹配

通过配置文件中的 `cmdline.allow` 规则，基于进程命令行 Glob 模式匹配 Agent：

```json
{
  "cmdline": {
    "allow": [
      {"rule": ["hermes*"], "agent_name": "Hermes"},
      {"rule": ["node*", "*openclaw*"], "agent_name": "OpenClaw"},
      {"rule": ["*node*", "*cosh*"], "agent_name": "Cosh"},
      {"rule": ["claude*"], "agent_name": "Claude"}
    ]
  }
}
```

- 支持 `*`（任意字符串）和 `?`（单字符）通配符
- `rule` 数组中的多个模式按顺序匹配命令行参数
- `agent_name` 为该 Agent 在系统中的显示名称

### 1.2 DNS 域名规则匹配

通过 HTTPS 域名规则发现正在调用 LLM API 的进程：

```json
{
  "https": [
    {"rule": ["*.openai.com"]},
    {"rule": ["*.anthropic.com"]},
    {"rule": ["dashscope.aliyuncs.com", "*.maas.aliyuncs.com"]}
  ]
}
```

当检测到进程发起对匹配域名的 DNS 请求或已建立 TCP 连接时，自动附着 SSL 探针进行流量采集。

### 1.3 内置支持的 Agent

| Agent 名称 | 识别方式 |
|------------|----------|
| Hermes | 进程命令行包含 `hermes` |
| Cosh (Copilot Shell) | Node.js 进程，命令行包含 `cosh` / `copilot-shell` |
| OpenClaw | 进程名包含 `openclaw-gatewa` |
| Claude Code | 进程名以 `claude` 开头 |
| 自定义 Agent | 通过 `cmdline.allow` 配置自定义规则 |

---

## 2. 进程健康监控

AgentSight 后台 HealthChecker 线程周期性执行健康检查：

### 2.1 监控机制

AgentSight 对所有发现的 Agent 进程提供**两层监控**：

| 层级 | 能力 | 端口要求 |
|------|------|----------|
| **进程存活性监控** | 周期性扫描进程是否存在，消失即触发 `agent_crash` 崩溃检测 | 无需端口 |
| **HTTP 健康探测** | 对监听端口发起 HTTP 请求，判断响应延迟和进程挂死 | 需要端口 |

即使 Agent 没有监听任何 TCP 端口（如 Cosh），AgentSight 依然能检测其崩溃退出并生成中断事件。

### 2.2 检查流程

1. 扫描系统中所有匹配的 Agent 进程
2. 将消失的进程标记为 Offline 并触发崩溃检测
3. 对存活进程检测其监听的 TCP 端口
4. 有端口时发起 HTTP 探测，判断响应状态

### 2.3 健康状态

| 状态 | 含义 |
|------|------|
| `Healthy` | HTTP 探测收到响应（任何状态码） |
| `Unhealthy` | 连接被拒绝或无法到达 |
| `Hung` | 连接成功但响应超时（进程可能挂死） |
| `NoPort` | 进程存活但无监听端口（仍可检测崩溃） |
| `Offline` | 进程异常退出（仅关联了 crash 事件时保留） |

### 2.4 Dashboard 展示规则

Dashboard 的「Agent 看板」从 `genai_events.db` 与 `trajectories.db` 汇总历史活动，展示所有曾被采集到的 Agent、最后活跃时间、活动量、Token 与数据来源。

独立的「运行时健康」区域继续实时展示已发现的 Agent 进程健康状态，展示规则如下：

- **正常退出**（无未完成的 LLM 调用）：静默移除，不在侧边栏展示，不生成中断事件
- **异常退出**（存在 pending LLM 调用被中断）：生成 `agent_crash` 中断事件，侧边栏展示崩溃记录，保留 5 分钟后自动清理
- **卡顿（Hung）**：始终展示，提供一键重启按钮

这意味着 Agent 正常重启（如 OpenClaw 自动重启）不会在侧边栏产生噪音，只有真正影响用户对话的崩溃才会被展示。

### 2.5 API 查询

```bash
# 查询历史 Agent 活动
curl http://127.0.0.1:7396/api/agent-health

# 查询当前进程健康（支持 include_clients=true）
curl http://127.0.0.1:7396/api/agent-process-health
```

响应示例：

```json
{
  "agents": [
    {
      "agent_name": "OpenClaw",
      "last_seen_ns": 1717830000000000000,
      "genai_calls": 128,
      "genai_tokens": 500000,
      "trajectory_steps": 42,
      "trajectory_tokens": 120000,
      "source": "genai_events+trajectories"
    }
  ]
}
```

---

## 3. 中断事件检测

AgentSight 实时分析每次 LLM API 调用，自动检测并分类 12 种中断类型。

### 3.1 中断类型

| 类型 | 触发条件 | 严重级别 | 验证状态 |
|------|----------|----------|----------|
| `agent_crash` | Agent 进程中途消失（有未完成的 LLM 调用） | Critical | 已验证 |
| `dead_loop` | Agent 陷入逻辑循环（重复工具调用序列/相似输出） | Critical | 已验证 |
| `retry_storm` | 同一错误类型在会话内重复超过阈值 | Critical | 已验证 |
| `auth_error` | HTTP 401/403 或错误含 `invalid_api_key` | High | 已验证 |
| `network_timeout` | HTTP 408/504 或错误含 `timeout` | High | 已编码 |
| `service_unavailable` | HTTP 502/503 或错误含 `overloaded` | High | 已编码 |
| `context_overflow` | 上下文长度超限（`context_length_exceeded` 等） | High | 已编码 |
| `sse_truncated` | SSE 流未正常结束 | High | 已编码 |
| `llm_error` | HTTP >= 400 通用兜底（优先级最低） | High | 已编码 |
| `rate_limit` | HTTP 429 或错误含 `rate_limit` | Medium | 已编码 |
| `safety_filter` | finish_reason == `content_filter` | Medium | 已编码 |
| `token_limit` | finish_reason == `length` 且输出达到上限 | Medium | 已编码 |

> **验证状态说明**：「已验证」表示已通过端到端集成测试确认功能正确；「已编码」表示检测逻辑已实现，将在后续版本补充端到端验证。

### 3.2 严重级别

| 级别 | 权重 | 说明 |
|------|------|------|
| Critical | 4 | 需要立即处理，Agent 已不可用 |
| High | 3 | 严重影响功能，需尽快排查 |
| Medium | 2 | 对用户有影响，建议关注 |
| Low | 1 | 轻微影响，可观察 |

### 3.3 检测优先级

当一次 LLM 调用同时命中多条规则时，按以下优先级取最高优先级的类型：

1. AuthError（401/403）
2. RateLimit（429）
3. NetworkTimeout（408/504）
4. ServiceUnavailable（502/503）
5. ContextOverflow
6. SafetyFilter
7. LlmError（通用 HTTP 错误兜底）
8. SseTruncated
9. TokenLimit

### 3.4 API 接口

```bash
# 查询中断事件列表
curl "http://127.0.0.1:7396/api/interruptions?start_ns=&end_ns=&agent_name=&interruption_type=&severity="

# 按严重级别统计
curl "http://127.0.0.1:7396/api/interruptions/count"

# 按类型统计
curl "http://127.0.0.1:7396/api/interruptions/stats"

# 按 Session 维度聚合
curl "http://127.0.0.1:7396/api/interruptions/session-counts"

# 按 Conversation 维度聚合
curl "http://127.0.0.1:7396/api/interruptions/conversation-counts"

# 查询指定 Session 的中断
curl "http://127.0.0.1:7396/api/sessions/{session_id}/interruptions"

# 查询指定 Conversation 的中断
curl "http://127.0.0.1:7396/api/conversations/{conversation_id}/interruptions"
```

---

## 4. 死循环检测（DeadLoop）

当 Agent 未触发任何错误，但陷入逻辑循环（重复相同的工具调用或生成相似输出）时，AgentSight 通过跨调用分析识别此类问题。

### 4.1 检测规则

| 规则 | 说明 | 默认阈值 |
|------|------|----------|
| 工具序列重复 | 连续 N 次调用使用相同的工具名序列 | 3 次 |
| 输出相似度循环 | 连续 N 次输出的 Jaccard 相似度 > 阈值 | 3 次，相似度 0.85 |
| Token 空耗 | 输入 Token 持续增长但输出不变 | — |

### 4.2 配置

```json
{
  "deadloop": {
    "enabled": true,
    "kill_after_count": 3
  }
}
```

- `enabled` — 是否启用死循环检测（默认关闭）
- `kill_after_count` — 连续检测到 N 次循环后自动终止 Agent 进程（设为 0 表示仅告警不终止）

### 4.3 检测窗口

- 滑动窗口大小：最近 10 次 LLM 调用
- 仅分析同一 conversation 内的调用序列

---

## 5. OOM 崩溃归因

当 Agent 进程因内存不足 (OOM) 被内核杀掉时，AgentSight 能自动归因。

### 5.1 检测路径

| 路径 | 触发时机 | 延迟 |
|------|----------|------|
| 实时路径 | HealthChecker 检测到进程消失后查询 dmesg | ~30s |
| 启动恢复 | AgentSight 自身被 OOM 后重启，扫描 dmesg 历史 | 启动时 |

### 5.2 事件标记

OOM 崩溃事件在 `detail` 字段中额外包含：

```json
{
  "oom": true,
  "source": "healthchecker+dmesg",
  "pid": 12345,
  "agent_name": "OpenClaw"
}
```

---

## 6. 健康分计算

AgentSight 基于中断事件计算 Agent 运行健康分（0~100）。

### 6.1 计算公式

```
health_score = 100 - min(100, capped_penalty / total_conversations * 100)
```

### 6.2 计算规则

- **惩罚权重**：Critical=10, High=5, Medium=2, Low=1
- **同 Session 惩罚上限**：单个 Session 内累计惩罚最高 10 分（防止重试风暴放大惩罚）
- **分母为 Conversation 数**：避免长生命周期 Agent 被单次错误过度惩罚

### 6.3 示例

| 场景 | 正常对话数 | 中断情况 | 健康分 |
|------|-----------|----------|--------|
| 完全健康 | 10 | 无 | 100 |
| 偶发错误 | 10 | 1 个 auth_error (high=5) | 95 |
| 进程崩溃 | 10 | 1 个 agent_crash (critical=10) | 90 |
| 严重异常 | 5 | 3 个 auth_error (capped=10) | 60 |

---

## 7. Prometheus 指标导出

AgentSight 提供标准 Prometheus 格式的 `/metrics` 端点：

```bash
curl http://127.0.0.1:7396/metrics
```

导出指标包括：

| 指标名 | 类型 | 说明 |
|--------|------|------|
| `agentsight_llm_requests_total` | counter | 各 Agent 的 LLM 请求总数 |
| `agentsight_interruptions_total` | counter | 各中断类型的事件总数 |

---

## 8. Dashboard 可视化

AgentSight 内置 Web Dashboard，通过 `agentsight serve` 启动：

```bash
# 启动（默认 127.0.0.1:7396）
agentsight serve

# 绑定到所有接口
agentsight serve --host 0.0.0.0 --port 7396
```

打开 `http://<server-ip>:7396` 即可查看：

- **会话列表** — 所有被追踪的 Agent 会话及其 Token 统计
- **对话详情** — 每轮 LLM 调用的输入输出、工具调用、Token 消耗
- **中断事件** — 按严重级别标注的中断事件时间线
- **Agent 健康** — 实时进程健康状态面板
- **Token 趋势** — 分 Agent / 分模型的 Token 时序图

---

## 9. 快速开始

### 9.1 环境要求

| 组件 | 版本要求 |
|------|----------|
| Linux 内核 | >= 5.8（需 BTF 支持） |
| 运行权限 | root（eBPF 和 dmesg 需要） |

### 9.2 安装

```bash
sudo yum install agentsight
```

### 9.3 启动追踪

```bash
# 前台模式（开发调试）
sudo agentsight trace

# 后台 Daemon 模式
sudo agentsight trace --daemon
```

### 9.4 配置文件

配置文件路径：`/etc/agentsight/config.json`（若不存在则使用内置默认规则）

完整配置示例：

```json
{
  "https": [
    {"rule": ["*.openai.com"]},
    {"rule": ["*.anthropic.com"]},
    {"rule": ["dashscope.aliyuncs.com", "*.maas.aliyuncs.com"]}
  ],
  "cmdline": {
    "allow": [
      {"rule": ["hermes*"], "agent_name": "Hermes"},
      {"rule": ["node*", "*openclaw*"], "agent_name": "OpenClaw"},
      {"rule": ["*node*", "*cosh*"], "agent_name": "Cosh"},
      {"rule": ["claude*"], "agent_name": "Claude"},
      {"rule": ["*python3*", "*my-agent*"], "agent_name": "MyAgent"}
    ]
  },
  "deadloop": {
    "enabled": false,
    "kill_after_count": 3
  }
}
```

修改配置后重启服务生效：

```bash
systemctl restart agentsight
```

---

## 10. 数据存储与导出

### 10.1 本地存储

所有诊断数据存储在 SQLite 数据库中：

- 默认路径：`/var/log/sysak/.agentsight/agentsight.db`
- 数据保留：默认 30 天自动清理

### 10.2 云端导出

支持将诊断数据导出到阿里云 SLS（日志服务）进行集中分析：

```bash
# 通过环境变量配置 Logtail 导出路径
Environment=SLS_LOGTAIL_FILE=/var/sysom/ilog/agentsight
```

### 10.3 ATIF 标准格式导出

支持将 Agent 对话轨迹以 ATIF（Agent Trajectory Interchange Format）v1.7 标准格式导出：

```bash
# 按 trace 导出
curl http://127.0.0.1:7396/api/export/atif/trace/{trace_id}

# 按 session 导出
curl http://127.0.0.1:7396/api/export/atif/session/{session_id}

# 按 conversation 导出
curl http://127.0.0.1:7396/api/export/atif/conversation/{conversation_id}
```

### 10.4 Conversation Grader

Conversation Grader 对指定 `conversation_id` 执行手动质量评估，并将结果持久化到 SQLite。当前版本只支持 `target_type=conversation`，评估器为规则版 `rule-v3`。

```bash
curl -X POST http://127.0.0.1:7396/api/grader/evaluate \
  -H 'Content-Type: application/json' \
  -d '{"target_type":"conversation","target_id":"{conversation_id}","force":false}'

curl "http://127.0.0.1:7396/api/grader/latest?target_type=conversation&target_id={conversation_id}"
```

如果 conversation 仍有 pending LLM 调用，评估接口返回 HTTP 409。确认需要评估当前不完整快照时，可以将 `force` 设置为 `true`，结果会标记 `evaluated_with_pending=true`。

---

## 11. 支持的 LLM 提供商

AgentSight 自动识别并解析以下 LLM API 格式：

| 提供商 | API 格式 | Token 解析 |
|--------|----------|-----------|
| OpenAI / 兼容 API | OpenAI Chat Completions | 完整 |
| Anthropic (Claude) | Messages API（含 cache token） | 完整 |
| Google Gemini | GenerateContent API | 完整 |
| 通义千问 (Qwen) | DashScope OpenAI 兼容模式（`/compatible-mode/v1/chat/completions`） | 完整 |
| 百炼 / DashScope 原生 | `/api/v1/services/aigc/text-generation/generation`、`/api/v1/services/aigc/multimodal-generation/generation` | 完整（多模态 `image_tokens` 计入 input） |

> 百炼原生协议是 Dify / LangGraph 中「百炼」类型节点发出的请求格式，与 OpenAI 兼容模式并行存在：
> 请求把 messages 嵌在 `input` 对象里，响应把结果包在 `output` 里且顶层无 `model`（model 名取自请求体），
> 流式响应不发 `data: [DONE]`，以 `output.finish_reason` 作为结束标志。

---

## 附录：完整 API 列表

| 端点 | 方法 | 说明 |
|------|------|------|
| `/health` | GET | 服务健康检查 |
| `/metrics` | GET | Prometheus 指标 |
| `/api/sessions` | GET | 会话列表 |
| `/api/sessions/{id}/traces` | GET | 会话下的对话列表 |
| `/api/traces/{id}` | GET | Trace 详情 |
| `/api/conversations/{id}` | GET | 对话详情 |
| `/api/grader/evaluate` | POST | 手动评估 Conversation 质量 |
| `/api/grader/latest` | GET | 查询最新 Conversation 评估结果 |
| `/api/agent-names` | GET | Agent 名称列表 |
| `/api/agent-health` | GET | SQLite 历史 Agent 活动 |
| `/api/agent-process-health` | GET | 当前 Agent 进程健康状态 |
| `/api/interruptions` | GET | 中断事件列表 |
| `/api/interruptions/count` | GET | 中断统计（按严重级别） |
| `/api/interruptions/stats` | GET | 中断统计（按类型） |
| `/api/interruptions/session-counts` | GET | 各 Session 的中断聚合 |
| `/api/interruptions/conversation-counts` | GET | 各 Session + Conversation 的中断聚合 |
| `/api/sessions/{id}/interruptions` | GET | 指定 Session 的中断 |
| `/api/conversations/{id}/interruptions` | GET | 指定 Conversation 的中断 |
| `/api/export/atif/trace/{id}` | GET | ATIF 格式导出 |

---
name: agentsight
description: 通过命令行或 Dashboard（前端） 查询 AgentSight 平台的 token 消耗数据、token 节省（优化）数据、审计事件和会话中断事件。当用户询问 token 用量、花费、消耗趋势、token 节省、优化效果、LLM 调用、进程行为审计，或询问会话中断、Agent 异常、LLM 错误等中断事件时使用此技能。
---

# Dashboard（前端）

AgentSight 提供 Web Dashboard（前端） 用于可视化查看 token 消耗和会话历史。

**访问方式：**
- 远程访问：`http://<server-ip>:7396`（需确保安全组已开放该端口）
- 本地访问：`http://127.0.0.1:7396`

**功能：**
- Token 消耗趋势图（按天/小时）
- 会话历史浏览
- LLM 调用详情查看
- 支持按时间范围、进程过滤

**启动服务：**
```bash
# 如果服务未运行，启动它
sudo systemctl start agentsight

# 查看服务状态
sudo systemctl status agentsight
```

---

# Token 查询

## 常用命令

| 命令 | 说明 |
|------|------|
| `/usr/local/bin/agentsight token --period today` | 今天消耗 |
| `/usr/local/bin/agentsight token --period yesterday` | 昨天消耗 |
| `/usr/local/bin/agentsight token --hours 3` | 最近 3 小时 |
| `/usr/local/bin/agentsight token --period today --compare` | 今天 vs 昨天对比 |

## 返回示例

```
今天共消耗 125,000 tokens，比昨天（98,000）增长 27%。

输入: 125,000 | 输出: 85,000
```

---

# 审计查询

## 常用命令

| 命令 | 说明 |
|------|------|
| `/usr/local/bin/agentsight audit` | 最近 24 小时事件 |
| `/usr/local/bin/agentsight audit --last 48` | 最近 48 小时 |
| `/usr/local/bin/agentsight audit --pid 12345` | 指定进程 |
| `/usr/local/bin/agentsight audit --type llm` | 仅 LLM 调用 |
| `/usr/local/bin/agentsight audit --type process` | 仅进程行为 |
| `/usr/local/bin/agentsight audit --summary` | 汇总统计 |
| `/usr/local/bin/agentsight audit --summary --last 72` | 最近 72 小时汇总 |
| `/usr/local/bin/agentsight audit --json` | JSON 格式 |

## 返回示例

**汇总输出：**
```
=== Audit Summary (last 24 hours) ===

LLM calls:        42
Process actions:  128

Providers:
  OpenAI: 35 calls
  Anthropic: 7 calls

Top commands:
  python agent.py: 25 times
  node server.js: 17 times
```

**事件列表（JSON）：**
```json
{"event_type":"llm_call","pid":1234,"comm":"python",
 "extra":{"provider":"OpenAI","model":"gpt-4o","input_tokens":1500,"output_tokens":800}}
```

## 事件类型

| 类型 | 字段 |
|------|------|
| `llm_call` | provider, model, input_tokens, output_tokens, request_path, response_status, is_sse |
| `process_action` | filename, args, exit_code |

---

# 会话中断查询

数据库：`/var/log/sysak/.agentsight/interruption_events.db`

中断类型：`llm_error`、`sse_truncated`、`context_overflow`、`agent_crash`、`token_limit`
严重级别：`critical` > `high` > `medium` > `low`

使用 `/usr/local/bin/agentsight interruption --help` 查看所有子命令和参数。

---

# Token 节省查询

`GET /api/token-savings` — 参数：`start_ns`、`end_ns`、`agent_name`；数据源：`genai_events.db` + `~/.tokenless/stats.db`
`GET /api/agent-names` — 获取可选的 `agent_name` 值列表

---

# 注意事项

- 数据存储：`/var/log/sysak/.agentsight/agentsight.db`（SQLite）
- 默认保留：7 天
- 时间戳：纳秒级 Unix 时间戳
- 权限：需要 root 运行 eBPF

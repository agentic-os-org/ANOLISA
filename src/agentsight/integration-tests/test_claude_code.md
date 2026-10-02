# Claude Code 集成测试

> 前置条件见 [RULES.md](RULES.md)（环境变量、部署流程、通用规则）

## 测试目标

验证 agentsight 对 Claude Code 客户端的支持：BoringSSL 探针 attach、Anthropic SSE 流（含 thinking/tool_use）解析落库、msg_id 会话关联、进程退出 inode 清理。

1. Claude Code 进程启动后，sslsniff 应识别其使用的 BoringSSL 库并 attach（判定依据：日志含 `[attach_process] pid=<claude_pid>: attaching ... → <claude_binary_path>`，且无 `BoringSSL byte-pattern detection failed for <claude_binary_path>`）
2. 同一 Claude Code 进程的多次 SSL 句柄不应对相同 inode 重复 attach（判定依据：日志含 `[attach_process] pid=<claude_pid>: skipping already-traced <claude_binary_path>`，且该 inode 在首次 `attaching` 之后再次出现时被跳过）
3. Claude Code 调用 Anthropic API 后，SSE 流（含 thinking 与 tool_use 事件）应被解析并落入 SQLite `genai_events` 表（判定依据：`SELECT * FROM genai_events WHERE provider='anthropic' AND pid=<claude_pid>` 返回 ≥1 条记录，且 `model` 字段非空、以 `claude-` 开头）
4. response_map 应能从 Anthropic 响应中提取 `msg_*` 格式 ID 用于会话关联（判定依据：`genai_events.call_id` 中存在以 `msg_` 开头的字符串）
5. Claude Code 进程退出后，其 inodes 应从 `traced_files` 移除（判定依据：日志含 `[detach_process] pid=<claude_pid>: removed N inodes from traced_files`，N ≥ 1）

## 判定方法

优先使用 **SQLite 查询**验证数据落库，日志（`RUST_LOG=debug`）用于辅助定位 attach / detach 行为。

| 方法 | 适用场景 |
|------|----------|
| `sqlite3 <db_path> "SELECT ..."` | 验证 SSE 解析与 msg_id 关联落库（目标 3、4） |
| 日志 grep 关键行 | 验证 BoringSSL attach、inode 去重、进程退出清理（目标 1、2、5） |

数据库默认路径：`/var/log/sysak/.agentsight/agentsight.db`

## 测试配置

使用以下 JSON 配置文件（保存到测试机 `/etc/agentsight/config.json`）：

```json
{
  "cmdline": {
    "allow": [
      {"rule": ["*claude*"]}
    ]
  }
}
```

> **说明**：cmdline allow 通过 `*claude*` 匹配 Claude Code 进程命令行，触发 sslsniff 对该进程的 BoringSSL byte-pattern 探测与 attach。

## 测试步骤

### 步骤 1：验证 BoringSSL 探针 attach

1. 将上述配置写入 `/etc/agentsight/config.json`
2. 启动 trace 并把日志重定向到文件：
   ```bash
   RUST_LOG=debug agentsight trace --verbose 2>/tmp/agentsight-test-claude.log &
   ```
3. 启动 Claude Code 客户端，记录其 PID（记为 `<claude_pid>`）
4. grep 关键日志确认 attach 成功：
   ```bash
   grep "\[attach_process\] pid=<claude_pid>" /tmp/agentsight-test-claude.log | grep "attaching"
   ```
   预期：至少 1 行，kind 为 BoringSSL，path 指向 Claude Code 二进制（或其使用的 BoringSSL 库文件）
5. 确认无 BoringSSL byte-pattern 探测失败：
   ```bash
   grep "BoringSSL byte-pattern detection failed" /tmp/agentsight-test-claude.log
   ```
   预期：无输出（或不针对当前 Claude Code 二进制路径）

### 步骤 2：触发 Anthropic API 调用并验证 SSE 解析 + msg_id 关联

1. 保持 agentsight trace 运行
2. 在 Claude Code 中发起一次会同时触发 thinking 与 tool_use 的对话（例如要求 Claude 执行 shell 命令或读取本地文件，并确认 extended thinking 已开启）
3. 等待响应完成（SSE 流终结）
4. 查询 SQLite 验证 SSE 解析与落库：
   ```bash
   sqlite3 /var/log/sysak/.agentsight/agentsight.db \
     "SELECT pid, provider, model, call_id FROM genai_events \
      WHERE pid=<claude_pid> AND provider='anthropic' \
      ORDER BY start_timestamp_ns DESC LIMIT 5"
   ```
   预期：返回 ≥1 条记录，`model` 以 `claude-` 开头（如 `claude-sonnet-4-*`、`claude-opus-*`）
5. 验证 msg_* 格式 ID 被提取并写入 call_id：
   ```bash
   sqlite3 /var/log/sysak/.agentsight/agentsight.db \
     "SELECT call_id FROM genai_events \
      WHERE pid=<claude_pid> AND call_id LIKE 'msg_%' LIMIT 5"
   ```
   预期：返回 ≥1 条以 `msg_` 开头的 call_id

### 步骤 3：验证进程退出后 inode 清理

1. 终止 Claude Code 进程：`kill <claude_pid>`
2. 等待 ≤5 秒（让 sslsniff 处理进程退出事件）
3. grep 退出清理日志：
   ```bash
   grep "\[detach_process\] pid=<claude_pid>: removed" /tmp/agentsight-test-claude.log
   ```
   预期：找到对应行，`removed N inodes from traced_files` 中 N ≥ 1
4. （可选）若该 Claude Code 二进制再次启动，sslsniff 应能重新 attach 而不会因为旧 inode 仍在 `traced_files` 中而被跳过（验证清理对后续 attach 的恢复作用）

## 运行条件

- root 权限（eBPF 要求）
- Linux kernel >= 5.8 with BTF
- 测试机已安装 Claude Code 客户端（其 SSL 实现为静态/动态链接的 BoringSSL）
- 网络可达 `api.anthropic.com`，并已配置有效 `ANTHROPIC_API_KEY`
- 测试对话需触发至少一次 extended thinking 与一次 tool_use（覆盖 `aggregate_sse_events` 的 Thinking 与 ToolUse 分支）

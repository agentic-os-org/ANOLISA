# Tokenless 快速开始

[English](../../../en/token-saving/tokenless/QUICKSTART.md)

大约三分钟内完成 Tokenless 安装、接入 Claude Code、运行一次真实任务，并确认一条
压缩前后的 Token 记录。Tokenless 在后台工作，不需要改变 Prompt 或日常使用
Agent 的方式。

实际节省效果取决于工作负载。工具调用密集型任务通常最明显；较短或以对话为主的
任务可能变化不大。

## 1. 安装 Tokenless 并接入 Claude Code

下面以 Claude Code 作为示例 Agent：

```bash
curl -fsSL https://get.agentic-os.sh | bash
export PATH="$HOME/.local/bin:$PATH"
anolisa install tokenless
anolisa adapter enable tokenless claude-code
```

如果已经安装 anolisa CLI，可以直接从 `anolisa install tokenless` 开始。只有首次
安装提示当前 Shell 找不到 `~/.local/bin` 时，才需要执行 PATH 设置。

使用其他 Agent？按照[使用其他 Agent](#使用其他-agent)中的对应接入方式操作，后续
步骤保持不变。大多数 Agent 使用 `anolisa adapter enable` 命令，OpenCode 则使用
链接指向的生命周期脚本。

## 2. 运行一次真实任务

重启 Claude Code，使其加载 Adapter，然后启动新的 Session，并运行一次工具密集型
任务。例如：

> 运行当前仓库的完整测试，只总结失败项。

Prompt 中不需要提到 Tokenless。

## 3. 查看节省效果

Claude Code 使用一次 Shell、API 或其他受支持的工具后，运行：

```bash
tokenless stats list --limit 5
tokenless stats summary
```

输出示例（实际数值因任务而异）：

```text
Showing 1 record(s):
================================================================================
[ID:42] 2026-08-12 10:20:30 | claude-code | Session:- | Tool:- | Chars:5120→2880(-2240) | Tokens:1280→720(-44%)

Tokenless Statistics Summary
============================================================
Total Records: 1

Character Savings:
  Before: 5120 chars
  After:  2880 chars
  Saved:  2240 chars (43.8%)

Token Savings:
  Before: 1280 tokens
  After:  720 tokens
  Saved:  560 tokens (43.8%)

Breakdown by Operation:
----------------------------------------
  compress-response: 1 records
    Chars: 5120 -> 2880 (-43.8%)
    Tokens: 1280 -> 720 (-43.8%)
```

当 `stats list` 中出现 Token 估算值从压缩前到压缩后下降的记录时，首次体验即完成。
如需检查某条记录具体改变了什么，复制其 ID 后运行：

```bash
tokenless stats diff <record-id>
```

需要查看一段时间内的可视化节省趋势时，前往
[AgentSight 用户指南](../../agent-observability/agentsight/integrations.md#tokenlesstoken-节省)。
Tokenless 与 AgentSight 由同一用户运行时，Dashboard 可以直接读取本地统计，不需要
配置 SLS。

如果没有记录，可能是内容没有经过 Tokenless，或处理后没有变短。先检查 Adapter
和组件健康状态：

```bash
anolisa adapter status tokenless
anolisa doctor tokenless
```

再参阅[开启后没有产生统计记录](troubleshooting.md#启用后没有产生统计记录)。

Token 数只是在 Tokenless 已处理内容范围内的估算值，不等于模型账单的直接变化。
统计和 diff 可能包含原始工具内容；涉及敏感数据时不要分享输出。完整说明见
[效果度量](measuring-savings.md)和
[配置与数据隐私](configuration-and-privacy.md)。

## 使用其他 Agent

扫描当前机器，然后只启用正在使用的 Agent：

```bash
anolisa adapter scan
```

| Agent | 接入方式 |
|-------|----------|
| cosh / Copilot Shell | `anolisa adapter enable tokenless cosh` |
| OpenClaw | `anolisa adapter enable tokenless openclaw` |
| Hermes | `anolisa adapter enable tokenless hermes` |
| Qoder | `anolisa adapter enable tokenless qoder` |
| Claude Code | `anolisa adapter enable tokenless claude-code` |
| Codex | `anolisa adapter enable tokenless codex` |
| DeepSeek Harness（dsh） | `anolisa adapter enable tokenless dsh --profile <profile>` |
| OpenCode | `anolisa adapter enable tokenless opencode` |
| Qwen Code | `anolisa adapter enable tokenless qwencode` |
| QwenPaw | `anolisa adapter enable tokenless qwenpaw` |
| WorkBuddy | 生命周期脚本（见下文） |

接入后重启对应的 Agent CLI 或 IDE。OpenClaw 还需要运行
`openclaw gateway restart`；如果安全检查拒绝 Plugin，请按照
[OpenClaw 接入说明](framework-integration.md#2-启用一个-adapter)处理。
DeepSeek Harness 必须提供 `<profile>`，并与 `dsh --profile <profile>` 使用的名称
保持一致。启用 Bundle 后应重启这个 profile。需要启用多个 profile 时，应在同一条
命令中重复传入 `--profile`。

```bash
anolisa adapter enable tokenless dsh \
  --profile web \
  --profile headless
```

后续每次 enable 或 re-enable 都会替换 receipt 记录的完整 profile 集合。每次都要
列出需要继续使用 Tokenless 的全部 profile。

OpenCode 可使用相同的 Adapter 命令。对于没有 ANOLISA 组件记录的 npm 安装，随附生命周期
脚本仍可作为替代方式。WorkBuddy 在本版本尚未注册到 `anolisa adapter enable`；请使用
[WorkBuddy 接入说明](framework-integration.md#workbuddy)中的随附生命周期脚本。

## 可选：不接入 Agent 测试压缩

需要在启用 Adapter 前单独确认 CLI 时，运行下面这组结果确定的检查：

```bash
printf '%s\n' \
  '{"status":"ok","data":{"name":"demo","items":[1,2,3]},"debug":{"trace":"verbose"},"metadata":null}' \
  | tokenless compress-response

tokenless stats list --limit 1
```

命令返回的仍是合法 JSON，其中 `debug` 和 `metadata` 会被省略。不包含可移除字段的
内容会原样返回且不记录。

## 平台适配性

| 平台 | anolisa CLI 安装 |
|------|------------------|
| Linux x86_64/aarch64 | 支持 |
| macOS Apple Silicon | 支持 |
| macOS x86_64 | 暂不支持 |
| Windows 或使用 musl 的 Linux（例如 Alpine） | 暂不支持 |

本页只提供 anolisa CLI 安装路径。需要从源码构建独立 CLI 时，请参阅
[用户手册 · 从源码构建独立 CLI](user-manual.md#从源码构建独立-cli)。

## 下一步

- [Python SDK](sdk.md)：通用与 AgentScope 两层及可运行示例
- [AgentScope SDK 集成](sdk/agentscope.md)：AgentScope 1.x、2.x 与 App 挂载
- [Agent 集成](framework-integration.md)：产品 Adapter 激活
- [用户手册](user-manual.md)：能力边界和文档导航
- [CLI 参考](cli-reference.md)：全部子命令和参数
- [效果度量](measuring-savings.md)：统计、双跑对比和 AgentSight/SLS
- [配置与数据隐私](configuration-and-privacy.md)：开关、存储和敏感数据
- [故障排查](troubleshooting.md)：常见错误、升级和卸载

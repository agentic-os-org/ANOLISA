# Agent Memory

[English](README.md)

面向 AI Agent 的 CMA 风格持久化文件系统记忆服务，基于 MCP 协议。提供沙箱化文件工具、BM25 + 向量混合检索、自动捕获/召回、git 版本控制和 tar.gz 快照。Agent Memory 是 [ANOLISA](../../README_zh.md) 的记忆组件。仅支持 Linux。

## 特性

- **文件形态记忆** — 通过 37 个 MCP 工具以文件系统语义读写记忆；命名空间隔离与路径沙箱（openat2 RESOLVE_BENEATH）
- **混合语义检索** — BM25 关键词 + 稠密向量嵌入，通过倒数排名融合（RRF）组合；时间衰减排序
- **自动捕获与召回** — 对话结束时自动提取观察，下一次 prompt 构建前注入相关记忆
- **记忆聚合** — 从会话审计日志中自动提取原子事实
- **版本控制与快照** — 可选 git 自动提交 + tar.gz 快照，支持文件级和挂载级回滚
- **安全** — 注入内容的 prompt 注入检测与密钥/PII 脱敏
- **跨会话任务** — 跨会话保存/恢复/关闭任务及完整上下文

## 快速开始

### 安装

```bash
# 推荐
anolisa install agent-memory

# 或通过 RPM（Alinux）
sudo yum install agent-memory
```

### OpenClaw 适配器

随包附带的插件（`memory-anolisa`）由
`/usr/share/anolisa/adapters/agent-memory/openclaw/scripts/install.sh` 部署，默认授予插件声明的能力。设置
`AGENT_MEMORY_ACCEPT_CAPABILITIES=0` 可拒绝授予同意——带门禁的宿主上安装将失败，直至交互式授予。设置
`AGENT_MEMORY_SAFE_INSTALL=1` 可在仍会传递 unsafe-install 覆盖参数的宿主上拒绝它。完整说明见[用户指南](../../docs/user-guide/zh/token-saving/agent-memory.md)。

```bash
bash /usr/share/anolisa/adapters/agent-memory/openclaw/scripts/install.sh
openclaw gateway restart
```

两个可选安装参数都从 `openclaw plugins install --help` 协商，但两个开关并不对称。只有宿主列出完整的 `--accept-capabilities` 时才传递它——当前宿主会列出，因此 `AGENT_MEMORY_ACCEPT_CAPABILITIES` 在这些宿主上仍然会改变 argv：取 `1` 时在 `openclaw plugins install <插件目录> --force` 之后追加 `--accept-capabilities`，取 `0` 时省略它，带同意门禁的宿主随后会拒绝这次安装。只有宿主仍声明 `--dangerously-force-unsafe-install` 有效时才传递该覆盖参数（OpenClaw 2026.6.1 及更早版本）。当前宿主把它标注为 deprecated no-op，两种取值下都不会收到它，因此 `AGENT_MEMORY_SAFE_INSTALL` 在这些宿主上不产生任何差别，安装期安全由运维自有的 `security.installPolicy` 决定。安装日志会说明命中的是哪一种情况。

### 集成（MCP 客户端）

添加到 MCP 配置（Claude Code、Cursor 等）：

```json
{
  "mcpServers": {
    "agent-memory": {
      "command": "/usr/bin/agent-memory",
      "args": [],
      "env": {
        "USER_ID": "alice",
        "MEMORY_PROFILE": "advanced"
      }
    }
  }
}
```

## 架构

单进程 Tokio 异步运行时，通过 stdio JSON-RPC 2.0 暴露 37 个 MCP 工具：

- **Tier A**（11 工具）：文件操作
- **Tier B**（6 工具）：结构化检索
- **Tier C**（7 工具）：治理（快照、版本控制、聚合）
- **主权**（13 工具）：关于、遗忘、同意、导入导出、任务、梦境合成

Profile 门控（basic/advanced/expert）按部署形态控制工具可见性。OpenClaw 适配器会把该档位
透传给子进程，但只接受 `basic`/`advanced`：`expert` 会隐藏其 memory 契约所依赖的 Tier B
工具，插件会拒绝加载。

## 环境要求

- Linux（x86_64 / aarch64）
- Rust ≥ 1.85（源码构建）
- Node.js ≥ 20 与 npm（源码构建 —— 打包内置的 OpenClaw adapter）
- 可选：用于向量检索的 embedding 提供方（OpenAI 或 Ollama）

## 许可证

Apache License 2.0 — 详见 [LICENSE](LICENSE)。

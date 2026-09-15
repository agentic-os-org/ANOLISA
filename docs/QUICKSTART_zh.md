# ANOLISA 快速入门

[English](QUICKSTART.md)

ANOLISA 是面向 AI Agent 工作负载的服务端操作系统层。只需安装一次 CLI，再按需
启用能带来第一个目标结果的能力。

## 安装 CLI

```bash
curl -fsSL https://get.agentic-os.sh | bash
export PATH="$HOME/.local/bin:$PATH"
```

如果已经安装 anolisa CLI，可以跳过这些命令。Alinux 4 用户也可通过
`sudo yum install anolisa` 安装 CLI。

## 选择第一个目标

### 减少 Agent Token 开销

安装 Tokenless，将它接入 Codex、Claude Code、Qoder、OpenClaw、Hermes、
Qwen Code 或 cosh，然后确认一条压缩前后的 Token 记录。

[开始三分钟 Tokenless 快速体验 →](user-guide/zh/token-saving/tokenless/QUICKSTART.md)

### 使用 Agent 原生终端

根据环境选择终端：

- [开始使用 cosh-ng](user-guide/zh/user-entrypoint/cosh-ng/QUICKSTART.md)——面向 AI 时代重构的终端
- [开始使用 Copilot Shell](user-guide/zh/user-entrypoint/copilot-shell/QUICKSTART.md)——可扩展的 Agent Shell

在 Alibaba Cloud Linux 4 上使用 RPM backend，让 dnf 选择匹配的系统库。
脚本只会为组件操作调用 `sudo`。

```bash
curl -fsSL https://get.agentic-os.sh | bash -s -- --component cosh-ng --backend rpm --install-mode system
export PATH="$HOME/.local/bin:$PATH"
```

`--cosh-ng` 仍可作为 `--component cosh-ng` 的简写。在 macOS arm64 上改用
`--backend raw --install-mode user`。

### 增加可观测性、安全或运行时控制

| 目标 | 开始位置 |
|------|----------|
| 观察 Agent 活动与 Token 使用 | [AgentSight](user-guide/zh/agent-observability/agentsight/README.md) |
| 增加安全策略 | [Agent Sec Core](user-guide/zh/agent-security/agent-sec-core/QUICKSTART.md) |
| 创建工作区恢复点 | [ws-ckpt](user-guide/zh/runtime/ws-ckpt.md) |
| 按需挂载 Skills | [SkillFS](user-guide/zh/runtime/skillfs.md) |
| 跨 Session 复用上下文 | [Agent Memory](user-guide/zh/token-saving/agent-memory.md) |
| 确定性调优内核参数 | [ktuner](user-guide/zh/user-entrypoint/ktuner.md) |

每个组件页面都会首先说明支持的平台和首选安装路径。仅支持 Linux 的组件必须在
Linux 上安装和运行。

## 检查安装状态

通过 CLI 查看当前机器和已经安装的组件：

```bash
anolisa env
anolisa list
anolisa status
```

Adapter 用于将已安装组件接入 Agent 框架。安装组件后，扫描可用的接入路径：

```bash
anolisa adapter scan
```

## 下一步

- [安装指南](user-guide/zh/installation.md)——平台支持、system mode、RPM 和所有组件安装命令
- [完整用户指南](user-guide/zh/README.md)——配置、运行和排查各项能力
- [anolisa CLI 参考](user-guide/zh/user-entrypoint/anolisa-cli.md)——生命周期与 Adapter 命令
- [故障排查](user-guide/zh/troubleshooting.md)——常见安装和运行问题
- [从源码构建](BUILDING_zh.md)——仅面向开发者构建

Qoder 插件发布方也可使用[独立插件打包器](../src/tokenless/packaging/qoder/README_zh.md)，分发前请阅读运行依赖和原文恢复限制。

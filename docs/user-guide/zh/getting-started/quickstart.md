# ANOLISA 快速入门

[English](../../en/getting-started/quickstart.md)

ANOLISA 是面向 AI Agent 工作负载的服务端操作系统层。只需安装一次 CLI，再按需
启用能带来第一个目标结果的能力。

AW Preview 包的安装步骤见[安装 AW](../aw/installation.md)。

## 安装 CLI

```bash
curl -fsSL https://get.agentic-os.sh | bash
export PATH="$HOME/.local/bin:$PATH"
```

如果已经安装 anolisa CLI，可以跳过这些命令。Alinux 4 用户也可通过
`sudo yum install anolisa` 安装 CLI。

## 选择第一个目标

### 减少 Agent Token 开销

Windows x64 用户可以构建 Tokenless 原生 npm 包，并接入
[千问办公本地模式](../tokenless/framework-integration.md#千问办公windows-x64-本地模式)。
参见 [Windows 构建说明](../../../developer-guide/zh/tokenless/windows.md#在-windows-上构建和测试)，这些软件包尚未发布。
上面的 anolisa CLI 安装方式适用于 Linux/macOS。

安装 Tokenless，将它接入 Codex、Claude Code、Qoder、OpenClaw、Hermes、
Qwen Code 或 cosh-ng，然后确认一条压缩前后的 Token 记录。

[安装并体验 Tokenless →](../tokenless/index.md)

### 使用 Agent 原生终端

从 [cosh-ng](../cosh-ng/index.md) 开始使用面向 AI 时代重构的终端。

在 Alibaba Cloud Linux 4 上使用 RPM backend，让 dnf 选择匹配的系统库。
脚本只会为组件操作调用 `sudo`。

```bash
curl -fsSL https://get.agentic-os.sh | bash -s -- --component cosh-ng --backend rpm --install-mode system
export PATH="$HOME/.local/bin:$PATH"
```

`--cosh-ng` 仍可作为 `--component cosh-ng` 的简写。在 macOS 11+ (arm64 / x86_64) 上改用
`--backend raw --install-mode user`。

### 增加可观测性、安全或运行时控制

| 目标 | 开始位置 |
|------|----------|
| 观察 Agent 活动与 Token 使用 | [AgentSight](../agentsight/index.md) |
| 增加安全策略 | [Agent Sec Core](../agent-sec-core/index.md) |
| 评估 Rust SkillSec 核心 | [SkillSec V2](../agent-sec-core/skillsec-v2.md) |
| 创建工作区恢复点 | [ws-ckpt](../ws-ckpt/index.md) |
| 按需挂载 Skills | [SkillFS](../skillfs/index.md) |
| 跨 Session 复用上下文 | [Agent Memory](../agent-memory/index.md) |
| 确定性调优内核参数 | [ktuner](../ktuner/index.md) |

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

- [镜像与云服务入口](images.md)——系统镜像、容器基础镜像、云服务入口与社区条目提交
- [安装指南](installation.md)——平台支持、system mode、RPM 和所有组件安装命令
- [完整用户指南](../index.md)——配置、运行和排查各项能力
- [anolisa CLI 参考](anolisa-cli.md)——生命周期与 Adapter 命令
- [故障排查](troubleshooting.md)——常见安装和运行问题
- [从源码构建](building.md)——准备工具链并构建组件
- [测试](testing.md)——验证源码改动并了解测试覆盖范围

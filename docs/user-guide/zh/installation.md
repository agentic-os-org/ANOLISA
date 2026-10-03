# 安装指南

[English](../en/installation.md)

本指南介绍如何安装 ANOLISA CLI、所需组件和适配器。

---

## 第一步 安装 ANOLISA CLI

`anolisa` CLI 是管理所有 ANOLISA 组件的统一入口。

### 方式 A 安装脚本（推荐）

```bash
curl -fsSL https://get.agentic-os.sh | bash
```

同一个脚本准备好 CLI 后，可以立即管理一个已发布组件。参数使用 ANOLISA
registry 中的组件名，并指定 backend 和范围。默认 backend 是 raw。在
Alibaba Cloud Linux 4 上通过 RPM backend 安装 cosh-ng，让 dnf 选择匹配的
系统库。

```bash
curl -fsSL https://get.agentic-os.sh | bash -s -- --component cosh-ng --backend rpm --install-mode system
export PATH="$HOME/.local/bin:$PATH"
```

`--cosh-ng` 是 `--component cosh-ng` 的简写。组件、backend 和平台检查仍由
CLI 负责，安装脚本不维护第二份组件白名单。显式使用 system 模式时，脚本只为
组件操作调用 `sudo`，CLI 仍安装在当前用户目录。未指定 `--install-mode` 时，
范围沿用 ANOLISA 的 euid 默认规则。

脚本默认把 CLI 安装到 `~/.local/bin`，但 shell 会运行 `PATH` 中最先出现的
`anolisa`。如果旧的 npm 或 Homebrew 安装排在前面，安装器会同时报告两个
路径和版本，并且不会输出 `done`。脚本不会删除其他渠道的文件。可以把独立
安装目录放到最前面，再刷新当前 shell 的命令缓存。

```bash
export PATH="$HOME/.local/bin:$PATH"
hash -r
```

请把这条 `export` 写入 Bash 或 Zsh 实际读取的配置文件。fish 用户可以执行
`fish_add_path --move "$HOME/.local/bin"`。如果选择移除旧安装，请通过对应渠道执行
`npm uninstall -g @anolisa/cli` 或 `brew uninstall anolisa`。

### 方式 B YUM（Alinux）

```bash
sudo yum install anolisa
```

安装后验证版本。

```bash
anolisa --version
```

如果当前 CLI 较旧，先根据它的安装来源完成自更新，再安装使用新版 raw
包契约的组件。

```bash
# 通过 get.agentic-os.sh 安装的 CLI
anolisa update self

# 由 RPM 管理的 CLI
sudo anolisa update self
```

AgentSecCore 要求 `anolisa` 0.2.17 或更高版本。CLI 无法安全读取契约时，
会先给出更新提示并停止安装，不会改动主机。

---

## 第二步 环境检测

运行环境检查，识别系统能力。

```bash
anolisa env
```

命令会显示以下信息。

- 操作系统和架构
- 可用文件系统（btrfs 用于 ws-ckpt）
- FUSE 可用性（用于 skillfs）
- 已安装的 Agent 运行时（cosh、OpenClaw、Hermes）
- 内核特性（eBPF 用于 agentsight）

---

## 第三步 安装组件

根据需要逐个安装组件。

```bash
anolisa install <component>
```

### 可用组件

| 组件 | 说明 | 支持的模式 |
|------|------|------------|
| `cosh` | Copilot Shell AI 终端助手 | user、system |
| `cosh-ng` | AI 原生终端与 Agent 运行时（实验阶段） | system（Linux）、user 或 system（macOS 11+ (arm64 / x86_64)） |
| `os-skills` | 系统管理与 DevOps 技能 | user、system |
| `tokenless` | Token 优化（压缩） | user、system |
| `ws-ckpt` | 工作区快照/回滚 | **system** |
| `skillfs` | FUSE 虚拟技能文件系统 | **system** |
| `agent-memory` | 基于 MCP 的持久化记忆 | user、system |
| `agentsight` | eBPF 追踪与 Dashboard | **system** |
| `sec-core` | 本地安全运行时、scanner 和 adapter | **system** |

> **注意** 仅支持 system 模式的组件需要 `sudo`，并且必须显式选择 system 范围。
> ```bash
> sudo anolisa --install-mode system install agentsight
> ```

在 Alibaba Cloud Linux 4 上，通过 RPM backend 把 cosh-ng 安装到 system
范围，随后运行 `cosh` 进入终端。

```bash
sudo anolisa --install-mode system install cosh-ng --backend rpm
cosh
```

公共安装脚本可以合并 CLI 引导和组件安装。

```bash
curl -fsSL https://get.agentic-os.sh | bash -s -- --component cosh-ng --backend rpm --install-mode system
export PATH="$HOME/.local/bin:$PATH"
```

在 macOS 11+ (arm64 / x86_64) 上，cosh-ng raw 包使用独立的 user scope 契约。

```bash
curl -fsSL https://get.agentic-os.sh | bash -s -- --component cosh-ng --backend raw --install-mode user
export PATH="$HOME/.local/bin:$PATH"
```

ANOLISA CLI 使用组件名 `sec-core`。Alinux 的 RPM 包名仍然是
`agent-sec-core`。

```bash
sudo anolisa --install-mode system install sec-core

# 如果直接安装 RPM，配置 adapter 前先让 ANOLISA 记录该组件
sudo yum install anolisa agent-sec-core
sudo anolisa --install-mode system adopt sec-core
```

随后进入[第四步](#第四步-配置适配器)，由拥有目标 Agent 配置的用户执行
`anolisa adapter enable sec-core <framework>`。

### 安装全部组件

```bash
anolisa install --all
```

### YUM 替代方式（Alinux）

每个组件也可通过 YUM 安装。请在同一条命令中安装 system CLI，避免 `sudo`
依赖用户目录中的 `PATH`。直接安装 RPM 不会生成 ANOLISA 状态记录，继续使用
组件生命周期或 adapter 命令前，需要先执行 `adopt`。

```bash
sudo yum install anolisa <rpm-package>
sudo anolisa --install-mode system adopt <component>
```

---

## 第四步 配置适配器

适配器把组件接入特定 Agent 框架。安装组件后再启用适配器。

```bash
anolisa adapter scan
anolisa adapter enable <component> [framework]
```

### 示例

```bash
# Tokenless cosh hook
/usr/share/tokenless/scripts/install.sh --cosh

# Tokenless OpenClaw 插件
/usr/share/tokenless/scripts/install.sh --openclaw

# ws-ckpt OpenClaw 插件（需要 OpenClaw >= 2026.2.13）
ws-ckpt plugin install --runtime openclaw

# ws-ckpt Hermes 插件
ws-ckpt plugin install --runtime hermes

# AgentSecCore OpenClaw 插件
anolisa adapter enable sec-core openclaw
```

system 安装负责管理组件文件。adapter 需要写入当前用户的 Agent 配置，
普通的 user scope 框架安装不需要 `sudo`。

---

## 第五步 启动常驻服务

安装组件和启动常驻服务是两个动作。AgentSight 会安装
`agentsight.service` 及其 enforcer 依赖，两个单元默认都不启用。
主机准备开始采集时，再启动主服务。

```bash
sudo systemctl enable --now agentsight.service
sudo systemctl status agentsight.service
```

主服务以 root 身份运行 eBPF trace 和 Dashboard，数据保存在仅 root 可读的
`/var/log/sysak/.agentsight`。查询服务数据时也要使用 `sudo`。如需前台排查，
先停止服务，再在两个终端中分别以 root 身份启动 tracer 和 server。

```bash
sudo systemctl stop agentsight.service

# 终端 1
sudo agentsight trace

# 终端 2
sudo agentsight serve
```

---

## 第六步 验证安装

查看所有已安装组件的状态。

```bash
anolisa status
```

运行内置诊断工具。

```bash
anolisa doctor
```

---

## 卸载

移除指定组件。

```bash
anolisa uninstall <component>
```

公共安装脚本也可以接收已安装的组件名进行卸载。它会先刷新 stable CLI，再把
操作交给 `anolisa uninstall`。

```bash
curl -fsSL https://get.agentic-os.sh | bash -s -- --component cosh-ng --install-mode system --uninstall
```

当前没有批量卸载命令。先列出安装记录，再逐个卸载目标组件，以便分别确认其
权限来源和系统软件包移除策略。

```bash
anolisa list --installed
anolisa uninstall <component>
```

---

## 升级

更新指定组件。

```bash
anolisa update <component>
```

同时更新指定组件和脚本安装的 stable CLI。

```bash
curl -fsSL https://get.agentic-os.sh | bash -s -- --component cosh-ng --install-mode system --upgrade
```

更新所有已安装组件。

```bash
anolisa update all
```

`update all` 只更新已记录的组件，不更新 CLI。脚本安装的 CLI 使用
`anolisa update self`，RPM 管理的 CLI 使用 `sudo anolisa update self`。

---

## 下一步

- [anolisa CLI 参考](user-entrypoint/anolisa-cli.md)
- [cosh-ng 快速开始](user-entrypoint/cosh-ng/QUICKSTART.md)
- [Copilot Shell](user-entrypoint/copilot-shell/QUICKSTART.md)
- [故障排查](troubleshooting.md)

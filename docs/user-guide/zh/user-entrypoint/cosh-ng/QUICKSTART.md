# cosh-ng 快速开始

[English](../../../en/user-entrypoint/cosh-ng/QUICKSTART.md)

cosh-ng 默认启动 Enhanced Assisted 模式。bash 或 zsh 仍可交互，Cosh 也可能
把自然语言请求路由给 Agent。要求完全不加载 Cosh Hook 的会话可在启动时显式
选择 Native 集成。

## 1. 安装

在 Alibaba Cloud Linux 4 上安装 ANOLISA CLI，再通过 RPM backend 把
cosh-ng 安装到 system 范围。

```bash
curl -fsSL https://get.agentic-os.sh | bash
export PATH="$HOME/.local/bin:$PATH"
sudo "$HOME/.local/bin/anolisa" --install-mode system install cosh-ng --backend rpm
```

公共安装脚本可以合并 CLI 和组件安装，并且只在执行组件操作时请求 `sudo`。

```bash
curl -fsSL https://get.agentic-os.sh | bash -s -- --cosh-ng --backend rpm --install-mode system
export PATH="$HOME/.local/bin:$PATH"
```

在 macOS arm64 上改用 user 范围：

```bash
curl -fsSL https://get.agentic-os.sh | bash -s -- --cosh-ng --backend raw --install-mode user
export PATH="$HOME/.local/bin:$PATH"
```

Alibaba Cloud Linux 4 用户也可以直接安装 RPM。

```bash
sudo yum install cosh-ng
```

验证两个用户命令：

```bash
cosh --version
cosh-cli --version
```

修改软件包和服务通常需要 root 权限；工作区快照命令还需要运行中的 `ws-ckpt` 守护进程。

当前发布的 Linux raw 契约无法覆盖所有已路由的发行版，因此不作为推荐的
Linux 安装路径。raw 包支持 macOS arm64，但依赖 Linux 的软件包和服务操作
不可用。源码构建仅供贡献者使用，请参阅
[开发者入门指南](../../../../developer-guide/zh/cosh-ng/getting-started.md)。

## 2. 启动终端

在 Agent 要处理的项目或系统目录中启动 `cosh`：

```bash
cd your-project
cosh
```

默认的 Enhanced Assisted 保持 Shell 原生提示符外观，同时输入可能在 Shell
执行前被分类和路由。

```text
user@host:~/project$ git status
user@host:~/project$ 分析上次部署失败的原因
```

在空提示符按 `Shift+Tab` 可切换到 Enhanced Shell-only。此时普通输入交给
Shell，但仍可获得命令执行后的洞察。再次按下即可返回 Assisted。可通过
`shell.status_symbols` 或 `COSH_SHELL_STATUS_SYMBOLS=1` 开启提示符上方的
可选 `◇ `/`◌ ` 状态行，默认关闭。

要求不加载 Cosh Hook、不观察也不提供洞察时，显式启动 Native。

```bash
COSH_SHELL_INTEGRATION=native cosh
```

Native 和 Enhanced 集成在启动时确定，二者切换需要重新启动 `cosh`；Assisted
与 Shell-only 子状态可在会话中直接切换。

操作需要同意时，cosh 会先显示审批卡片或问题卡片。

Enhanced Assisted 中的常用起始命令如下。

```text
/auth
/help
/status
/mode approval recommend
/session list
```

`/auth` 用于选择或更新模型提供商认证，`/help` 查看斜杠命令，`/status` 查看运行时和会话状态，`/mode approval recommend` 在每次 Agent 工具调用前请求确认，`/session list` 列出当前工作区可恢复的会话。

使用 `/session list --all` 可同时列出其他工作区创建的会话；恢复会话时，请先进入创建它的工作区。

## 3. 复用技能

列出并查看当前工作区可用的 Skills：

```text
/skills list
/skills detail service-health
```

工作区、用户、扩展和系统 Skill 目录会按优先级合并。搜索顺序和文件格式见 [Skills](core/skills.md)。

## 4. 继续完成任务

| 目标 | 继续阅读 |
|---|---|
| 控制审批和安全策略 | [工具审批](shell/approval.md) |
| 恢复或压缩会话 | [会话恢复](shell/session-recovery.md) |
| 选择模型并完成认证 | [模型提供商](core/providers.md) |
| 接入其他服务提供的工具 | [接入 MCP 服务](mcp.md) |
| 自动处理软件包、服务、快照或审计工作 | [结构化 OS CLI](cli/overview.md) |
| 集成其他前端 | [无界面模式](core/headless-mode.md) |

[完整用户手册](README.md)按任务整理了其余内容。

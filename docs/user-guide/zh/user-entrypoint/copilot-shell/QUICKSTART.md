# 快速开始

> 欢迎使用 Copilot Shell！

本指南帮助你在几分钟内上手 AI 驱动的编程和系统管理。完成后，你将了解
如何使用 Copilot Shell 完成常见的开发和运维任务。

## 前提条件

确保你已准备好：

- 一台 **Alibaba Cloud Linux (Alinux)** 机器上的终端
- 一个代码项目或待管理的系统
- 已配置好支持的认证方式之一（参见下方[认证](#第-2-步认证)）

## 第 1 步：安装

### 从源码构建

需要 [Node.js 20+](https://nodejs.org/download)，可通过 `node -v` 检查版本。

```bash
cd deprecated/copilot-shell
make build
```

构建完成后，打包产物位于 `dist/cli.js`。

## 第 2 步：认证

首次启动 Copilot Shell 时，需要配置认证：

```bash
cosh
```

在会话内使用 `/auth` 命令选择认证方式：

```bash
/auth
```

### 支持的认证方式

| 认证方式 | 说明 |
|----------|------|
| 阿里云认证 | 默认方式。ECS 上自动检测并启动 Web 认证（浏览器链接 + 二维码）；非 ECS 环境直接输入 AK/SK。 |
| OpenAI 兼容 | 支持 DashScope、DeepSeek、Kimi、GLM、MiniMax 或任何 OpenAI 兼容端点 |

> [!TIP]
>
> 后续切换账号或认证方式时，使用会话内的 `/auth` 命令。

## 第 3 步：开始第一次会话

在任意项目目录下启动 Copilot Shell：

```bash
cd /path/to/your/project
cosh
```

你会看到欢迎界面和会话信息。输入 `/help` 查看所有可用命令。

> [!NOTE]
>
> 也可以使用别名 `co` 或 `copilot` 来代替 `cosh`。

## 与 Copilot Shell 对话

### 提问

Copilot Shell 会分析你的文件并回答问题。你可以询问代码库：

```
解释一下这个项目的目录结构
```

或询问系统状态：

```
显示当前磁盘使用情况和内存占用最高的进程
```

> [!NOTE]
>
> Copilot Shell 会按需读取文件，无需手动添加上下文。它还内置了
> 用于系统管理任务的 OS 级技能。

### 进行代码修改

尝试一个简单的编码任务：

```
在主文件中添加一个 hello world 函数
```

Copilot Shell 会：

1. 找到相应的文件
2. 展示提议的更改
3. 请求你的确认
4. 执行修改

> [!NOTE]
>
> Copilot Shell 在修改文件前总是请求许可。你可以逐个审批，
> 也可以为当前会话启用「全部接受」模式。

### 系统管理

Copilot Shell 集成了 OS 级技能，用于常见运维任务：

```
检查是否有失败的 systemd 服务
```

```
分析 nginx 访问日志，找出最近一小时访问量最高的 10 个 IP
```

```
设置一个每天凌晨 3 点清理 /tmp 的 cron 任务
```

### 使用 Git

Git 操作变成了自然语言对话：

```
我改了哪些文件？
```

```
用一条描述性的信息提交我的更改
```

```
创建一个名为 feature/quickstart 的新分支
```

```
帮我解决合并冲突
```

### 修 Bug 或添加功能

用自然语言描述你的需求：

```
为用户注册表单添加输入验证
```

或修复已有问题：

```
有个 bug：用户可以提交空表单，帮我修复
```

Copilot Shell 会：

- 定位相关代码
- 理解上下文
- 实现修复方案
- 运行可用的测试

### 进入交互式 Shell

使用 `/bash` 命令从 Copilot Shell 内进入交互式 shell：

```
/bash
```

输入 `exit` 返回 Copilot Shell 会话。

## 常用命令

| 命令 | 功能 | 示例 |
|------|------|------|
| `cosh` | 启动 Copilot Shell | `cosh` |
| `/auth` | 切换认证方式 | `/auth` |
| `/hooks list` | 查看所有已注册 hooks 及状态 | `/hooks list` |
| `/help` | 显示帮助 | `/help` 或 `/?` |
| `/bash` | 进入交互式 shell | `/bash` |
| `/model` | 切换模型 | `/model` |
| `/compress` | 用摘要替换聊天历史以节省 token | `/compress` |
| `/clear` | 清屏 | `/clear`（快捷键 `Ctrl+L`） |
| `/theme` | 切换主题 | `/theme` |
| `/language` | 查看或切换语言设置 | `/language` |
| → `ui [lang]` | 设置界面语言 | `/language ui zh-CN` |
| → `output [lang]` | 设置 LLM 输出语言 | `/language output Chinese` |
| `/quit` | 退出 | `/quit` 或 `/exit` |

**使用快捷键提高效率**

- 按 `?` 查看所有快捷键
- 使用 Tab 补全命令
- 按 ↑ 查看历史命令
- 输入 `/` 查看所有 slash 命令

## 获取帮助

- **在 Copilot Shell 内**：输入 `/help` 或直接问"怎么做……"
- **问题反馈**：在项目仓库提交 Issue

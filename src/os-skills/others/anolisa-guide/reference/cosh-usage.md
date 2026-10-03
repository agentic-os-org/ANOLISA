> **爬取时间**: 2026-06-17 16:29:16
> **原文链接**: https://help.aliyun.com/zh/alinux/how-to-use-alibaba-cloud-linux-4-agentic-edition
> **文档更新**: 2026-05-28T10:06:03+08:00

---

Alibaba Cloud Linux 4 Agentic Edition（ANOLISA）以 Copilot Shell（cosh）替代传统 bash 作为系统登录后的第一入口，支持自然语言与命令双模交互，让人类用户和智能体都能以最自然的方式调用系统能力。本文介绍了 Copilot Shell 的交互设计理念、基础使用方法（自然语言模式、命令模式、YOLO 模式、文件引用）及完整的斜杠命令与快捷键参考。

## **交互入口**

Alibaba Cloud Linux 4 Agentic Edition（ANOLISA）的交互式 Shell 默认为 Copilot Shell（cosh），替代bash 作为系统登录后的第一入口。

Copilot Shell 是与 ANOLISA 交互的核心组件，针对不同的访问对象，具备不同的交互特点。

-   对人类用户：它是一个可以用自然语言交流的智能终端
    
-   对智能体：它是调用系统能力的标准接口
    

### Alibaba Cloud Linux 4 Agentic Edition（ANOLISA） 交互设计

Alibaba Cloud Linux 4 Agentic Edition（ANOLISA） 的交互设计遵从「双模交互」，主要有以下特点：

-   **自然语言模式**：用户直接用口语化语言描述意图，系统将利用大模型能力，转化为可执行的系统命令
    
-   **命令模式**：如需手动执行简单命令，用户无须离开 Copilot Shell 即可快速执行。同时 Copilot Shell 支持 bash 回退，可调起全功能交互式 bash 进行传统命令执行操作
    

两种模式可以自由混合使用。

### Copilot Shell 与传统 Shell 的关系

在 Alibaba Cloud Linux 4 Agentic Edition（ANOLISA） 中，Copilot Shell 保留 bash 兼容性的同时，增加了自然语言理解、Skill 调用、Agent 框架集成等能力。将复杂系统级能力抽象成自然语言，更适合人类用户和 Agent 智能体使用。

## 基础使用

进入 Alibaba Cloud Linux 4 Agentic Edition （ANOLISA）后，将直接进入 Copilot Shell，可以直接开始输入自然语言或系统命令。首次运行将自动引导配置大模型后端。

### 自然语言模式

直接用中文或英文描述你的意图，系统会自动理解并执行：

```
> 查看当前系统的内存使用情况
> 帮我安装 nginx 并配置为开机自启
> 帮我部署Openclaw
```

系统 shell 会将自然语言转化为对应的系统操作，执行前会显示计划并等待确认（取决于审批模式设置，详见「配置管理 - 审批模式」）。

### 命令模式

`/bash` 回车后进入shell命令行模式，输入 'exit' 或按 Ctrl+D 返回

或者可以使用 `!` 前缀快速执行 Shell 命令（使用 esc 退出）：

```
> !git status
> !top -bn1 | head -20
```

### YOLO 模式

默认情况下，系统 shell 在执行操作前会要求用户确认。如果希望全自动执行、无需手动确认，可以开启 YOLO 模式：

```
# 通过命令切换
> /approval-mode yolo
```

> **安全提示**：YOLO 模式会跳过所有操作确认，建议仅在隔离的测试环境中使用。生产环境请使用 Default 或 Plan 模式。详见「配置管理 - 审批模式」。

### 使用 @ 引用文件

在对话中使用 `@` 引入文件作为上下文：

```
> @/etc/nginx/nginx.conf 帮我检查这个配置有没有问题
> @/var/log/messages 最近有没有报错？
```

* * *

## Copilot Shell 命令参考

### 命令分类总览

| **前缀** | **类型** | **功能** | **示例** |
| --- | --- | --- | --- |
| `/` | 斜杠命令 | 元操作控制 | `/help`、`/clear` |
| `@` | At 命令 | 注入文件内容到上下文 | `@src/main.py` |
| `!` | 感叹号命令 | 直接执行 Shell 命令 | `!git status` |

### 斜杠命令一览

#### 会话与项目管理

| **命令** | **说明** | **用法** |
| --- | --- | --- |
| `/init` | 分析当前目录，创建初始上下文 | `/init` |
| `/summary` | 从会话历史生成项目摘要 | `/summary` |
| `/compress` | 将聊天历史替换为摘要以节省 Token | `/compress` |
| `/resume` | 恢复之前的会话 | `/resume` |
| `/restore` | 将文件恢复到工具执行前的状态 | `/restore` 或 `/restore <ID>` |

#### 工具与模型管理

| **命令** | **说明** | **用法** |
| --- | --- | --- |
| `/model` | 切换当前会话模型 | `/model` |
| `/auth` | 切换认证方式 | `/auth` |
| `/bash` | 直接执行 bash 命令，不经过 AI 解析 | `/bash` |
| `/tools` | 显示可用工具列表 | `/tools`、`/tools desc` |
| `/skills` | 列出并运行 Skills | `/skills`、`/skills <name>` |
| `/mcp` | 列出 MCP 服务器和工具 | `/mcp`、`/mcp desc` |
| `/approval-mode` | 更改审批模式 | `/approval-mode <mode>` |
| `/memory` | 管理 AI 指令上下文 | `/memory add 重要信息` |
| `/extensions` | 列出已激活的扩展 | `/extensions` |
| `/clawhub` | 对接clawhub skill市场 | `/clawhub` |

#### 界面与工作区控制

| **命令** | **说明** | **用法** |
| --- | --- | --- |
| `/clear` | 清除终端屏幕 | `/clear`（快捷键 Ctrl+L） |
| `/theme` | 切换视觉主题 | `/theme` |
| `/vim` | 切换 Vim 编辑模式 | `/vim` |
| `/directory` | 管理多目录工作区 | `/dir add ./src,./tests` |
| `/settings` | 打开设置编辑器 | `/settings` |

#### 语言设置

| **命令** | **说明** | **用法** |
| --- | --- | --- |
| `/language` | 查看/更改语言设置 | `/language` |
| `/language ui` | 设置 UI 界面语言 | `/language ui zh-CN` |
| `/language output` | 设置 LLM 输出语言 | `/language output Chinese` |

内置 UI 语言：`zh-CN`、`en-US`

#### 信息与帮助

| **命令** | **说明** | **用法** |
| --- | --- | --- |
| `/help` | 显示帮助信息 | `/help` 或 `/?` |
| `/about` | 显示版本信息 | `/about` |
| `/stats` | 显示会话统计信息 | `/stats` |
| `/bug` | 提交问题报告 | `/bug 按钮点击无响应` |
| `/copy` | 复制最后输出到剪贴板 | `/copy` |
| `/quit` | 退出 cosh | `/quit` 或 `/exit` |

### 快捷键

| **快捷键** | **功能** | **说明** |
| --- | --- | --- |
| Shift+Tab | 切换审批模式 | 快速切换当前审批级别 |
| Esc | 中断当前操作 | 停止正在执行的任务 |
| Ctrl+L | 清屏  | 等同于 `/clear` |
| Ctrl+T | 切换工具描述 | MCP 工具管理 |
| Ctrl+C ×2 | 退出确认 | 安全退出机制 |
| Ctrl+Z | 撤销输入 | 文本编辑 |
| Ctrl+Shift+Z | 重做输入 | 文本编辑 |

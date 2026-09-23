# Copilot Shell

[English](README.md)

AI 驱动的终端开发助手，支持代码理解、任务自动化和系统管理。Copilot Shell 是 [ANOLISA](../../README.md) 的旧版组件，基于上游 [Qwen Code](https://github.com/QwenLM/qwen-code) v0.9.0 构建。

本组件已进入弃用阶段，新增开发请使用 [cosh-ng](../../src/cosh-ng/README_zh.md)。过渡期间保留现有命令、安装、构建和发布标识。修改前请阅读[根 AGENTS.md](../../AGENTS.md)。

## 特性

- **自然语言编程** — 用自然语言描述即可修改代码、实现功能或修复 Bug。
- **代码分析与导航** — 理解整个项目结构，回答代码相关问题。
- **多工具编排** — 在单次会话中集成文件、Shell、搜索、Web、LSP 和 MCP 工具。
- **交互式 Shell** — `/bash` 命令进入交互式 Shell，输入 `exit` 返回。
- **技能系统** — 本地 + 远程技能发现，支持优先级回退（项目 > 用户 > 扩展 > 远程）。
- **Hooks 系统** — PreToolUse 事件，在工具调用前拦截处理。
- **Git 工作流自动化** — 自动化提交、分支创建、冲突解决和发布说明生成。
- **多认证方式支持** — 阿里云认证（ECS RAM Role 网页认证或 AK/SK）、Qwen OAuth、Custom Provider（DashScope、DeepSeek、Kimi、GLM、MiniMax 及任意 OpenAI 兼容接口）。
- **PTY 模式** — 完整的伪终端支持，包括 `sudo` 命令。
- **可扩展** — 通过 MCP 服务器和自定义技能扩展功能。

## 快速开始

### RPM 安装

```bash
sudo yum install copilot-shell
```

### 从源码构建

```bash
cd deprecated/copilot-shell
make build
```

### 运行

```bash
make start

# 交互模式
cosh

# 或使用其他别名
co
copilot
```

### 认证

```bash
# 阿里云认证（默认）
# - ECS 上：自动检测环境，打开浏览器或展示二维码进行网页授权
# - 非 ECS：直接输入 AK/SK
cosh    # 按照屏幕提示操作
```

> **注：** 支持复用openclaw的模型配置。

## 架构

Copilot Shell 采用 npm workspaces 的 monorepo 布局：

| 包                    | 说明                                            |
| --------------------- | ----------------------------------------------- |
| `packages/cli`        | 终端 UI 层 — 输入处理、命令解析、Ink/React 渲染 |
| `packages/core`       | 后端核心 — AI 模型通信、提示词构建、工具编排    |
| `packages/test-utils` | 共享测试工具                                    |

## 开发

### 前置要求

- Node.js >= 20.0.0
- npm（随 Node.js 一起安装）

### 常用命令

```bash
make install          # 安装依赖
make build            # 构建所有包
make test             # 运行单元测试
make lint             # ESLint 检查
make format           # Prettier 格式化
```

> **注意：** `make install`（即 `npm install`）会自动初始化 husky pre-commit 钩子。每次提交时，暂存文件会自动执行 Prettier 格式化和 ESLint 检查。如需跳过，可使用 `git commit --no-verify`。

### RPM 打包

```bash
make rpm
```

## 配置

Copilot Shell 使用分层配置系统（按优先级从高到低）：

1. 命令行参数
2. 环境变量
3. 项目配置（`.copilot-shell/settings.json`）
4. 用户配置（`~/.copilot-shell/settings.json`）
5. 系统配置
6. 默认值

## 文档

- [快速入门](../../docs/user-guide/zh/user-entrypoint/copilot-shell/QUICKSTART.md)
- [贡献指南](CONTRIBUTING.md)

## 许可证

Apache License 2.0 — 详见 [LICENSE](../../LICENSE)。

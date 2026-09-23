# 架构概述

Copilot Shell 是一个终端内的 AI 编程助手，采用 TypeScript 编写，
以 npm monorepo 形式组织代码。

## 仓库结构

```
deprecated/copilot-shell/
├── packages/
│   ├── cli/          # 命令行入口和 TUI 层
│   ├── core/         # 核心引擎（模型、工具、会话管理）
│   └── test-utils/   # 测试辅助工具
├── scripts/          # 构建与发布脚本
├── integration-tests/ # 端到端集成测试
├── hooks/            # 内置 hook 脚本
└── eslint-rules/     # 自定义 ESLint 规则
```

## 包职责

### `@copilot-shell/cli`

命令行入口层，负责：

- 解析 CLI 参数（`yargs`）
- 渲染交互式 TUI（`ink` + React）
- Slash 命令注册与派发
- 用户输入/输出流处理
- 扩展和技能的发现与加载

### `@copilot-shell/core`

核心引擎，负责：

- **模型适配**：统一 OpenAI / 阿里云 DashScope 等后端
- **工具系统**：工具定义、权限管理、执行调度
- **会话管理**：对话历史、上下文压缩（Compact）、检查点
- **Hook 运行时**：事件触发、脚本执行、结果聚合
- **MCP 客户端**：stdio / SSE 两种传输协议
- **配置系统**：多层配置合并（系统 > 用户 > 项目 > 默认）
- **安全**：沙箱集成、工具审批策略
- **可观测性**：OpenTelemetry 指标 / 追踪 / 日志

### `@copilot-shell/test-utils`

测试共享工具，提供 mock 模型、mock MCP 服务器等测试基础设施。

## 关键设计决策

### 分层配置

配置采用四层优先级体系：

```
系统设置 (/etc/copilot-shell/settings.json)       ← 管理员强制覆盖（最高）
  ↓
项目级 (.copilot-shell/settings.json)
  ↓
用户级 (~/.copilot-shell/settings.json)
  ↓
系统默认 (/etc/copilot-shell/system-defaults.json) ← 最低
```

数组字段采用**替换**策略，对象字段采用**浅合并**策略。

### Agent Loop

核心循环流程：

```
用户输入 → UserPromptSubmit hooks
  → BeforeModel hooks → LLM 请求 → AfterModel hooks
  → BeforeToolSelection hooks → 工具选择
  → PreToolUse hooks → 工具执行 → PostToolUse hooks
  → Stop hooks → 输出
```

每个阶段都有对应的 Hook 事件，允许外部脚本介入控制流。

### 模型适配层

通过统一的 `ModelProvider` 接口适配多家模型后端：

- 请求/响应格式标准化
- 流式输出统一处理
- Token 计数和用量统计
- 认证令牌自动刷新

### 工具权限模型

四级审批模式：

| 模式 | 行为 |
|------|------|
| `plan` | 所有工具需确认 |
| `default` | 仅文件修改和 shell 需确认 |
| `auto-edit` | 仅 shell 需确认 |
| `yolo` | 全部自动批准 |

白名单（`allowedTools`）和排除列表（`excludeTools`）提供细粒度控制。

## 技术栈

| 层次 | 技术 |
|------|------|
| 运行时 | Node.js ≥ 20 |
| 语言 | TypeScript (ESM) |
| 构建 | esbuild |
| TUI | ink (React) |
| 测试 | vitest |
| 格式化 | Prettier |
| 代码检查 | ESLint |
| 包管理 | npm workspaces |

## 目录约定

| 路径 | 用途 |
|------|------|
| `~/.copilot-shell/` | 用户数据目录（配置、会话、技能） |
| `.copilot-shell/` | 项目级配置目录 |
| `/etc/copilot-shell/` | 系统级配置 |
| `~/.copilot-shell/extensions/` | 已安装扩展 |
| `~/.copilot-shell/skills/` | 用户级技能 |

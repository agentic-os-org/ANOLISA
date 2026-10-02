# 工具与审批模式

Copilot Shell 通过内置工具执行文件操作、Shell 命令、搜索等任务。
本文介绍工具系统和审批模式的使用方法。

## 内置工具

Copilot Shell 提供以下核心工具：

| 工具 | 功能 |
|------|------|
| 文件读写 | 读取、创建、编辑文件 |
| Shell 执行 | 运行 Shell 命令 |
| 代码搜索 | 基于 ripgrep 的全文搜索 |
| 文件搜索 | 按名称模式查找文件 |
| 目录列表 | 列出目录内容 |
| Web 搜索 | 联网搜索信息 |
| MCP 工具 | 通过 MCP 协议调用外部工具 |

使用 `/tools` 命令查看当前会话中所有可用工具。

## 审批模式

审批模式决定 Copilot Shell 执行工具前是否需要用户确认。通过 `/approval-mode`
命令或 `tools.approvalMode` 配置项设置。

### plan

只生成操作计划，不执行任何工具调用。适用于查看 AI 的决策思路。

### default（默认）

每次工具调用前都会请求确认。用户可以：

- 按 `y` 或 Enter 确认执行
- 按 `n` 拒绝
- 按 `a` 切换到本次会话全部接受

### auto-edit

自动批准文件编辑操作，其他操作（如 Shell 命令）仍需确认。
适合信任 AI 的代码修改能力但希望控制外部命令执行的场景。

### yolo

自动批准所有工具调用，不需要任何确认。

> [!WARNING]
>
> `yolo` 模式会跳过所有安全确认。建议仅在受控环境或配合
> sandbox hooks 使用。也可通过 `cosh --yolo` 或 `cosh -y` 启动时指定。

## 工具过滤

### 白名单

设置特定工具免确认执行：

```json
{
  "tools": {
    "allowed": ["ReadFile", "ListDir", "GrepSearch"]
  }
}
```

白名单中的工具在 `default` 模式下也会自动执行，无需确认。

### 排除工具

禁止特定工具被调用：

```json
{
  "tools": {
    "exclude": ["WebSearch"]
  }
}
```

被排除的工具对 AI 不可见，不会出现在工具列表中。

## Shell 工具配置

### 交互式 Shell（PTY）

启用后，Shell 命令通过伪终端执行，支持 `sudo`、交互式程序等：

```json
{
  "tools": {
    "shell": {
      "enableInteractiveShell": true
    }
  }
}
```

### 输出显示

```json
{
  "tools": {
    "shell": {
      "showColor": true,
      "pager": "cat"
    }
  }
}
```

## 工具输出截断

当工具输出过大时，Copilot Shell 会自动截断以节省 token：

```json
{
  "tools": {
    "enableToolOutputTruncation": true,
    "truncateToolOutputThreshold": 50000,
    "truncateToolOutputLines": 200
  }
}
```

- `truncateToolOutputThreshold`：超过此字符数时触发截断（-1 禁用）
- `truncateToolOutputLines`：截断后保留的行数

## 搜索工具

Copilot Shell 默认使用内置的 ripgrep 进行代码搜索：

```json
{
  "tools": {
    "useRipgrep": true,
    "useBuiltinRipgrep": true
  }
}
```

- `useRipgrep`：启用 ripgrep（比默认实现更快）
- `useBuiltinRipgrep`：使用内置的 rg 二进制。设为 `false` 使用系统的 `rg`

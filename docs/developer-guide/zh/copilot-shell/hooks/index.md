# Copilot Shell Hooks

Hooks 是 copilot-shell 在代理循环的特定节点执行的脚本或程序，
允许你在不修改 CLI 源码的情况下拦截和自定义行为。

## 什么是 Hooks？

Hooks 作为代理循环的一部分同步运行 —— 当 hook 事件触发时，
copilot-shell 会等待所有匹配的 hook 完成后才继续执行。

通过 Hooks，你可以：

- **注入上下文**：在模型处理请求前注入相关信息（如 git 历史）
- **验证操作**：审查工具参数，阻止潜在危险操作
- **执行策略**：实施安全扫描和合规检查
- **记录交互**：跟踪工具使用和模型响应以供审计
- **优化行为**：动态过滤可用工具或调整模型参数
- **沙箱命令**：自动将危险 shell 命令包装在 linux-sandbox 中隔离执行

### 入门指南

- **[编写 Hooks 指南](writing-hooks.md)**：从零开始创建 hook 的教程
- **[Hooks 参考](reference.md)**：输入/输出 Schema 和退出码的技术规范

## 核心概念

### Hook 事件

Hooks 由 copilot-shell 生命周期中的特定事件触发。

| 事件 | 触发时机 | 影响 | 常见用例 |
|------|----------|------|----------|
| `SessionStart` | 会话开始（启动/恢复/清除） | 注入上下文 | 初始化资源、加载上下文 |
| `SessionEnd` | 会话结束（退出/清除） | 通知 | 清理资源、保存状态 |
| `UserPromptSubmit` | 用户提交提示后、规划前 | 阻止/上下文 | 添加上下文、验证输入 |
| `Stop` | 代理即将停止时 | 重试/停止 | 审查输出、强制重试 |
| `BeforeModel` | 发送 LLM 请求前 | 阻止/模拟 | 修改请求、切换模型 |
| `AfterModel` | 收到 LLM 响应后 | 阻止/观察 | 过滤响应、记录日志 |
| `BeforeToolSelection` | LLM 选择工具前 | 过滤工具 | 过滤可用工具集 |
| `PreToolUse` | 工具执行前 | 阻止/重写 | 验证参数、阻止危险操作 |
| `PostToolUse` | 工具执行后 | 阻止/上下文 | 处理结果、运行测试 |
| `PostToolUseFailure` | 工具执行失败后 | 恢复 | 提取原始命令、沙箱绕过 |
| `PreCompact` | 上下文压缩前 | 通知 | 保存状态 |
| `Notification` | 系统通知发生时 | 通知 | 转发桌面提醒 |
| `PermissionRequest` | 权限对话框显示时 | 允许/拒绝 | 自动批准或拒绝 |

### 全局机制

#### 严格 JSON 要求（"黄金法则"）

Hooks 通过 `stdin`（输入）和 `stdout`（输出）通信。

1. **必须保持沉默**：脚本**不得**向 `stdout` 输出 JSON 对象以外的任何文本
2. **污染 = 失败**：如果 `stdout` 包含非 JSON 文本，解析将失败
3. **调试用 stderr**：所有日志和调试信息使用 `stderr`（如 `echo "debug" >&2`）

#### 退出码

| 退出码 | 标签 | 行为影响 |
|--------|------|----------|
| **0** | 成功 | `stdout` 被解析为 JSON |
| **2** | 系统阻止 | 操作被中止，`stderr` 作为拒绝原因 |
| **其他** | 警告 | 非致命失败，显示警告后继续 |

#### 匹配器（Matcher）

使用 `matcher` 字段过滤触发 hook 的具体工具或事件：

- **工具事件**（`PreToolUse`、`PostToolUse`）：匹配器为**正则表达式**
- **生命周期事件**：匹配器为**精确字符串**
- **通配符**：`"*"` 或 `""`（空字符串）匹配所有

#### 多个 Hook 匹配时

当同一事件有多个 hook 匹配时：

1. **规划与去重**：按事件+匹配器选择 hook，基于 `name:command` 去重
2. **执行模式**：默认**并行**执行；若任一 hook 设置 `sequential: true` 则**串行**执行
3. **串行链式传递**：`PreToolUse` 可修改 `tool_input`，后续 hook 看到修改后的输入
4. **最终输出合并**：限制性结果优先（`deny`/`block`），原因文本拼接

## 配置

Hooks 在 `settings.json` 中配置，支持多层合并（优先级从高到低）：

1. **项目配置**：`.copilot-shell/settings.json`
2. **用户配置**：`~/.copilot-shell/settings.json`
3. **系统配置**：`/etc/copilot-shell/settings.json`
4. **扩展**：已安装扩展中定义的 hooks

### 配置示例

```json
{
  "hooks": {
    "enabled": true,
    "PreToolUse": [
      {
        "matcher": "run_shell_command",
        "sequential": true,
        "hooks": [
          {
            "type": "command",
            "command": "python3 hooks/sandbox-guard.py",
            "name": "sandbox-guard",
            "timeout": 10000,
            "description": "将危险命令包装在沙箱中执行"
          }
        ]
      }
    ]
  }
}
```

### Hook 配置字段

| 字段 | 类型 | 必需 | 说明 |
|------|------|------|------|
| `type` | string | 是 | 执行引擎，目前仅支持 `"command"` |
| `command` | string | 是 | 要执行的 shell 命令 |
| `name` | string | 否 | 用于日志和 CLI 命令中标识 hook |
| `timeout` | number | 否 | 执行超时（毫秒），默认 60000 |
| `description` | string | 否 | hook 用途的简要说明 |

### 环境变量

Hooks 执行时可用以下环境变量：

- `COPILOT_SHELL_PROJECT_DIR`：项目根目录的绝对路径

## 安全与风险

> **警告**：Hooks 以你的用户权限执行任意代码。

**项目级 hooks** 在打开不受信任的项目时特别危险。copilot-shell 会对项目
hooks 进行**指纹识别**。如果 hook 的名称或命令发生变化（如通过 `git pull`），
它会被视为**新的不受信任的 hook**，执行前会收到警告。

## 管理 Hooks

使用 CLI 命令管理 hooks：

- **查看**：`/hooks panel`
- **全部启用/禁用**：`/hooks enable-all` 或 `/hooks disable-all`
- **单个切换**：`/hooks enable <name>` 或 `/hooks disable <name>`

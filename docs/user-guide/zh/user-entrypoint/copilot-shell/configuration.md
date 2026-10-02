# 配置系统

Copilot Shell 采用分层配置系统，支持从系统级到项目级的多层覆盖。

## 配置文件位置

| 层级 | 路径 | 用途 |
|------|------|------|
| 系统设置 | `/etc/copilot-shell/settings.json` | 管理员全局策略 |
| 系统默认 | `/etc/copilot-shell/system-defaults.json` | 系统默认值 |
| 用户设置 | `~/.copilot-shell/settings.json` | 个人偏好 |
| 项目设置 | `.copilot-shell/settings.json` | 项目级覆盖 |

## 优先级（从高到低）

1. 命令行参数
2. 环境变量
3. 系统设置（管理员强制覆盖）
4. 项目设置
5. 用户设置
6. 系统默认值

高优先级的值会覆盖低优先级的值。系统设置作为管理员策略层，
优先级高于用户和项目设置，用于强制执行组织级安全策略。

## 配置类别

### general — 通用设置

| 键 | 类型 | 默认值 | 说明 |
|----|------|--------|------|
| `general.language` | string | `"auto"` | 界面语言（`auto`/`en`/`zh-CN`） |
| `general.outputLanguage` | string | `"auto"` | LLM 输出语言 |
| `general.vimMode` | boolean | `false` | 启用 Vim 键绑定 |
| `general.preferredEditor` | string | — | 首选编辑器 |
| `general.gitCoAuthor` | boolean | `true` | 自动添加 Co-authored-by |
| `general.terminalBell` | boolean | `true` | 响应完成时播放终端铃声 |
| `general.chatRecording` | boolean | `true` | 保存聊天历史到磁盘 |
| `general.checkpointing.enabled` | boolean | `false` | 启用会话检查点 |

### ui — 界面设置

| 键 | 类型 | 默认值 | 说明 |
|----|------|--------|------|
| `ui.theme` | string | `"Copilot Shell Dark"` | 颜色主题 |
| `ui.hideTips` | boolean | `false` | 隐藏提示信息 |
| `ui.showLineNumbers` | boolean | `false` | 代码显示行号 |
| `ui.compactMode` | boolean | `false` | 紧凑模式（`Ctrl+O` 切换） |
| `ui.enableWelcomeBack` | boolean | `true` | 显示「欢迎回来」对话框 |
| `ui.customThemes` | object | `{}` | 自定义主题定义 |

### tools — 工具设置

| 键 | 类型 | 默认值 | 说明 |
|----|------|--------|------|
| `tools.approvalMode` | enum | `"default"` | 审批模式（plan/default/auto-edit/yolo） |
| `tools.allowed` | array | — | 免确认的工具白名单 |
| `tools.exclude` | array | — | 排除的工具列表 |
| `tools.shell.enableInteractiveShell` | boolean | `false` | 启用 PTY 交互式 Shell |
| `tools.useRipgrep` | boolean | `true` | 使用 ripgrep 搜索 |

### security — 安全设置

| 键 | 类型 | 默认值 | 说明 |
|----|------|--------|------|
| `security.auth.selectedType` | string | — | 当前认证类型 |
| `security.auth.enforcedType` | string | — | 强制认证类型 |
| `security.auth.apiKey` | string | — | OpenAI 兼容 API 密钥 |
| `security.auth.baseUrl` | string | — | OpenAI 兼容 Base URL |
| `security.folderTrust.enabled` | boolean | `false` | 文件夹信任 |

### model — 模型设置

| 键 | 类型 | 默认值 | 说明 |
|----|------|--------|------|
| `model.name` | string | — | 当前使用的模型 |
| `model.maxSessionTurns` | number | `-1` | 最大会话轮次（-1 无限） |
| `model.sessionTokenLimit` | number | — | 会话 token 上限 |
| `model.chatCompression` | object | — | 聊天压缩配置 |
| `model.generationConfig.timeout` | number | — | 请求超时（毫秒） |
| `model.generationConfig.maxRetries` | number | — | 最大重试次数 |
| `model.generationConfig.contextWindowSize` | number | — | 覆盖上下文窗口大小 |

### context — 上下文设置

| 键 | 类型 | 默认值 | 说明 |
|----|------|--------|------|
| `context.fileName` | string | — | 上下文文件名 |
| `context.includeDirectories` | array | `[]` | 额外包含的目录 |
| `context.fileFiltering.respectGitIgnore` | boolean | `true` | 尊重 .gitignore |
| `context.fileFiltering.respectQwenIgnore` | boolean | `true` | 尊重 .copilotignore |

### mcp — MCP 服务器

| 键 | 类型 | 默认值 | 说明 |
|----|------|--------|------|
| `mcpServers` | object | `{}` | MCP 服务器配置 |
| `mcp.allowed` | array | — | 允许的 MCP 服务器 |
| `mcp.excluded` | array | — | 排除的 MCP 服务器 |

### hooksConfig — Hooks 配置

| 键 | 类型 | 默认值 | 说明 |
|----|------|--------|------|
| `hooksConfig.enabled` | boolean | `true` | 启用 hooks 系统 |
| `hooksConfig.disabled` | array | `[]` | 禁用的 hook 名称列表 |

### hooks — Hook 事件配置

| 键 | 类型 | 说明 |
|----|------|------|
| `hooks.PreToolUse` | array | 工具执行前触发的 hooks |
| `hooks.UserPromptSubmit` | array | 代理处理前触发的 hooks |
| `hooks.Stop` | array | 代理处理后触发的 hooks |

### skills — 技能配置

| 键 | 类型 | 默认值 | 说明 |
|----|------|--------|------|
| `skills.customPaths` | array | `[]` | 自定义技能搜索路径 |
| `skillOS.baseUrl` | string | — | 远程 Skill-OS 地址 |
| `clawhub.registry` | string | — | Clawhub 注册表 URL |

### webSearch — Web 搜索

| 键 | 类型 | 说明 |
|----|------|------|
| `webSearch.provider` | array | 搜索提供商配置（Tavily/Google/DashScope） |
| `webSearch.default` | string | 默认搜索提供商 |

### autoMemory — 自动记忆

| 键 | 类型 | 默认值 | 说明 |
|----|------|--------|------|
| `autoMemory.enabled` | boolean | `false` | 启用自动记忆提取 |
| `autoMemory.cooldownSeconds` | number | `1800` | 提取冷却时间（秒） |

## 配置示例

完整的用户配置示例（`~/.copilot-shell/settings.json`）：

```json
{
  "general": {
    "language": "zh-CN",
    "outputLanguage": "Chinese",
    "vimMode": false
  },
  "ui": {
    "theme": "Copilot Shell Dark",
    "compactMode": false
  },
  "tools": {
    "approvalMode": "default",
    "shell": {
      "enableInteractiveShell": true
    }
  },
  "security": {
    "auth": {
      "selectedType": "aliyun"
    }
  },
  "model": {
    "generationConfig": {
      "timeout": 60000,
      "maxRetries": 3
    }
  }
}
```

## 环境变量

部分配置支持通过环境变量设置：

| 环境变量 | 对应配置 |
|----------|----------|
| `OPENAI_API_KEY` | `security.auth.apiKey` |
| `OPENAI_BASE_URL` | `security.auth.baseUrl` |

## 项目信任

当 Copilot Shell 在新项目目录下首次运行时，项目级配置
（`.copilot-shell/settings.json`）默认不受信任。只有在用户确认信任该目录
后，项目设置才会生效。

通过 `security.folderTrust.enabled` 控制此行为。

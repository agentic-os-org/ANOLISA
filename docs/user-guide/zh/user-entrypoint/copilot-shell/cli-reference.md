# CLI 参考

Copilot Shell 的命令行接口支持多种标志和选项，用于控制启动行为、
认证方式、工具配置和输出格式。

## 基本用法

```bash
cosh [options] [query]
```

别名：`co`、`copilot`

## 常用选项

| 选项 | 简写 | 说明 |
|------|------|------|
| `--help` | `-h` | 显示帮助信息 |
| `--version` | `-v` | 显示版本号 |
| `--debug` | `-d` | 启用调试模式 |
| `--model <name>` | `-m` | 指定使用的模型 |
| `--prompt <text>` | `-p` | 非交互式模式：执行提示后退出 |
| `--prompt-interactive <text>` | `-i` | 执行提示后保持交互 |
| `--yolo` | `-y` | 自动批准所有操作（YOLO 模式） |

## 会话选项

| 选项 | 说明 |
|------|------|
| `--continue` | 恢复最近的会话 |
| `--resume <id>` | 恢复指定 ID 的会话 |
| `--max-session-turns <n>` | 限制最大会话轮次 |

## 审批选项

| 选项 | 说明 |
|------|------|
| `--approval-mode <mode>` | 设置审批模式（plan/default/auto-edit/yolo） |
| `--checkpointing` | 启用文件编辑检查点（可回退修改） |

## 认证选项

| 选项 | 说明 |
|------|------|
| `--auth-type <type>` | 指定认证类型 |
| `--openai-api-key <key>` | OpenAI 兼容 API 密钥 |
| `--openai-base-url <url>` | OpenAI 兼容 Base URL |

## 工具选项

| 选项 | 说明 |
|------|------|
| `--allowed-tools <list>` | 允许的工具（逗号分隔） |
| `--exclude-tools <list>` | 排除的工具（逗号分隔） |
| `--core-tools <path>` | 核心工具定义文件路径 |
| `--allowed-mcp-server-names <list>` | 允许的 MCP 服务器（逗号分隔） |

## 扩展选项

| 选项 | 说明 |
|------|------|
| `--extensions <list>` | 加载的扩展列表（逗号分隔） |
| `--list-extensions` | 列出所有已加载扩展后退出 |

## 输入输出选项

| 选项 | 简写 | 说明 |
|------|------|------|
| `--input-format <fmt>` | `-I` | 输入格式（text/stream-json） |
| `--output-format <fmt>` | `-O` | 输出格式（text/json/stream-json） |
| `--include-partial-messages` | — | 包含部分消息（仅 stream-json） |

## 高级选项

| 选项 | 说明 |
|------|------|
| `--all-files` / `-a` | 包含所有文件到上下文 |
| `--acp` | ACP 模式（Zed 集成） |
| `--proxy <url>` | 网络代理（格式：schema://user:password@host:port） |
| `--screen-reader` | 屏幕阅读器无障碍模式 |
| `--skip-startup-context` | 跳过工作区启动上下文 |
| `--skip-loop-detection` | 跳过循环检测 |

## 使用示例

### 非交互式执行

```bash
# 执行一个任务后退出
cosh -p "列出所有 TODO 注释"

# 指定模型
cosh -m qwen3.7-max -p "解释这段代码的作用"
```

### 恢复会话

```bash
# 恢复最近的会话
cosh --continue

# 恢复指定会话
cosh --resume abc123
```

### YOLO 模式

```bash
# 自动批准所有操作
cosh -y -p "修复所有 lint 错误"
```

### JSON 输出

```bash
# 以 JSON 格式输出结果
cosh -O json -p "分析项目依赖"
```

### 代理设置

```bash
cosh --proxy http://proxy.example.com:8080
```

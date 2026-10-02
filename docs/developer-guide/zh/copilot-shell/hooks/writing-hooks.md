# 编写 Hooks

本指南带你从零开始为 copilot-shell 创建 hooks，从简单日志到完整的工作流自动化。

## 前提条件

- copilot-shell 已安装并配置
- 熟悉 Shell 脚本、Python 或 Node.js
- 了解 JSON 格式

## 快速开始

创建一个记录所有工具执行的简单 hook。

**关键规则**：始终将日志写入 `stderr`，仅将最终 JSON 写入 `stdout`。

### 第一步：创建 hook 脚本

```bash
mkdir -p .copilot-shell/hooks
cat > .copilot-shell/hooks/log-tools.sh << 'EOF'
#!/usr/bin/env bash
input=$(cat)
tool_name=$(echo "$input" | jq -r '.tool_name')
echo "Logging tool: $tool_name" >&2
echo "[$(date)] Tool executed: $tool_name" >> .copilot-shell/tool-log.txt
echo "{}"
EOF
chmod +x .copilot-shell/hooks/log-tools.sh
```

### 第二步：在 settings.json 中注册

```json
{
  "hooks": {
    "enabled": true,
    "PostToolUse": [
      {
        "hooks": [
          {
            "type": "command",
            "command": ".copilot-shell/hooks/log-tools.sh",
            "name": "tool-logger",
            "timeout": 5000
          }
        ]
      }
    ]
  }
}
```

### 第三步：运行

启动 copilot-shell 后，每次工具执行都会被记录到 `.copilot-shell/tool-log.txt`。

---

## 实战示例

### 安全：阻止文件中写入密钥

防止写入包含 API 密钥或密码的文件。

**`.copilot-shell/hooks/block-secrets.sh`：**

```bash
#!/usr/bin/env bash
input=$(cat)
content=$(echo "$input" | jq -r '.tool_input.content // .tool_input.new_string // ""')

if echo "$content" | grep -qE 'api[_-]?key|password|secret'; then
  echo "Blocked potential secret" >&2
  cat <<EOF
{
  "decision": "deny",
  "reason": "安全策略：检测到潜在密钥内容。",
  "systemMessage": "安全扫描器已阻止此操作"
}
EOF
  exit 0
fi

echo '{"decision": "allow"}'
exit 0
```

**配置：**

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "write_file",
        "hooks": [
          {
            "type": "command",
            "command": ".copilot-shell/hooks/block-secrets.sh",
            "name": "secret-scanner"
          }
        ]
      }
    ]
  }
}
```

### 动态上下文注入（Git 历史）

在每次代理交互前添加项目上下文。

**`.copilot-shell/hooks/inject-context.sh`：**

```bash
#!/usr/bin/env bash
context=$(git log -5 --oneline 2>/dev/null || echo "No git history")

cat <<EOF
{
  "hookSpecificOutput": {
    "hookEventName": "UserPromptSubmit",
    "additionalContext": "最近提交:\n$context"
  }
}
EOF
```

### RAG 工具过滤（BeforeToolSelection）

使用 `BeforeToolSelection` 智能缩减工具空间。

**`.copilot-shell/hooks/filter-tools.js`：**

```javascript
#!/usr/bin/env node
const fs = require('fs');

function main() {
  const input = JSON.parse(fs.readFileSync(0, 'utf-8'));
  const { llm_request } = input;
  const messages = llm_request.messages || [];
  const lastUserMessage = messages.slice().reverse().find(m => m.role === 'user');

  if (!lastUserMessage) {
    console.log(JSON.stringify({}));
    return;
  }

  const text = lastUserMessage.content;
  const allowed = ['write_todos'];

  if (text.includes('read') || text.includes('check')) {
    allowed.push('read_file', 'list_directory');
  }
  if (text.includes('test')) {
    allowed.push('run_shell_command');
  }

  if (allowed.length > 1) {
    console.log(JSON.stringify({
      hookSpecificOutput: {
        hookEventName: 'BeforeToolSelection',
        toolConfig: { mode: 'ANY', allowedFunctionNames: allowed }
      }
    }));
  } else {
    console.log(JSON.stringify({}));
  }
}

main();
```

### 模型路由（BeforeModel）

根据复杂度将请求路由到不同模型。

**`.copilot-shell/hooks/model-router.py`：**

```python
#!/usr/bin/env python3
import sys, json

input_data = json.load(sys.stdin)
llm_request = input_data.get("llm_request", {})
messages = llm_request.get("messages", [])

last_msg = messages[-1]["content"] if messages else ""
is_simple = len(last_msg) < 100 and not any(
    kw in last_msg.lower() for kw in ["refactor", "architect", "design"]
)

if is_simple:
    result = {
        "hookSpecificOutput": {
            "hookEventName": "BeforeModel",
            "llm_request": {
                "model": "qwen-turbo",
                "config": {"temperature": 0.3},
            },
        }
    }
else:
    result = {}

print(json.dumps(result))
```

### 合成响应（BeforeModel — Mock）

跳过 LLM 调用，直接返回预定义响应。

**`.copilot-shell/hooks/mock-response.py`：**

```python
#!/usr/bin/env python3
import sys, json

input_data = json.load(sys.stdin)
messages = input_data.get("llm_request", {}).get("messages", [])
last_msg = messages[-1]["content"] if messages else ""

if "ping" in last_msg.lower():
    result = {
        "decision": "deny",
        "reason": "BeforeModel hook 处理的合成响应",
        "hookSpecificOutput": {
            "hookEventName": "BeforeModel",
            "llm_response": {
                "text": "pong!",
                "candidates": [{
                    "content": {"role": "model", "parts": ["pong!"]},
                    "finishReason": "STOP"
                }],
                "usageMetadata": {"totalTokenCount": 0}
            }
        }
    }
    print(json.dumps(result))
else:
    print("{}")
```

### 审计追踪（PostToolUse）

使用 `run_id` 关联单次代理运行内的所有工具调用。

**`.copilot-shell/hooks/audit-trail.py`：**

```python
#!/usr/bin/env python3
import sys, json, datetime

input_data = json.load(sys.stdin)

entry = {
    "timestamp": datetime.datetime.now().isoformat(),
    "session_id": input_data["session_id"],
    "run_id": input_data.get("run_id"),
    "event": input_data["hook_event_name"],
    "tool": input_data.get("tool_name", ""),
}

with open(".copilot-shell/audit.jsonl", "a") as f:
    f.write(json.dumps(entry) + "\n")

print("{}")
```

查询特定运行的所有操作：

```bash
jq 'select(.run_id == "sess########3")' .copilot-shell/audit.jsonl
```

---

## 使用不同语言编写 Hooks

### Python（推荐用于复杂逻辑）

```python
#!/usr/bin/env python3
import sys, json

def main():
    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError):
        print(json.dumps({}))
        return

    # 你的逻辑
    print(json.dumps({"decision": "allow"}))

if __name__ == "__main__":
    main()
```

### Node.js

```javascript
#!/usr/bin/env node
const fs = require('fs');

function main() {
  const input = JSON.parse(fs.readFileSync(0, 'utf-8'));
  // 你的逻辑
  console.log(JSON.stringify({ decision: 'allow' }));
}

main();
```

### Bash（仅用于简单 hooks）

```bash
#!/usr/bin/env bash
input=$(cat)
tool_name=$(echo "$input" | jq -r '.tool_name // empty')
echo '{"decision": "allow"}'
```

---

## 测试 Hooks

通过直接向脚本管道传入 JSON 来手动测试：

```bash
printf '{"hook_event_name":"PreToolUse","tool_name":"run_shell_command",
  "tool_input":{"command":"rm -rf /tmp/test"}}' \
  | python3 .copilot-shell/hooks/block-secrets.sh
```

在会话中启用实时追踪，设置 `COPILOT_SHELL_DEBUG=1` 可在调试日志中看到
hook 调用及其原始输出。

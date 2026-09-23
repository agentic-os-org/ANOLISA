# Writing Hooks

This guide walks you through creating hooks for copilot-shell from scratch,
from simple logging to complete workflow automation.

## Prerequisites

- copilot-shell installed and configured
- Familiarity with Shell scripting, Python, or Node.js
- Understanding of JSON format

## Quick Start

Create a simple hook that logs all tool executions.

**Key rule**: Always write logs to `stderr`; only write the final JSON to `stdout`.

### Step 1: Create the hook script

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

### Step 2: Register in settings.json

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

### Step 3: Run

After starting copilot-shell, every tool execution will be logged to
`.copilot-shell/tool-log.txt`.

---

## Practical Examples

### Security: Block Writing Secrets to Files

Prevent writing files that contain API keys or passwords.

**`.copilot-shell/hooks/block-secrets.sh`:**

```bash
#!/usr/bin/env bash
input=$(cat)
content=$(echo "$input" | jq -r '.tool_input.content // .tool_input.new_string // ""')

if echo "$content" | grep -qE 'api[_-]?key|password|secret'; then
  echo "Blocked potential secret" >&2
  cat <<EOF
{
  "decision": "deny",
  "reason": "Security policy: potential secret content detected.",
  "systemMessage": "Security scanner blocked this operation"
}
EOF
  exit 0
fi

echo '{"decision": "allow"}'
exit 0
```

**Configuration:**

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

### Dynamic Context Injection (Git History)

Add project context before each agent interaction.

**`.copilot-shell/hooks/inject-context.sh`:**

```bash
#!/usr/bin/env bash
context=$(git log -5 --oneline 2>/dev/null || echo "No git history")

cat <<EOF
{
  "hookSpecificOutput": {
    "hookEventName": "UserPromptSubmit",
    "additionalContext": "Recent commits:\n$context"
  }
}
EOF
```

### RAG Tool Filtering (BeforeToolSelection)

Use `BeforeToolSelection` to intelligently reduce the tool space.

**`.copilot-shell/hooks/filter-tools.js`:**

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

### Model Routing (BeforeModel)

Route requests to different models based on complexity.

**`.copilot-shell/hooks/model-router.py`:**

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

### Synthetic Response (BeforeModel — Mock)

Skip the LLM call and return a predefined response directly.

**`.copilot-shell/hooks/mock-response.py`:**

```python
#!/usr/bin/env python3
import sys, json

input_data = json.load(sys.stdin)
messages = input_data.get("llm_request", {}).get("messages", [])
last_msg = messages[-1]["content"] if messages else ""

if "ping" in last_msg.lower():
    result = {
        "decision": "deny",
        "reason": "Synthetic response handled by BeforeModel hook",
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

### Audit Trail (PostToolUse)

Use `run_id` to correlate all tool calls within a single agent run.

**`.copilot-shell/hooks/audit-trail.py`:**

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

Query all operations for a specific run:

```bash
jq 'select(.run_id == "sess########3")' .copilot-shell/audit.jsonl
```

---

## Writing Hooks in Different Languages

### Python (Recommended for complex logic)

```python
#!/usr/bin/env python3
import sys, json

def main():
    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError):
        print(json.dumps({}))
        return

    # Your logic here
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
  // Your logic here
  console.log(JSON.stringify({ decision: 'allow' }));
}

main();
```

### Bash (Only for simple hooks)

```bash
#!/usr/bin/env bash
input=$(cat)
tool_name=$(echo "$input" | jq -r '.tool_name // empty')
echo '{"decision": "allow"}'
```

---

## Testing Hooks

Manually test by piping JSON directly to the script:

```bash
printf '{"hook_event_name":"PreToolUse","tool_name":"run_shell_command",
  "tool_input":{"command":"rm -rf /tmp/test"}}' \
  | python3 .copilot-shell/hooks/block-secrets.sh
```

Enable live tracing in a session by setting `COPILOT_SHELL_DEBUG=1` to see
hook invocations and their raw output in debug logs.
# Writing hooks for copilot-shell

This guide walks you through creating hooks for copilot-shell, from simple
logging to comprehensive workflow automation.

## Prerequisites

- copilot-shell installed and configured
- Basic understanding of shell scripting, Python, or Node.js
- Familiarity with JSON for hook input/output

## Quick start

Let's create a simple hook that logs all tool executions.

**Crucial Rule:** Always write logs to `stderr`. Write only the final JSON to
`stdout`.

### Step 1: Create your hook script

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

### Step 2: Register in settings.json

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

### Step 3: Run copilot-shell

Now every tool execution will be logged to `.copilot-shell/tool-log.txt`.

---

## Practical examples

### Security: Block secrets in file writes

Prevent writing files containing API keys or passwords.

**`.copilot-shell/hooks/block-secrets.sh`:**

```bash
#!/usr/bin/env bash
input=$(cat)
content=$(echo "$input" | jq -r '.tool_input.content // .tool_input.new_string // ""')

if echo "$content" | grep -qE 'api[_-]?key|password|secret'; then
  echo "Blocked potential secret" >&2
  cat <<EOF
{
  "decision": "deny",
  "reason": "Security Policy: Potential secret detected in content.",
  "systemMessage": "Security scanner blocked operation"
}
EOF
  exit 0
fi

echo '{"decision": "allow"}'
exit 0
```

For `PreToolUse`, put any user-facing text in `systemMessage` — it is rendered
as a per-hook notification box labeled with the hook name, independent of the
final decision. Use `reason` for the denial/error message on `deny` or `block`
outcomes; if `systemMessage` is omitted, `reason` is used as the fallback text
for the notification.

**Configuration:**

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

### Dynamic context injection (Git History)

Add relevant project context before each agent interaction.

**`.copilot-shell/hooks/inject-context.sh`:**

```bash
#!/usr/bin/env bash
context=$(git log -5 --oneline 2>/dev/null || echo "No git history")

cat <<EOF
{
  "hookSpecificOutput": {
    "hookEventName": "UserPromptSubmit",
    "additionalContext": "Recent commits:\n$context"
  }
}
EOF
```

### RAG-based Tool Filtering (BeforeToolSelection)

Use `BeforeToolSelection` to intelligently reduce the tool space.

**`.copilot-shell/hooks/filter-tools.js`:**

```javascript
#!/usr/bin/env node
const fs = require('fs');

async function main() {
  const input = JSON.parse(fs.readFileSync(0, 'utf-8'));
  const { llm_request } = input;

  const messages = llm_request.messages || [];
  const lastUserMessage = messages
    .slice()
    .reverse()
    .find((m) => m.role === 'user');

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
    console.log(
      JSON.stringify({
        hookSpecificOutput: {
          hookEventName: 'BeforeToolSelection',
          toolConfig: {
            mode: 'ANY',
            allowedFunctionNames: allowed,
          },
        },
      }),
    );
  } else {
    console.log(JSON.stringify({}));
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
```

### Model Router (BeforeModel)

Route requests to different models based on complexity.

**`.copilot-shell/hooks/model-router.py`:**

```python
#!/usr/bin/env python3
import sys, json

input_data = json.load(sys.stdin)
llm_request = input_data.get("llm_request", {})
messages = llm_request.get("messages", [])

# Check if the last message is simple
last_msg = messages[-1]["content"] if messages else ""
is_simple = len(last_msg) < 100 and not any(
    kw in last_msg.lower() for kw in ["refactor", "architect", "design"]
)

if is_simple:
    # Use a faster, cheaper model for simple queries
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

### Synthetic Response (BeforeModel — Mock)

Skip the LLM call entirely and return a predefined response.

**`.copilot-shell/hooks/mock-response.py`:**

```python
#!/usr/bin/env python3
import sys, json

input_data = json.load(sys.stdin)
llm_request = input_data.get("llm_request", {})
messages = llm_request.get("messages", [])

last_msg = messages[-1]["content"] if messages else ""

if "ping" in last_msg.lower():
    result = {
    "decision": "deny",
    "reason": "Synthetic response handled by BeforeModel hook",
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

### Audit Trail with run_id (PostToolUse)

Use `run_id` to correlate all tool calls within a single agent run for auditing.

**`.copilot-shell/hooks/audit-trail.py`:**

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

Query all actions from a specific run:

```bash
jq 'select(.run_id == "sess########3")' .copilot-shell/audit.jsonl
```

### Response Logger (AfterModel)

Log all LLM responses for auditing.

**`.copilot-shell/hooks/log-responses.py`:**

```python
#!/usr/bin/env python3
import sys, json, datetime

input_data = json.load(sys.stdin)
llm_response = input_data.get("llm_response", {})
llm_request = input_data.get("llm_request", {})

entry = {
    "timestamp": datetime.datetime.now().isoformat(),
    "model": llm_request.get("model", "unknown"),
    "response_text": llm_response.get("text", "")[:200],
    "tokens": llm_response.get("usageMetadata", {}).get("totalTokenCount", 0)
}

with open(".copilot-shell/response-log.jsonl", "a") as f:
    f.write(json.dumps(entry) + "\n")

# Observation only - return empty output
print("{}")
```

---

## Writing hooks in different languages

### Python (Recommended for complex logic)

```python
#!/usr/bin/env python3
import sys, json

def main():
    try:
        input_data = json.load(sys.stdin)
    except (json.JSONDecodeError, EOFError):
        print(json.dumps({}))
        return

    # Your logic here
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
  // Your logic here
  console.log(JSON.stringify({ decision: 'allow' }));
}

main();
```

### Bash (Simple hooks only)

```bash
#!/usr/bin/env bash
input=$(cat)
# Use jq for JSON parsing
tool_name=$(echo "$input" | jq -r '.tool_name // empty')
echo '{"decision": "allow"}'
```

---

## Testing your hooks

You can test hooks manually by piping JSON directly to your script. For example,
to test the built-in `sandbox-guard.py` hook with a dangerous command:

```bash
printf '{"hook_event_name":"PreToolUse","tool_name":"run_shell_command",
  "tool_input":{"command":"rm -rf /tmp/test"}}' \
  | python3 deprecated/copilot-shell/hooks/sandbox-guard.py
```

For live tracing during a session, set `COPILOT_SHELL_DEBUG=1` to see hook
invocations and their raw outputs in the debug log.

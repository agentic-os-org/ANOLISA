# 安全启发规则

[English](../../en/cosh-ng/security-heuristics.md)

## 概述

cosh-ng 审计子系统实现了 PEP→PDP→Log 三阶段安全决策流水线。每条命令在执行前经过
结构化解析、策略匹配和日志记录，决定 Allow / Deny / RequireApproval 三种处置结果。

## 架构

```
原始命令字符串
     │
     ▼
┌─────────────────┐
│ action parser   │  拒绝 shell 元字符、控制字节
│ (PEP 边界)       │  结构化为 Action{subsystem,operation,target,args}
└────────┬────────┘
         │ 解析失败 → 直接 Deny
         ▼
┌─────────────────┐
│ evaluate (PDP)  │  遍历 policy.rules[]，首条匹配胜出
│                 │  无匹配 → policy.default
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│ audit log       │  脱敏后写入 JSONL 日志
│ (redact + log)  │  CallerInfo: session/user/uid/pid
└─────────────────┘
```

代码位于 `crates/cosh-platform/src/audit/`。

## 命令解析（action parser）

源文件位于 `audit/action.rs`。

解析器会在 PDP 处理前拒绝危险输入。

| 检查 | 拒绝条件 | 理由 |
|------|----------|------|
| 空字符串 | `trim()` 后为空 | 无有效操作 |
| 控制字节 | 包含 `\n` 或 `\r` | 防止命令注入 |
| Shell 元字符 | 包含 `;|&><$\`(){}` 中任意字符 | 防止命令链接/重定向/子 shell |

解析失败时，调用方应映射为 `Outcome::Deny`（永不自动放行）。

解析成功后，按首个 Token 决定结构。

- `pkg` / `svc` / `checkpoint` / `cosh` → 结构化子系统（operation=tokens[1]，target=tokens[2]）
- 其它 → Shell 子系统（operation=首 token，target=第二 token，args=tokens[1..]）

## 策略系统

源文件位于 `audit/policy.rs` 和 `audit/builtin.rs`。

### 策略加载优先级

1. `$COSH_AUDIT_POLICY` 环境变量指定的文件
2. `~/.copilot-shell/cosh/audit.toml`（用户级）
3. `/etc/cosh/audit.toml`（系统级）
4. 内置 `balanced` 预设（出厂默认）

只使用第一个存在的来源，不跨文件合并。

### 内置预设

| 预设 | 默认结果 | 适用场景 |
|------|----------|----------|
| `permissive` | Allow | 沙箱 / CI 环境 |
| `balanced` | RequireApproval | 日常开发（默认） |
| `strict` | Deny | 生产 / 不可信 Agent |

### 策略文件格式（TOML）

```toml
version = "v1"
default = "RequireApproval"   # Allow / Deny / RequireApproval

[[rules]]
name = "allow-readonly"
matches.subsystem = "shell"
matches.operation = { one_of = ["ls", "cat", "ps", "df", "echo", "uptime"] }
outcome = "Allow"

[[rules]]
name = "deny-destructive"
matches.subsystem = "shell"
matches.operation = { one_of = ["rm", "sudo", "shutdown", "dd", "mkfs", "tee"] }
outcome = "Deny"
reason = "destructive command blocked by policy"
```

### 匹配语法（StringMatch）

| 形式 | 示例 | 说明 |
|------|------|------|
| 精确匹配 | `"install"` | 字符串相等 |
| 枚举匹配 | `{ one_of = ["start", "restart", "stop"] }` | 任一匹配即可 |
| Glob 匹配 | `{ glob = "-i*" }` | 支持 `*` 和 `?` |

match 块支持 `subsystem`、`operation`、`target`、`arg[].key` 和 `arg[].value` 字段。

## 决策引擎（evaluate）

源文件位于 `audit/evaluate.rs`。

- 遍历 `policy.rules[]`，第一条匹配的规则决定结果
- 未匹配任何规则时使用 `policy.default`
- 返回 `Decision { outcome, reason, matched_rule, policy_version }`
- `policy_version` 包含来源标识 + SHA256 哈希，确保审计可追溯

## balanced 预设核心规则

### 允许（Allow）

| 类别 | 命令示例 |
|------|----------|
| 只读单体命令 | `uptime`, `ls -la`, `cat`, `ps aux`, `df -h`, `echo` |
| Git 只读 | `git status`, `git log`, `git diff`, `git show`, `git blame` |
| Git 分支查看 | `git branch`, `git branch -v` |
| Git stash 查看 | `git stash`, `git stash list`, `git stash show` |
| 安全工具对 | `systemctl status`, `apt list`, `dnf list`, `docker ps` |
| pkg/svc 只读 | `pkg search`, `pkg list`, `svc status`, `svc list` |
| checkpoint 只读 | `checkpoint list`, `checkpoint status` |

### 拒绝（Deny）

| 类别 | 命令示例 |
|------|----------|
| 破坏性命令 | `rm -rf /`, `sudo`, `shutdown`, `dd`, `mkfs`, `tee` |
| Git 变更 | `git push`, `git reset --hard`, `git clean`, `git rebase` |
| Git 分支变更 | `git branch -D`, `git branch -m`, `git branch --delete` |
| Git stash 变更 | `git stash drop`, `git stash clear`, `git stash pop/apply` |
| sed 就地编辑 | `sed -i`, `sed --in-place` |
| find 破坏 | `find . -delete`, `find . -fprint` |

### 需要审批（RequireApproval）

| 类别 | 命令示例 |
|------|----------|
| 包管理写操作 | `pkg install`, `pkg remove` |
| 服务管理写操作 | `svc start`, `svc restart` |
| 快照写操作 | `checkpoint create`, `checkpoint restore` |
| 未知命令 | 不匹配任何 allow/deny 规则的命令 |

## 日志与脱敏

源文件位于 `audit/log.rs` 和 `audit/redact.rs`。

### 脱敏规则

写入日志前会自动脱敏。

| 检测方式 | 触发条件 | 替换值 |
|----------|----------|--------|
| 敏感 key | args 中 key 包含 `password/secret/token/api_key/apikey` | `<redacted>` |
| PEM 内容 | raw 字段包含 `BEGIN PRIVATE KEY` 等 PEM 头 | `<redacted-pem>` |

脱敏在 log-write 时执行（非 PDP 阶段），确保 PDP 可以基于原始值决策。

### 日志条目字段

```json
{
  "timestamp": "2025-01-01T00:00:00Z",
  "session_id": "p1234-t1704067200",
  "user": "admin",
  "uid": 1000,
  "euid": 1000,
  "sudo_user": null,
  "pid": 1234,
  "action": { "subsystem": "pkg", "operation": "install", ... },
  "decision": { "outcome": "RequireApproval", "reason": "...", ... },
  "source": "Cli",
  "redacted": false
}
```

日志路径通过 `$COSH_AUDIT_LOG` 环境变量覆盖（测试用）。

## 公开 API

| 函数 | 用途 |
|------|------|
| `audit::check(action, source, &loaded)` | 完整 PEP→PDP→Log 流程 |
| `audit::classify(action, &loaded)` | 仅 PDP，不写日志（TUI 实时分类用） |
| `audit::record_decision(action, &decision, source)` | 记录已决策结果（如解析失败的 Deny） |
| `audit::evaluate(action, &loaded)` | 纯 PDP 函数 |
| `parse_action_string(raw)` | 原始字符串 → Action |
| `LoadedPolicy::load()` | 加载活跃策略 |

## 测试验证

```bash
cd src/cosh-ng

# 审计策略匹配测试（balanced 预设的 allow/deny/approve 覆盖）
cargo test --locked -p cosh-platform -- audit

# action parser 测试
cargo test --locked -p cosh-platform -- action

# 策略加载和验证测试
cargo test --locked -p cosh-platform -- policy

# 脱敏测试
cargo test --locked -p cosh-platform -- redact
```

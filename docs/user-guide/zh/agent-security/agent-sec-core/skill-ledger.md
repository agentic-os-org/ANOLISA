# Skill Ledger 用户使用手册

本文说明 Python V1 部署和 Agent 集成。Rust V2 核心、系统签名密钥和 root daemon 的使用方式见
[SkillGuard V2 指南](skillguard-v2.md)。

Skill Ledger 是 agent-sec-core 的安全子系统，为 AI Agent Skill 提供文件哈希、扫描结果和密码学签名的版本链，帮助发现 Skill 被篡改或注入恶意内容。默认快速扫描由内置静态扫描器自动执行；可选深度扫描由 Agent 按 `skill-vetter` 协议驱动执行。

---

## 第一部分：快速体验

### 核心概念

| 概念 | 说明 |
|------|------|
| **Manifest** | JSON 记录（`.skill-meta/latest.json`），包含文件哈希、扫描结果和数字签名；由 `scan`、`certify` 或 `init` baseline 创建和更新 |
| **版本链** | 只追加的账本——每个非根版本通过 `previousVersionId` 和 `previousManifestSignature` 指向已验真的父版本；恢复时可以建立新的签名链段 |
| **状态** | 每个 Skill 的安全状态：`pass` ✅ · `none` 🆕 · `drifted` 🔄 · `warn` ⚠️ · `deny` 🚨 · `tampered` 🔴 |

### 1. 初始化签名密钥

```bash
# 初始化密钥，并为已覆盖目录中的 Skill 建立快速扫描 baseline
agent-sec-cli skill-ledger init
```

baseline 是批量写操作。由 host 提供的只读已打包系统 Skill 会遵循
[只读的已打包系统 Skill](#只读的已打包系统-skill) 中的跳过契约；密钥初始化仍会完成。

`init --no-baseline` 只初始化密钥，不扫描 Skill。重复初始化复用密钥，返回
`keyCreated: false`、`key: null`；新建密钥时返回 `keyCreated: true`，密钥信息位于
`key`。`init --force-keys` 更换密钥对，并归档原公钥供签名校验。

密钥存放位置：

| 文件 | 路径 | 权限 |
|------|------|------|
| 私钥文件 | `~/.local/share/agent-sec/skill-ledger/key.enc` | 0600；默认未加密，`--passphrase` 时加密 |
| 公钥 | `~/.local/share/agent-sec/skill-ledger/key.pub` | 0644 |

如需口令保护私钥：

```bash
# 交互式输入口令
agent-sec-cli skill-ledger init --passphrase

# 或通过环境变量（适用于 CI）
SKILL_LEDGER_PASSPHRASE="your-secret" agent-sec-cli skill-ledger init --passphrase
```

### 2. 检查 Skill 完整性

```bash
agent-sec-cli skill-ledger check /path/to/your-skill
```

输出 JSON，关键字段为 `status`：

| 状态 | 含义 |
|------|------|
| `none` 🆕 | `latest.json` 与任何版本 JSON/snapshot artifact 均不存在，或已验真且与当前文件匹配的 manifest 为 `scanStatus=none` |
| `pass` ✅ | manifest 验真成功 + 文件未变 + 扫描通过 |
| `drifted` 🔄 | manifest 验真成功，但 live Skill 与已签名 fileHashes 不同；这是尚未扫描的内容分歧，不是 scanner 已确认的风险结论 |
| `warn` ⚠️ | manifest 验真成功，但上次扫描存在低风险发现 |
| `deny` 🚨 | manifest 验真成功，但上次扫描存在高危发现 |
| `tampered` 🔴 | Ledger metadata 的 schema、哈希、签名、已签名身份或 latest/版本 artifact 一致性校验失败，包括历史 artifact 仍存在但 `latest.json` 缺失、缺少签名或回放旧的已签名 latest |

Skill Ledger 会先验证已有 manifest 的真实性，并将 `latest.json` 绑定到最新已验真的版本 artifact，再将其中的文件哈希与当前 Skill 比较。仅当 `latest.json` 和版本 JSON/snapshot artifact 均不存在时，缺少 latest 才表示 `none`；若历史 artifact 仍存在，则 Ledger 不完整，返回 `tampered`。因此，即使当前文件同时发生变化，缺少签名、签名无效或回放旧的已签名 latest 仍返回 `tampered`；只有已验真的当前 manifest 才可能返回 `drifted`。

### 3. 快速扫描 + 签名认证

如需在认证前执行机器可调用的只读评估，可运行：

```bash
agent-sec-cli skill-ledger analyze /path/to/your-skill --format json
```

`analyze` 会针对当前目录依次运行 `code-scanner` 和 `static-scanner`。它不会
创建密钥、`.skill-meta`、manifest、snapshot、签名、配置项或安全事件。投稿
服务可以将结果作为增量信号，但不能用它替代已有内容规则和审核策略。

进程契约如下：

| 退出码 | 含义 |
|--------|------|
| `0` | 覆盖完整；通过 `status` 判断 `pass`、`warn` 或 `deny` |
| `1` | scanner 或文件未能完整覆盖；`status=error` 且 `coverage_complete=false` |
| `2` | 输入或协议使用错误，包括缺少 `SKILL.md` |

协议错误包括缺少 Skill 根目录参数（`skill-root-required`）和不支持的输出格式
（`unsupported-format`）；两者均返回退出码 `2` 和 JSON 错误负载。

调用方必须同时检查退出码、顶层 `status` 和 `coverage_complete`。Findings
按 `file`、`line`、`rule` 排序；scanner 结果固定先输出 `code-scanner`，再输出
`static-scanner`。随包 JSON Schema 位于
`agent_sec_cli/skill_ledger/analyze.schema.json`。
分析最多接受 2,000 个普通文件、50 MiB 文件总量和 32 层目录深度；超过任一限制
都会返回覆盖不完整。

Node.js subprocess 示例：

```javascript
import { spawn } from "node:child_process";

const child = spawn(
  "agent-sec-cli",
  ["skill-ledger", "analyze", skillDir, "--format", "json"],
  { stdio: ["ignore", "pipe", "pipe"] },
);

let stdout = "";
child.stdout.setEncoding("utf8");
child.stdout.on("data", (chunk) => {
  stdout += chunk;
});
child.on("close", (code) => {
  const result = JSON.parse(stdout);
  if (code !== 0 || result.status === "error" || !result.coverage_complete) {
    throw new Error("Skill analysis did not complete");
  }
  // 保留现有投稿规则，将 result.scanners 作为补充证据。
});
```

`analyze` 当前随完整的 `agent-sec-cli` wheel 和 RPM 交付。后续可将共享 scanner
提取为 scanner-only wheel 或 RPM 子包，但 scanner 规则必须继续保持单一源码。

#### 只读的已打包系统 Skill

`scan` 是有状态的认证命令：成功时会把 scanner 结果、签名 manifest 和 snapshot
写入 `.skill-meta`。它不是 `analyze` 的只读别名。

对于 `/usr/share/anolisa/skills/` 或 `/usr/local/share/anolisa/skills/` 的直接
子目录中由 host 提供的已打包 Skill，如果账本状态不可写，批量写路径会跳过该项。
`scan --all` 和默认的 `init` baseline 都会返回以下逐 Skill 结果：

```json
{
  "canonicalSkillDir": "/usr/share/anolisa/skills/example",
  "skillName": "example",
  "status": "skipped",
  "reasonCode": "readonly_system_skill",
  "persisted": false
}
```

`skipped` 是批量操作状态，不是六种完整性状态之一。它不表示 `pass`，不构成 Skill
认证，也不会单独使批量命令失败；除非其它项返回 `status=error`，命令退出码为 `0`。
跳过项不会写入逐 Skill scanner 结果、manifest、snapshot 或 `.skill-meta` 状态，
全局密钥初始化行为保持不变。显式 `scan <dir>` 仍保持严格语义：退出码为 `1`，并提示
调用方使用 `analyze` 获取只读 findings。

同样的批量跳过规则也适用于 `$XDG_DATA_HOME/anolisa/skills/`（默认
`~/.local/share/anolisa/skills/`）直接子目录中由 host 提供的 Skill：
当账本状态不可写且未被 `managedSkillDirs` 覆盖时，返回
`reasonCode=readonly_default_skill`。这包括镜像中对运行用户只读的 raw 用户 Skill。
可写的 raw 用户 Skill 正常扫描；跳过项不会加入 `managedSkillDirs`。
显式扫描和已纳管用户 Skill 的写入仍会因权限错误而失败。
SkillFS backing 和 resolver 的错误仍按错误处理。

`check` 和 `status` 的语义不变。若跳过项此前不存在任何账本 artifact，`check`
返回 `none`，聚合健康度仍可能为 `unscanned`；这些值不会把批量跳过转化为认证或
安全结论。

默认认证路径使用内置快速扫描器，不依赖 LLM。对单个 Skill 执行：

```bash
agent-sec-cli skill-ledger scan /path/to/your-skill
```

扫描完成后，可重新检查状态：

```bash
agent-sec-cli skill-ledger check /path/to/your-skill
```

如需更完整的语义审查，可通过 Agent 触发深度扫描。Agent 读取内置的 `skill-vetter-protocol.md` 扫描协议，逐文件对目标 Skill 进行四阶段审查（来源验证 → 代码审查 → 权限边界评估 → 风险分级），将结果写入 findings JSON 文件。随后将 findings 文件传入 `certify` 完成签名认证：

```bash
agent-sec-cli skill-ledger certify /path/to/your-skill \
  --findings /tmp/skill-vetter-findings-your-skill.json \
  --scanner skill-vetter \
  --delete-findings
```

`scan` 会运行内置快速扫描器并签名入账；`certify` 则只导入外部 findings。`certify` 会依次：

1. 先验证已有 manifest 的真实性，再验证文件一致性（文件变更或 manifest 无效时自动创建新版本）
2. 规范化 findings 并合并到 manifest 的 `scans[]` 数组
3. 聚合 `scanStatus`（`pass` / `warn` / `deny`）
4. 重新签名并写入 `.skill-meta/latest.json`

已有 manifest 无效或缺少签名时，CLI 不会原地签名，也不会继承其中的扫描结果或用户决策。恢复操作会创建新版本，并且只链接身份、哈希、签名和 snapshot 均通过校验的最新历史版本。若不存在这样的父版本，新签名版本会将两个 previous 字段都置为 `null`，成为新的链段根。

输出示例：

```json
{
  "versionId": "v000002",
  "scanStatus": "pass",
  "newVersion": true,
  "skillName": "your-skill"
}
```

### 4. 查看整体安全状况

```bash
# 查看 skill-ledger 系统整体状况（密钥、配置、所有 Skill 健康度）
agent-sec-cli skill-ledger status

# 包含每个 Skill 的详细状态
agent-sec-cli skill-ledger status --verbose
```

`status` 输出 JSON，包含三个区块：

| 区块 | 说明 |
|------|------|
| `keys` | 签名密钥状态（是否初始化、指纹、是否加密、归档密钥数） |
| `config` | 配置摘要（默认目录、managedSkillDirs 模式数、已注册扫描器） |
| `skills` | 聚合健康度（已发现 Skill 数、各状态计数、整体 health 标签） |

`health` 标签含义：`healthy`（没有 critical/attention 状态，且不是全部 none；可能包含 pass/none 混合）、`unscanned`（全部 none）、`attention`（存在 drifted/warn）、`critical`（存在 deny/tampered/error）、`empty`（无已注册 Skill）。

`none` 表示没有可用的已认证 scanner verdict，既可能是账本 artifact 不存在，也可能
是已验真 manifest 的 `scanStatus` 为 `none`；`unscanned` 表示所有已发现 Skill 当前
都检查为 `none`。二者都不能解释为 `pass`。批量写操作跳过的只读已打包系统 Skill
如果没有既有账本 artifact，会继续处于这些已有状态。

使用 `--verbose` 时会额外输出 `results` 数组，包含每个 Skill 的详细检查结果。

### 5. 审计完整版本链

深度验证全部历史版本——校验 schema、哈希、签名、已签名身份和显式父版本链接。父链接必须指向编号更早且通过验真的版本，并携带该父版本的准确签名；两个 previous 字段均为 `null` 的已签名版本是合法链段根。无效历史版本仍会使整体 audit 失败。

```bash
agent-sec-cli skill-ledger audit /path/to/your-skill

# 同时验证快照文件哈希
agent-sec-cli skill-ledger audit /path/to/your-skill --verify-snapshots
```

### 6. Agent 驱动扫描（推荐方式）

最自然的使用方式是通过 AI Agent 自然语言触发。默认“扫描”会执行快速扫描；只有用户明确要求深度扫描，或在快速扫描后确认继续，才执行 `skill-vetter` 深度扫描：

| 说法 | 效果 |
|------|------|
| "扫描 /path/to/skill" | 对指定 Skill 执行快速扫描认证 |
| "扫描所有 skill" | 批量快速扫描 `config.json` 中配置的所有 Skill |
| "深度扫描 /path/to/skill" | 按 `skill-vetter` 协议执行逐文件深度审查并认证 |
| "检查 skill 状态" | 仅输出状态分诊表，不执行扫描 |

Skill 工作流：

- **Phase 1**（环境准备与状态查看）：校验 CLI、密钥，解析目标 Skill，输出分诊表
- **Phase 2**（快速扫描认证）：调用内置 `code-scanner` 与 `static-scanner`，再签名写入 manifest
- **Phase 3**（可选深度扫描）：`skill-vetter` 四阶段审查——来源验证 → 代码审查 → 权限边界评估 → 风险分级，再通过 `certify --findings` 写入版本链

---

## 第二部分：通过 SkillFS 激活、用户决策与宿主 Hook Policy 保护 Skill 安全

### 架构概览

Skill Ledger 推荐与 SkillFS 联合使用：SkillFS 捕获 Skill 变更，通知 Skill Ledger daemon 扫描并刷新 `.skill-meta/activation.json`/xattr。宿主 hook/capability 仍作为兼容路径挂载；有原生提示或确认能力的宿主默认 `policy = "ask"`，Hermes 因 Python plugin API 未暴露这两种机制而默认 `policy = "observe"`。

```
┌──────────────────────────────────────────────────┐
│                  Agent 运行时                      │
│                                                   │
│  ┌──────────────┐      ┌──────────────────────┐   │
│  │  SkillFS     │      │  skill-ledger        │   │
│  │  变更捕获      │      │  SKILL.md            │   │
│  │               │      │  (按需深度扫描)       │   │
│  │      │        │      └──────────┬───────────┘   │
│  │      ▼        │                 │               │
│  │ daemon notify │                 │               │
│  │      │        │                 │               │
│  │      ▼        │                 │               │
│  │ activation    │                 │               │
│  │ refresh       │                 │               │
│  └──────┤────────┘                 │               │
│         ▼                         ▼               │
│  ┌──────────────────────────────────────────┐     │
│  │       agent-sec-cli skill-ledger          │     │
│  │   show / export / decide / scan / certify │     │
│  └──────────────────────────────────────────┘     │
│                      │                            │
│                      ▼                            │
│           .skill-meta/latest.json                 │
│           .skill-meta/activation.json + xattr     │
└───────────────────────────────────────────────────┘
```

- **推荐路径——SkillFS + daemon activation**：SkillFS 负责发现 Skill 文件变化；daemon 根据最新签名 manifest、用户决策和 activation policy 刷新可执行 activation 目标。Agent 运行时读取 activation metadata，而不是默认依赖宿主 hook 前置检查。
- **兼容路径——宿主 hook/capability policy**：OpenClaw、Hermes、copilot-shell 和 Qwen Code 在 Skill 加载前调用 `agent-sec-cli skill-ledger show`；Codex 和 Qoder CLI 则在各自的本地 Skill 触发边界调用只读的 `agent-sec-cli skill-ledger check`。Hermes 仅支持 `observe` / `block` 且默认 `observe`；其它宿主保留各自原生的 `observe` / `warn` / `ask` / `block` 行为和默认值。
- **Agent 驱动扫描**：`scan` 执行内置快速扫描并签名；`skill-ledger` Skill 在用户要求深度扫描时驱动完整的四阶段安全审查，并通过 `certify --findings` 导入结果。**按需触发**，由用户请求发起。

### 推荐路径：SkillFS + daemon activation

**工作原理：**

启用 SkillFS 后，Skill Ledger 的运行态入口由 daemon 处理：

1. SkillFS 捕获 Skill 目录创建、更新、删除或内容变更。
2. SkillFS 通知 Skill Ledger daemon 的 `skill_ledger.skillfs_notify_change` 接口。
3. daemon 根据签名 manifest、当前文件状态、用户决策和 activation policy 刷新 `.skill-meta/activation.json`，并尽力同步写入 xattr。
4. 若当前版本不可直接激活，activation metadata 会指向上一个可信 `pass` / `warn` snapshot；若没有可信 fallback，则指向安全 pending review stub；用户 `block` 决策或 fail-safe 场景才写 `target: null`。

**版本要求：SkillFS 必须 ≥ 0.4.0。**

第 2 步的 `skill_ledger.skillfs_notify_change` 从 0.4.0 起使用 **notify v2**：业务
payload 只有 `canonicalSkillDir`、`skillId`、`eventKind` 和 `paths` 四个字段。这是一次
**没有回退路径的破坏性升级** —— daemon 明确拒绝 `schemaVersion != 2` 的请求，SkillFS 侧
也不做版本协商。因此两个组件必须协调升级：

- 0.4.0 之前的 SkillFS 只发送 notify v1，会被当前 daemon 逐条拒绝；
- notify 投递失败在 SkillFS 侧只是 warning，不会中断 FUSE 服务。

版本不匹配的表现因此是**静默失效**：新装的 Skill 一直停留在 hidden，两侧都没有明显
报错。排查时先确认 `skillfs --version` ≥ 0.4.0。

**两个 socket，方向相反。** 联合部署最常见的接线错误来自把它们混为一谈：

| Socket | 谁监听 | 默认路径 | 用途 |
|---|---|---|---|
| daemon socket | agent-sec-core daemon | `$XDG_RUNTIME_DIR/agent-sec-core/daemon.sock`（`AGENT_SEC_DAEMON_SOCKET` 可覆盖） | SkillFS 用 `--notify-socket` 指向这里，发送变更通知 |
| control socket | SkillFS | `/run/user/<uid>/skillfs/control.sock`（Ledger 侧可用 `AGENT_SEC_SKILLFS_CONTROL_SOCKET` 覆盖） | daemon 反向查询 `skill.resolveLiveSource`，并写 activation 元数据 |

resolver 按以下顺序选择 control endpoint：embedding caller 显式传入的 `socket_path`、
`AGENT_SEC_SKILLFS_CONTROL_SOCKET`、上表中的 per-effective-UID 默认路径。最终路径必须与
SkillFS control socket 一致。完整 ACS 部署应让 SkillFS 与 daemon 使用相同 numeric UID，
确保跨容器的默认 endpoint 和 key owner 校验一致。

#### 使用 HMAC 的联合部署

legacy 部署可不启用 HMAC；完整 ACS 部署应在两个方向同时启用：

| 环境变量 | 读取方 | 用途 |
|---|---|---|
| `AGENT_SEC_SKILLFS_CONTROL_SOCKET` | Ledger resolver | 选择 SkillFS control socket |
| `AGENT_SEC_SKILLFS_CONTROL_AUTH_KEY_FILE` | Ledger resolver | 认证 control 请求与响应 |
| `AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE` | agent-sec-core daemon | 认证收到的 SkillFS 通知 |

例如，将以下环境变量提供给 agent-sec-core daemon 以及需要解析 SkillFS path 的 CLI
进程，并在 SkillFS 侧配置匹配的 socket 与 key 内容：

```bash
export AGENT_SEC_SKILLFS_CONTROL_SOCKET="/run/user/$(id -u)/skillfs/control.sock"
export AGENT_SEC_SKILLFS_CONTROL_AUTH_KEY_FILE="/run/agent-sec-keys/skillfs-control.key"
export AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE="/run/agent-sec-keys/skillfs-notify.key"

skillfs mount /path/to/skills /mnt/skillfs \
  --security --activation-mode file \
  --notify-socket "$XDG_RUNTIME_DIR/agent-sec-core/daemon.sock" \
  --notify-auth-key-file "$AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE" \
  --control-socket "$AGENT_SEC_SKILLFS_CONTROL_SOCKET" \
  --trusted-peer-key-file "$AGENT_SEC_SKILLFS_CONTROL_AUTH_KEY_FILE"
```

control 与 notify 环境变量可以指向同一个 key 文件，但推荐为两个方向使用独立 key。
control key 在第一次 resolve 时加载，并缓存到该 resolver client 生命周期结束；不支持
热更新。daemon 在 bind socket 前加载 notify key，因此非法 notify key 会阻止 daemon 启动。

每个 key 文件都必须通过以下校验：

- 配置路径是绝对路径；
- 目标是普通文件，而不是 symlink、目录、FIFO 或 device；
- 文件 owner 是读取进程的 effective UID；
- 不设置任何 group/other 权限位（推荐 mode `0600`）；
- 未经修改的文件内容长度为 32–4096 bytes。

control resolver 会明确区分 legacy 可用性回退和已认证部署要求：

| Control key | Socket `ENOENT` | 其他连接、认证或协议错误 |
|---|---|---|
| 未配置 | 回退到 canonical host path | 返回 `skill_root_resolve_failed` |
| 已配置 | 返回 `skill_root_resolve_failed` | 返回 `skill_root_resolve_failed` |

通过认证的 `managed=false` 仍是可信的未覆盖结果，会使用 canonical host path。配置 control
key 后，Ledger 不会再以明文重试请求。

notify 处理遵循相同的防降级边界：

| Notify key | 行为 |
|---|---|
| 未配置 | 保留 legacy 明文 notify；收到认证握手时拒绝 |
| 已配置 | `skill_ledger.skillfs_notify_change` 必须使用 HMAC；拒绝明文 notify，daemon 其他 method 继续使用现有明文协议 |

容器化 ACS 部署需要在对应容器间共享两个 Unix socket 目录和所需 key 内容。各进程使用
相同 numeric UID，并确保挂载后的 key 由该 UID 所有。Kubernetes Secret volume 使用基于
symlink 的 projection，key loader 会有意拒绝这种路径。应将各 Secret 复制到 `emptyDir`
等私有共享卷，在那里设置 owner 和 mode，再让环境变量指向生成的普通非 symlink 文件；
不要直接指向 projected Secret 路径。

Hermes 布局下 activation 流程携带的是嵌套身份（`category/skill`），而非扁平 skill 名；
`skillId` 会保留两个分量。

完整的协议定义、canonical path 语义和部署边界见
[Skill Ledger 的 SkillFS 集成设计](../../../../../src/agent-sec-core/docs/design/SKILL_LEDGER_SKILLFS_INTEGRATION_zh.md)
与 [SkillFS 用户指南](../../runtime/skillfs.md)。

### 统一宿主 Hook 控制

宿主 adapter 使用 `SKILL_LEDGER_HOOK_ENABLED` 作为总开关，使用
`SKILL_LEDGER_MODE` 选择行为。开关默认 `true`；有原生提示或确认能力的宿主默认
`ask`。Hermes 默认 `observe` 且仅支持 `observe` / `block`；旧 `warn` / `ask` 会降级
为 `observe` 并写宿主诊断。`observe` 执行检查和审计但不显示用户提示；旧值 `debug`
映射为 `observe`，旧值 `deny` 映射为 `block`。环境变量 policy 优先于 Hermes/OpenClaw
capability 配置。

宿主 Agent 在加载插件时读取这些变量。修改后需重启承载该 hook 的 Agent 进程；
hook 和 agent-sec-core 并不是需要单独重启的 policy 服务。

Hermes 以外的宿主在 hook 无法请求确认时可将 `ask` fallback 为 `warn`。Hermes 不会
通过改写助手最终回复模拟这两种 action，不支持的值统一降级为 `observe`。其它宿主在
当前边界无法执行阻断时也不得声称已经阻断。设置
`SKILL_LEDGER_HOOK_ENABLED=false` 后不读取业务输入、不初始化密钥，也不调用 CLI。

### 兼容路径：Hook / capability policy

当 Agent 加载 Skill 时，OpenClaw、Hermes、copilot-shell 和 Qwen Code hook 会解析 Skill 目录，执行 `agent-sec-cli skill-ledger show <skill_dir>`，并由统一 `policy` 控制宿主特定行为。这些 hook 只消费 summary 中的 `message`：

| Policy | 行为 |
|--------|------|
| `observe` | `message != null` 时只写审计/debug 诊断并放行。 |
| `warn` | `message == null` 静默放行；`message != null` 时展示 warning 并放行。 |
| `ask` | 默认值。`message == null` 静默放行；`message != null` 时请求用户确认或使用宿主 approval UI。 |
| `block` | `message != null` 时直接阻断，并把 message 作为原因或告警信息。 |

Hermes 只实现 `observe` 和 `block` 两行；兼容的 `warn` / `ask` 会转为 `observe` 并记录
诊断，绝不通过 `transform_llm_output` 写入助手回复。

`message` 的触发规则由 Skill Ledger 统一决定：用户已有 `allow` / `always_allow` / `rollback` / `block` 决策时不提示；latest 为 `pass` 或 `warn` 且可直接暴露时不提示；无用户决策且 latest 为 `deny` / `none` / `drifted` / `tampered` 时提示，并说明当前 active 是 fallback 版本还是安全 pending review stub。`latestStatus=unmanaged` 表示当前 daemon 无法管理该 root，无法写 `.skill-meta` 或记录用户决策，因此只作为诊断返回，`message=null`，所有 hook policy 包括 `block` 都静默放行。

Codex 和 Qoder CLI 是低层完整性门禁，均在完成 canonical path 和根目录边界校验后执行 `skill-ledger check <skill_dir>`。Codex 在 `UserPromptSubmit` 解析 `$skill-name`；该边界无法请求确认，因此 `ask` fallback 为 `warn`。Qoder CLI 为 `Skill` tool 注册独立的 `PreToolUse` hook，根据事件中的绝对 `cwd` 建立 user → project 目录表，并解析 `SKILL.md` frontmatter `name`（无 frontmatter 时回退目录名）。Qoder frontmatter 存在但 `name` 缺失、歧义或使用 hook 无法安全解析的 YAML scalar 时，不会降级为非本地 Skill，而是按当前 policy 处理。`pass` 静默放行；`none` / `drifted` / `warn` / `deny` / `tampered` 以及 `error` 按 `observe` / `warn` / `ask` / `block` policy 静默审计、提示后放行、在支持的边界请求确认或阻断。Qoder CLI 不可用、执行失败、超时或输出不可解析也按该四档 policy 处理，而不是固定 fail-open；旧值 `debug` 仅作为 `observe` 的兼容别名。

六个 adapter 均默认启用 Skill Ledger；Hermes 使用 `observe`，其它 adapter 保持 `ask`。copilot-shell、Codex、Qoder CLI 和 Qwen Code 在默认 manifest 注册各自的 hook 边界。OpenClaw 和 Hermes 还可使用 capability 配置，`SKILL_LEDGER_MODE` 仍作为部署级覆盖。除上述明确说明的 Qoder CLI 低层门禁外，其它兼容 hook 在 CLI 基础设施异常时保持 fail-open，避免阻断 Skill 加载。

Ledger 内置发现项也包含 raw 用户目录。对此目录，`XDG_DATA_HOME`
未设置、为空、为相对路径或包含 `.`、`..` 路径段时使用 `~/.local/share`，
与 ANOLISA 和 cosh 保持一致。`enableDefaultSkillDirs=false` 也会禁用此内置项。
hook 保留现有 policy 和未管理 Skill 的处理规则；识别目录不代表认证其内容。

`agent-sec-cli capabilities --agent cosh --capability skill-ledger --output json`
会显示生效的 XDG 数据根目录设置，不解析用户 home，也不读取 Agent 配置；
回退值显示为 `~/.local/share`。

copilot-shell hook 当前仅覆盖 project / user / system 三类目录：`<cwd>/.copilot-shell/skills/`、`~/.copilot-shell/skills/`、raw 用户目录 `$XDG_DATA_HOME/anolisa/skills/`（默认 `~/.local/share/anolisa/skills/`），以及 RPM 与 raw install 对应的 system 根目录 `/usr/share/anolisa/skills/` 和 `/usr/local/share/anolisa/skills/`。若 Skill 来自 custom、extension、remote 或其它路径，hook 会 fail-open 并跳过 skill-ledger 检查；OpenClaw 插件则按读取到的 `SKILL.md` 路径提取 Skill 目录。

批量认证或安装后认证场景中，建议先完成目录定位和认证，再让 Agent 读取未认证 Skill 内容：批量认证前避免主动读取未认证 Skill 的 `SKILL.md` 或辅助文件；安装成功后应先定位最终本地目录，确认包含 `SKILL.md`，再执行快速扫描认证。

**OpenClaw 启用方式**：

```json
{
  "capabilities": {
    "skill-ledger": {
      "enabled": true,
      "policy": "ask"
    }
  }
}
```

**Hermes 启用方式**：

```toml
[capabilities.skill-ledger]
enabled = true
timeout = 5
policy = "observe"
```

Hermes 只支持 `observe` 和原生 `block`。已有 `warn` / `ask` 值会降级为 `observe` 并写
宿主诊断；旧配置中的 `enable_block=false` 同样映射为 `observe`。
Hermes `observe` 模式下，非空 exposure summary 写 `INFO`；`deny` / `tampered` 状态或
`reasonCode=tampered` 的激活结果提升为 `WARNING`。这些日志等级不会阻断 Skill，也不会
向助手回复写入内容。

**copilot-shell 配置方式**：默认 Cosh manifest 已注册 `skill-ledger` hook。默认 policy 为 `ask`；如需 observe-only、warning-only 或强拒绝，可设置 `SKILL_LEDGER_MODE=observe` / `warn` / `block`。`debug` 仍作为 `observe` 的别名。该环境变量应由可信宿主或部署环境设置，不应由 Skill、项目脚本或不可信 shell 启动逻辑设置；如需防止本地 shell profile 被篡改后降级策略，后续应迁移到可信宿主配置源。

**Qoder CLI 配置方式**：安装 `qoder-plugin` 后，plugin 自动注册 matcher 为 `Skill` 的 `PreToolUse` hook。默认 policy 为 `ask`；可由可信启动环境设置 `SKILL_LEDGER_MODE=observe` / `warn` / `block`，并通过 `SKILL_LEDGER_TIMEOUT` 调整 CLI 超时（默认 5 秒）。`debug` 仍作为 `observe` 的别名。hook 覆盖 `~/.qoder/skills/` 和 `<cwd>/.qoder/skills/` 下的本地 Skill，用户级同名 Skill 优先；仅在两个目录表都可信解析且没有匹配时，才把调用视为内置、plugin 或 remote Skill，放行并记录 debug。hook 不自动执行 `init` 或 `scan`。`latest.json` 与任何版本 JSON/snapshot artifact 均不存在的 Skill 以 `none` 状态进入 policy；历史 artifact 仍存在但 `latest.json` 缺失，或已有 latest manifest 缺少签名、签名无效时，则以 `tampered` 进入。完成审查后需显式执行 `agent-sec-cli skill-ledger scan <skill_dir>`。

Skill Ledger 全局 `activationPolicy` 属于 SkillFS/daemon activation；这里的 hook `policy` 只控制宿主 hook/capability 的用户可见行为和日志等级。

### 非 pass Skill 用户审查与决策

当 hook 或 `show` 提示当前 skill 需要用户审查时，先查看统一暴露摘要：

```bash
agent-sec-cli skill-ledger show /path/to/skill
```

重点字段：

| 字段 | 含义 |
|------|------|
| `latestStatus` | 最新 skill 根目录或最新签名版本的状态 |
| `activeVersionId` | 当前真实暴露给 SkillFS 的版本；为 `null` 时表示没有真实 active version |
| `target` | SkillFS 当前读取的 target；pending 状态会指向 `.skill-meta/versions/__pending_decision__.snapshot` |
| `userDecision` | 当前命中的用户决策；为 `null` 表示尚未决策 |
| `message` | 需要提示用户的信息；为 `null` 时 hook 静默 |

若需要完整查看未暴露的待审查版本，导出 latest snapshot、manifest 和 findings：

```bash
agent-sec-cli skill-ledger export /path/to/skill --version latest --output /tmp/skill-review
```

审查后通过统一 `decide` 命令选择：

```bash
# 允许当前具体版本；不继承到未来版本
agent-sec-cli skill-ledger decide /path/to/skill --action allow --reason "reviewed manually"

# 允许当前及未来版本，直到用户主动改成其它决策或清除
agent-sec-cli skill-ledger decide /path/to/skill --action always_allow --reason "trusted source"

# 完全隐藏当前 skill；该 block 不继承到未来新版本
agent-sec-cli skill-ledger decide /path/to/skill --action block --reason "unsafe behavior"

# 回退到指定版本；不写 --version 时默认选择当前真实 active version
agent-sec-cli skill-ledger decide /path/to/skill --action rollback --version v000001 --reason "use previous trusted version"

# 清除 latest manifest 上的用户决策，恢复全局 activation 行为
agent-sec-cli skill-ledger decide /path/to/skill --clear
```

注意：hook 的 `ask` 确认只允许本次宿主操作继续，不等价于 Skill Ledger 的 `allow`。只有 `decide` 会改变后续 activation target。

### Agent 驱动深度扫描

#### 配置 Skill 目录（批量扫描使用）

默认已包含以下静态目录及 raw 用户目录：`~/.openclaw/skills/*`、`~/.copilot-shell/skills/*`、`~/.hermes/skills/**`、`~/.qoder/skills/*`、`/usr/share/anolisa/skills/*`、`/usr/local/share/anolisa/skills/*`。项目级 Qoder 目录不作为相对默认项；对项目 Skill 显式执行 `scan` 或 `certify` 后，其绝对目录会沿用自动记忆机制写入 `managedSkillDirs`。如需添加其它目录，创建或编辑 `~/.config/agent-sec/skill-ledger/config.json`：

```json
{
  "enableDefaultSkillDirs": true,
  "managedSkillDirs": [
    "/opt/custom-skills/*",
    "/opt/custom-skills/my-skill"
  ]
}
```

默认目录默认启用；`managedSkillDirs` 用于 skill-ledger 动态管理或用户额外配置的目录，会追加到默认目录之后（自动去重）。如需隔离运行，可将 `enableDefaultSkillDirs` 设为 `false`。

- `"path/*"` — glob 模式：每个包含 `SKILL.md` 的子目录视为一个 Skill
- `"path/to/skill"` — 单个 Skill 目录（同样需包含 `SKILL.md`）

不存在的目录会被静默忽略。此外，对 Skill 执行 `scan` 或 `certify` 时，未收录的目录会自动追加到配置中，方便后续 `--all` 批量操作。`check` 是只读状态检查，不会写入配置。

对于 raw 用户根 `$XDG_DATA_HOME/anolisa/skills/`（默认
`~/.local/share/anolisa/skills/`）的直接子 Skill，自动记忆仅登记该 Skill
自身路径，即使存在其它兄弟 Skill 也不扩大为父目录通配符。因此，扫描可写 Skill
不会把只读兄弟 Skill 一并加入 `managedSkillDirs`。其它根目录保留现有父目录通配符规则。
已有配置条目不会自动改写：已有通配符覆盖的用户 Skill 仍视为已纳管，写入失败仍会报错。
若确认 raw 根通配符属于误添加，可将它替换为实际需要纳管的各个 Skill 路径，保留有意配置的覆盖范围。

#### 定时执行默认快速扫描

如果希望定期刷新默认快速扫描结果，可以把 `scan --all` 放入 cron。`scan --all` 会自动跳过文件未变且已有完整扫描结果的 Skill，只补扫新增、变更、缺少扫描结果或 manifest 异常的 Skill。对于由 host 提供的只读已打包系统 Skill，它还会返回 `status=skipped` 和 `reasonCode=readonly_system_skill`；本次运行不会为这些项创建或刷新认证，因此应检查 JSON 结果。

无口令密钥场景：

```bash
mkdir -p "$HOME/.local/state/agent-sec"
AGENT_SEC_CLI="$(command -v agent-sec-cli)"
CRON_LINE="0 3 * * * $AGENT_SEC_CLI skill-ledger scan --all >> $HOME/.local/state/agent-sec/skill-ledger-scan.log 2>&1"
(crontab -l 2>/dev/null | grep -Fv "skill-ledger scan --all"; echo "$CRON_LINE") | crontab -
```

使用口令保护私钥时，定时任务需要提供 `SKILL_LEDGER_PASSPHRASE`。下面的命令会把口令以明文写入当前用户的 crontab 和系统 cron spool，请只在可信单用户环境中使用；更安全的做法是使用默认无口令密钥，或通过本机 secret manager / 受限权限文件包装 `scan --all`。

```bash
read -rsp "SKILL_LEDGER_PASSPHRASE: " SKILL_LEDGER_PASSPHRASE; echo
mkdir -p "$HOME/.local/state/agent-sec"
AGENT_SEC_CLI="$(command -v agent-sec-cli)"
CRON_LINE="0 3 * * * SKILL_LEDGER_PASSPHRASE='$SKILL_LEDGER_PASSPHRASE' $AGENT_SEC_CLI skill-ledger scan --all >> $HOME/.local/state/agent-sec/skill-ledger-scan.log 2>&1"
(crontab -l 2>/dev/null | grep -Fv "skill-ledger scan --all"; echo "$CRON_LINE") | crontab -
unset SKILL_LEDGER_PASSPHRASE
```

查看已安装的定时任务：

```bash
crontab -l
```

#### 触发扫描

通过自然语言向 Agent 发出指令即可。默认扫描执行 Phase 1 → Phase 2；用户明确要求深度扫描时执行 Phase 1 → Phase 3。

**深度扫描规则表（skill-vetter）：**

| 级别 | 规则 ID | 检测目标 |
|------|---------|---------|
| deny | `dangerous-exec` | 危险进程执行（`child_process`、`subprocess`） |
| deny | `dynamic-code-eval` | 动态代码执行（`eval()`、`new Function()`） |
| deny | `env-harvesting` | 环境变量批量采集 + 网络发送 |
| deny | `crypto-mining` | 挖矿特征（`stratum`、`xmrig` 等） |
| deny | `credential-access` | 凭据与敏感文件访问（`~/.ssh/`、`.env`） |
| deny | `system-modification` | 系统文件篡改（`/etc/`、crontab） |
| deny | `prompt-override` | Prompt 覆盖指令 |
| deny | `hidden-instruction` | 隐藏指令（零宽字符、HTML 注释） |
| warn | `obfuscated-code` | 代码混淆（超长行、base64 + decode） |
| warn | `suspicious-network` | 可疑网络连接（直连 IP、非标准端口） |
| warn | `exfiltration-pattern` | 数据外泄模式（文件读取 + 网络发送组合） |
| warn | `agent-data-access` | Agent 身份数据访问（`MEMORY.md` 等） |
| warn | `unauthorized-install` | 未声明的包安装 |
| warn | `unrestricted-tool-use` | 无约束工具使用指令 |
| warn | `external-fetch-exec` | 外部获取执行（`curl | bash`） |
| warn | `privilege-escalation` | 权限提升（`sudo`、`chmod 777`） |

### 实战场景

#### 场景 A：加载第三方 Skill 时检测篡改

```
# SkillFS/daemon 或宿主 hook 检测到异常状态
[skill-ledger] 🚨 Skill 'third-party-tool' metadata signature verification failed
```

告警表明有人可能修改了 manifest，将 `scanStatus` 从 `deny` 改为 `pass` 以绕过安全检查。

#### 场景 B：Skill 更新后检测漂移

```bash
agent-sec-cli skill-ledger check /path/to/my-skill
# → {"status": "drifted", "added": [...], "modified": [...]}
```

更新 Skill 后状态变为 `drifted`。这只表示 live root 与已签名版本不同，不是 scanner 已确认的风险结果。触发重新扫描认证新内容，并获得当前扫描状态：

```
扫描 /path/to/my-skill
```

#### 场景 C：审计历史完整性

```bash
agent-sec-cli skill-ledger audit /path/to/my-skill --verify-snapshots
```

逐版本验证：schema → 哈希完整性 → 签名有效性 → 已签名身份 → 显式父版本链接 → 快照一致性。

---

## 命令速查表

| 命令 | 用途 |
|------|------|
| `agent-sec-cli skill-ledger init` | 初始化密钥，并为已覆盖 Skill 建立快速扫描 baseline |
| `agent-sec-cli skill-ledger init --no-baseline` | 只初始化密钥，不扫描 Skill |
| `agent-sec-cli skill-ledger analyze <dir> --format json` | 只读分析当前内容，不创建账本状态或认证 |
| `agent-sec-cli skill-ledger check <dir>` | 检查完整性状态（JSON 输出） |
| `agent-sec-cli skill-ledger show <dir>` | 展示 latest、active、用户决策、activation target、findings 与告警信息 |
| `agent-sec-cli skill-ledger export <dir> --version latest --output <path>` | 导出指定 snapshot、manifest 和 findings 供完整审查 |
| `agent-sec-cli skill-ledger decide <dir> --action allow|always_allow|block|rollback` | 写入用户决策并刷新 activation |
| `agent-sec-cli skill-ledger decide <dir> --clear` | 清除 latest manifest 上的用户决策 |
| `agent-sec-cli skill-ledger scan <dir>` | 执行快速扫描并签名写入 manifest；由 host 提供的只读已打包系统 Skill 会报错 |
| `agent-sec-cli skill-ledger scan --all` | 执行补齐式快速扫描；由 host 提供的只读已打包系统 Skill 返回运行状态 `skipped` |
| `agent-sec-cli skill-ledger certify <dir> --findings <file>` | 将深度扫描 findings 签名写入 manifest |
| `agent-sec-cli skill-ledger status` | 查看整体安全状况（密钥、配置、Skill 健康度） |
| `agent-sec-cli skill-ledger status --verbose` | 查看整体安全状况（含每个 Skill 详细结果） |
| `agent-sec-cli skill-ledger audit <dir>` | 深度验证版本链 |
| `agent-sec-cli skill-ledger list-scanners` | 查看已注册的扫描器列表 |

`activationPolicy` 和 `show/export --policy` 接受 `pass_warn_only`。通过
`list-scanners` 查询 `init/scan --scanners`、`certify --scanner` 可用的注册名称；
自定义扫描器可在 `config.json` 中注册。

`rotate-keys` 在 help 中可见，尚未实现：调用时会在 stderr 报告 `not implemented`，以非零
退出码结束，且不会修改 `key.enc`、`key.pub` 或 keyring。

## 关键路径

| 路径 | 用途 |
|------|------|
| `~/.local/share/agent-sec/skill-ledger/key.enc` | 私钥文件（默认未加密，`--passphrase` 时加密） |
| `~/.local/share/agent-sec/skill-ledger/key.pub` | 公钥 |
| `~/.local/share/agent-sec/skill-ledger/keyring/` | 归档的历史公钥（密钥轮换后） |
| `~/.config/agent-sec/skill-ledger/config.json` | 配置文件（managedSkillDirs、scanners） |
| `<skill_dir>/.skill-meta/latest.json` | 当前 manifest（由 `scan`、`certify` 或 `init` baseline 写入） |
| `<skill_dir>/.skill-meta/versions/` | 版本链历史 |

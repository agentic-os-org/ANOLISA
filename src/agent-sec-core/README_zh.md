# Agent Sec Core

[English](README.md)

**面向 AI Agent 的 OS 级安全内核。** 为 Agent 负载提供纵深防御：提示词注入检测、
代码扫描、PII 检测、Skill 完整性追踪、系统基线加固、沙箱隔离，以及本地安全事件
存储。全部本地运行，无 Token 消耗。适用于 [ANOLISA](../../README_zh.md) 等 AI Agent
运行平台，以及下文列出的六个 Agent 宿主。

[![AARM Aligned](https://img.shields.io/badge/AARM-Aligned-blue.svg)](https://aarm.dev/builders/agentseccore-anolisa)

**AgentSecCore (ANOLISA)** 已登记于 [AARM Builder Registry](https://aarm.dev/builders/agentseccore-anolisa)，当前状态为 **Aligned**。

AgentSecCore 的相关安全控制已纳入 [ANOLISA OWASP Agentic Top 10 控制映射](../../docs/user-guide/zh/agent-security/owasp-agentic-top10.md)。

## 背景

随着 AI Agent 逐步获得操作系统级别的执行能力（文件读写、网络访问、进程管理等），传统应用安全边界已不再适用。Agent Sec Core 从 **OS 层面** 为 Agent 构建纵深防御体系，确保 Agent 在受控、可审计、最小权限的环境中运行。

## 核心原则

1. **最小权限** — Agent 仅获得完成任务所需的最小系统权限。
2. **显式授权** — 敏感操作必须经过用户明确确认，禁止静默提权。
3. **零信任** — Skill 间互不信任，每次操作独立鉴权。
4. **纵深防御** — 执行前预防 → 运行时检测 → 内核级隔离，任一层失守不影响其他层。
5. **安全优先于执行** — 当安全与功能冲突时，安全优先；存疑时按高风险处理。

## 能力总览

| 模块 | 说明 | CLI 入口 |
|------|------|----------|
| **Prompt Scanner** | 提示词注入 / 越狱检测：规则引擎（L1）+ ML 分类器（L2），以及多轮意图检测（L4） | `agent-sec-cli scan-prompt` |
| **Code Scanner** | 对 bash / python 代码做静态危险操作分析 | `agent-sec-cli scan-code` |
| **PII Checker** | 个人数据与凭证检测，支持脱敏输出 | `agent-sec-cli scan-pii` |
| **Skill Ledger** | 基于 Ed25519 签名的 Skill 完整性账本，仅追加版本链 | `agent-sec-cli skill-ledger` |
| **Security Baseline** | 系统加固扫描与修复（封装 `loongshield seharden`） | `agent-sec-cli harden` |
| **Observability** | Agent 生命周期事件记录、会话复盘报告、交互式审查 TUI | `agent-sec-cli observability` |
| **Security Events** | 本地 JSONL + SQLite 事件存储，支持查询与聚合 | `agent-sec-cli events` |
| **Sandbox** | 系统调用级命令隔离（bubblewrap + seccomp），作为架构层使用 | `linux-sandbox` |

V1 后台守护进程 `agent-sec-daemon`（以 `agent-sec-core.service` systemd **user** unit
形式发布）提供健康检查、SkillFS 通知和安全查询 RPC。Prompt Scanner 通过 Rust 扩展
在进程内执行；daemon 不会预加载 Prompt Scanner 模型，也不提供扫描 RPC。

## V2 SkillSec 与 Policy CLI

Rust V2 核心通过现有 `skill-ledger` 命令提供 **SkillSec**：本地扫描、系统密钥签名、版本历史、
人工决策、回滚及 SkillFS 激活由一个 root daemon 统一处理。所有本地用户均可管理所有已注册 Skill，
仅 root 可以更换签名密钥。基线初始化与批量扫描、检查按调用者 XDG/HOME 规则发现 ANOLISA
用户安装的 Skill，同时保留已有的系统及 Agent 目录发现。配置、命令以及单独的部署和 Agent Hook 验收边界见
[V2 核心指南](../../docs/user-guide/zh/agent-security/agent-sec-core/skillsec-v2.md)。
不导入 V1 历史及用户密钥；本 PR 保留现有 Agent Hook 实现。

源码构建的 Rust `agent-sec-cli` 经 `asc-daemon` 提供全部 15 条 Policy、Scope、Binding
CRUD 命令。参阅[命令参考](../../docs/user-guide/zh/agent-security/agent-sec-core/policy-cli.md)
和 [V2 workspace](v2/README.md)。当前 PAP 状态仅在进程内存中，Binding 受理不代表策略已生效。
可通过 daemon 的 `--policy-admin-uid <UID>` 授权非 root Policy CLI 调用者；默认仍只授权 root。

V2 原生 OTel 目前仅用于本地日志关联，不提供公开 OTLP exporter。
现有 `--trace-context` JSON 传入 Agent 归属；`--otel-context`（version 1）传入上游
W3C 链路。`RUST_LOG=info` 开启 stderr 有界关联诊断，参阅
[上下文示例与日志限制](v2/README.md#native-opentelemetry-tracing)。

## 安全防护架构

```
┌─────────────────────────────────────────────────────────┐
│   Agent 宿主：cosh · OpenClaw · Hermes ·                 │
│               Qwen Code · Qoder · Codex                 │
├─────────────────────────────────────────────────────────┤
│   各宿主 Hook：code-scanner · prompt-scanner ·           │
│           pii-checker · skill-ledger · observability     │
├──────────────────────────┬──────────────────────────────┤
│  agent-sec-cli           │  agent-sec-daemon            │
│  scan-prompt / scan-code │  健康检查 + SkillFS 通知      │
│  scan-pii / skill-ledger │  安全查询 RPC                 │
│  harden / verify         │                              │
│  events / observability  │                              │
├──────────────────────────┴──────────────────────────────┤
│  Security Events（JSONL + SQLite）                       │
├─────────────────────────────────────────────────────────┤
│  linux-sandbox（bubblewrap + seccomp）                   │
├─────────────────────────────────────────────────────────┤
│  Linux Kernel · loongshield 基线                         │
└─────────────────────────────────────────────────────────┘
```

## 项目结构

```
agent-sec-core/
├── linux-sandbox/             # Rust 沙箱执行器（bubblewrap + seccomp）
│   ├── src/                   # Rust 源码（cli, policy, seccomp, bwrap_args, …）
│   ├── tests/                 # Rust 集成测试
│   └── docs/                  # dev-guide, user-guide
├── agent-sec-cli/             # 统一 CLI + 安全中间层（Python + Rust 扩展）
│   ├── src/agent_sec_cli/     # 主 Python 包
│   │   ├── cli.py             # CLI 入口点（Typer）
│   │   ├── asset_verify/      # Skill GPG 签名 + 哈希校验
│   │   ├── code_scanner/      # 代码扫描引擎（regex + llm）与规则集
│   │   ├── prompt_scanner/    # 提示词注入 / 越狱扫描器
│   │   ├── pii_checker/       # PII 与凭证检测
│   │   ├── skill_ledger/      # Ed25519 完整性账本与内置扫描器
│   │   ├── sandbox/           # 命令分类 + 沙箱策略生成
│   │   ├── observability/     # Observability 记录、report、审查 TUI
│   │   ├── security_events/   # JSONL + SQLite 事件存储
│   │   ├── security_middleware/ # 中间层 + 后端实现
│   │   ├── daemon/            # agent-sec-daemon 服务端与客户端
│   │   ├── model_service/     # 本地模型后端（如 Ollama）
│   │   └── telemetry/         # Telemetry schema 与写入器
│   ├── dev-tools/             # 后端扩展开发指南
│   └── pyproject.toml         # 构建配置
├── cosh-extension/            # Copilot Shell hooks + 沙箱 guard
├── openclaw-plugin/           # OpenClaw 插件（TypeScript）
├── hermes-plugin/             # Hermes 插件（Python capabilities）
├── qwen-code-extension/       # Qwen Code hooks
├── qoder-plugin/              # Qoder CLI hooks
├── codex-plugin/              # Codex hooks
├── skills/                    # 随包提供的安全扫描与审计 Skill
├── tools/                     # sign-skill.sh — PGP 技能签名工具
├── packaging/                 # raw 包构建 + systemd unit 模板
├── scripts/                   # CLI/daemon wrapper 与 CI 辅助脚本
├── docs/design/               # 设计文档
├── tests/                     # 单元测试、集成测试、打包测试、端到端测试
├── .anolisa/component.toml    # ANOLISA 组件契约
├── LICENSE
├── Makefile
├── agent-sec-core.spec.in     # RPM 打包 spec 模板
├── README.md
└── README_zh.md
```

六个 Agent 宿主均已实现全部五类 hook（code-scanner、prompt-scanner、pii-checker、
skill-ledger、observability）；各宿主支持的处置模式不同，见
[Agent Hook 环境变量](#agent-hook-环境变量)。

各 adapter 专项说明：
[OpenClaw](openclaw-plugin/README.md) ·
[Hermes](hermes-plugin/README.md) ·
[Codex](codex-plugin/README.md) ·
[Qwen Code](qwen-code-extension/README.md)

## Observability Hook 配置

OpenClaw、Hermes、cosh、Qwen Code、Qoder 和 Codex 集成默认都会启用
Observability hook。若需关闭，请在启动对应宿主前设置：

```bash
export OBSERVABILITY_HOOK_ENABLED=false
```

该变量仅接受 `true` / `false`（忽略大小写和首尾空白）；未设置或值无效时保持默认开启。
修改后需重启对应宿主进程。

`OBSERVABILITY_TIMEOUT` 控制每次本地 PII 脱敏和 Observability 数据写入 CLI 调用的超时秒数。
其余五个非 Hermes 集成默认使用 `5`；未设置、为空、非法或非正数时也使用 `5`。
Hermes 则回退到 Observability capability 的 `timeout`，并将其封顶为 `5`；有效环境变量
大于 `5` 时，所有集成都封顶为 `5`。

对于 OpenClaw 和 Hermes，原有 Observability capability 的 `enabled` 配置仍是独立开关。
任一开关关闭都会停止记录；将该环境变量设为 `true`，不会重新启用已在插件配置中关闭的
capability。

## 快速开始

### 前置条件

| 组件 | 要求 |
|------|------|
| **操作系统** | Alibaba Cloud Linux / Anolis / RHEL 系列 |
| **权限** | root 或 sudo（system mode 安装） |
| **loongshield** | >= 1.2.0（Security Baseline 后端） |
| **gpg / gnupg2** | >= 2.0（资产签名校验） |
| **Python** | 3.11.6（已固定；RPM 要求 `>= 3.11, < 3.12`） |
| **Rust** | >= 1.93（用于构建 `linux-sandbox` 与 CLI 原生扩展） |
| **bubblewrap** | `linux-sandbox` 运行依赖 |
| **ANOLISA CLI** | >= 0.2.17 |

### 安装 AgentSecCore

源码和 RPM 安装支持 Linux x86_64、aarch64。已发布的 ANOLISA raw 包仅支持
Linux x86_64 和 system mode，需要使用 0.2.17 或更高版本的 CLI。请根据 CLI
的安装来源完成更新。

```bash
# 通过 get.agentic-os.sh 安装的 CLI
anolisa update self

# 由 RPM 管理的 CLI
sudo anolisa update self

sudo anolisa --install-mode system install sec-core
sudo anolisa status sec-core
agent-sec-cli --version
```

`sec-core` 是 ANOLISA 中的组件名，RPM 继续使用包名 `agent-sec-core`。

```bash
sudo yum install anolisa agent-sec-core
sudo anolisa --install-mode system adopt sec-core
```

从 YUM 安装 CLI 后，`sudo` 可以从系统路径找到 `anolisa`。`adopt` 会把直接
安装的 RPM 写入 system 状态，adapter 命令随后才能读取组件契约。

从源码构建时，使用仓库级统一入口。

```bash
./scripts/build-all.sh --component sec-core
```

安装文件前，源码构建入口会检查 Node.js 20 或更高版本、bubblewrap、GnuPG 和
`jq`。user mode 会一次性列出缺少的系统 runtime package 和安装命令，然后退出；
安装这些依赖后重新执行同一命令即可。已提前准备好依赖的主机可以用
`--ignore-deps` 跳过检查。

源码构建会把运行时和集成资源安装到用户目录，但不会在 ANOLISA 状态中注册
组件。请使用已安装的集成脚本，不要继续执行 `anolisa adapter enable`。具体入口见
[源码集成入口](../../docs/user-guide/zh/agent-security/agent-sec-core/QUICKSTART.md#源码集成入口)。

通过 ANOLISA 管理的 raw 包或已执行 `adopt` 的 RPM 会放置框架 adapter。
请用拥有目标框架配置的用户启用 adapter。

```bash
anolisa adapter scan
anolisa adapter enable sec-core openclaw
```

把 `openclaw` 替换为 `hermes`、`qwencode`、`cosh`、`codex` 或 `qoder`，即可启用
其他已打包的集成。

### 上手命令

```bash
# Security Baseline 扫描
agent-sec-cli harden --scan --config agentos_baseline

# 代码扫描
agent-sec-cli scan-code --code 'rm -rf /' --language bash

# 提示词注入检测
agent-sec-cli scan-prompt --mode standard --text "ignore previous instructions"

# PII 检测
agent-sec-cli scan-pii --text "contact alice@example.com" --source manual

# 验证默认安装根目录中的发布签名 Skill
agent-sec-cli verify

# Skill 完整性检查
agent-sec-cli skill-ledger check /path/to/skill

# 最近 24 小时安全态势摘要
agent-sec-cli events --summary
```

完整 CLI 说明与各宿主集成步骤见
[AgentSecCore 用户指南](../../docs/user-guide/zh/agent-security/agent-sec-core/QUICKSTART.md)。

### 资产验证

`agent-sec-cli verify` 用于检查 GPG 签名的分发 manifest 和 SHA-256 文件覆盖。它与
Skill Ledger 职责不同：`verify` 验证发布或部署签名，`skill-ledger` 维护本地 Ed25519
运行时完整性历史。

批量验证会从两个可选系统根目录发现直接、非隐藏的 Skill 子目录：RPM 安装使用
`/usr/share/anolisa/skills`，标准 ANOLISA raw 安装使用
`/usr/local/share/anolisa/skills`。缺失或为空的根目录会被跳过，指向同一 canonical path
的重复根目录只扫描一次。至少找到一个候选且全部通过时 outcome 为 `verified`；任何候选
失败时为 `failed`；没有发现候选时为 `no_candidates`。`no_candidates` 会成功退出，
但不代表已验证任何资产。

这两个根目录是固定的包默认值，不会从任意安装 prefix 动态推导。对于重定位或自定义
Skill，请使用 `agent-sec-cli verify --skill /path/to/skill`。配置、受信密钥和根目录枚举
异常仍属于操作失败；候选 Skill 的签名、manifest、哈希、未签名文件或访问失败会产生
`failed` outcome。

详见 [资产验证用户指南](../../docs/user-guide/zh/agent-security/agent-sec-core/asset-verification.md)。

## Prompt Scanner

检测提示词注入与越狱尝试。`--mode` 选择检测强度：

| 模式 | 层级 |
|------|------|
| `fast` | 仅 L1 规则引擎 |
| `standard` | L1 + L2 ML 分类器（默认） |
| `strict` | L1 + L2（L3 预留） |
| `multi_turn` | L4 多轮意图检测；从 stdin 读取 JSON payload |

```bash
agent-sec-cli scan-prompt --text "ignore all system instructions"
agent-sec-cli scan-prompt --mode fast --text "user input"
agent-sec-cli scan-prompt --input prompts.txt --format json

# 安装后拉取一次默认 L2 模型
ollama pull modelscope.cn/ANOLISA/Qwen3Guard-Gen-0.6B-GGUF

# 验证 Ollama 能提供所需模型
agent-sec-cli scan-prompt warmup
```

L2 分类器默认使用 ModelScope 上的
`modelscope.cn/ANOLISA/Qwen3Guard-Gen-0.6B-GGUF`；
`modelscope.cn/ANOLISA/Warden-Gen-0.6B-GGUF` 为可选后端，用 `--model` 或
`PROMPT_SCANNER_L2_MODEL` 选择（`--model` 优先）。同一时刻只跑一个后端，且每个
后端都需各自执行 `ollama pull`。`warmup` 只检查 Ollama 能否提供当前选定的模型，
不会自动下载模型。

详见 [Prompt Scanner 用户使用指南](../../docs/user-guide/zh/agent-security/agent-sec-core/prompt-scanner.md)。

## Code Scanner

扫描 bash 与 python 源码中的危险操作。V2 RPM 中，`scan-code` 是 Rust daemon client：
它要求 `agent-sec-daemon` 已经运行，并在未显式传入 `--socket` 时使用
`AGENT_SEC_DAEMON_SOCKET`。CLI 不会启动 daemon，也不会回退到 Python。

```bash
# 部署环境提供 daemon endpoint。
export AGENT_SEC_DAEMON_SOCKET=/run/agent-sec-core/daemon.sock

# regex 引擎（V2 当前唯一可用的扫描引擎）
agent-sec-cli scan-code --code 'rm -rf /'
agent-sec-cli --socket /run/agent-sec-core/daemon.sock \
  scan-code --code 'import os; os.system("rm -rf /")' --language python
```

verdict 枚举为 `pass` / `warn` / `deny` / `error`；内置规则当前只产出 `warn` 或
`pass`。规则嵌入 V2 binary，不再从 Python 源码目录读取。`--mode llm` 为保持 CLI
兼容而保留，但会返回 `LLM model not available`；V2 尚未支持 `--trace-context` 和
code-scan telemetry，因此依赖它们的 hook 仍处于延期状态。

完整的 daemon endpoint、CLI 和宿主 hook 状态见
[Code Scanner 用户指南](../../docs/user-guide/zh/agent-security/agent-sec-core/code-scanner.md)。

## PII Checker

检测个人数据与凭证，可输出脱敏文本。

随包提供的 [pii-checker Skill](skills/pii-checker/SKILL.md) 支持 Agent 通过 V1 CLI
检查指定文本或文件，并生成脱敏文本。

```bash
agent-sec-cli scan-pii --text "contact alice@example.com" --source manual
echo "my key is AKID1234567890" | agent-sec-cli scan-pii --stdin --format json
agent-sec-cli scan-pii --text "card 4111111111111111" --redact-output
agent-sec-cli scan-pii --input ./sample.log --include-low-confidence
```

可在 `~/.config/agent-sec/pii-checker/rules.yaml` 中添加自定义业务类型。

详见 [PII Checker 用户使用指南](../../docs/user-guide/zh/agent-security/agent-sec-core/pii-checker.md)。

## Skill Ledger

基于 Ed25519 的 Skill 目录完整性账本。在 `.skill-meta/` 中记录文件哈希、版本链和扫描结果，通过 `agent-sec-cli skill-ledger` 子命令统一管理。
对于已有 manifest，Skill Ledger 会先验真、再检查文件漂移；已有但未签名的 manifest 会报告为 `tampered`。

六种完整性状态为 `pass` / `none` / `drifted` / `warn` / `deny` / `tampered`。

`scan` 和默认的 `init` baseline 都会写入签名账本；如需只读内容 findings，
请使用 `analyze`。批量模式下，如果 `/usr/share/anolisa/skills/` 或
`/usr/local/share/anolisa/skills/` 下由 host 提供的已打包 Skill 无法写入账本状态，
命令会返回 `status=skipped`、`reasonCode=readonly_system_skill`、
`persisted=false`。`$XDG_DATA_HOME/anolisa/skills/`（默认
`~/.local/share/anolisa/skills/`）直接子目录中由 host 提供的只读 Skill，
在未被 `managedSkillDirs` 覆盖时也会跳过，原因码为 `readonly_default_skill`；
已纳管的用户 Skill 仍保留写入错误。这个运行状态不是 `pass` 结果，也不构成认证；显式执行
`scan <dir>` 仍会报错。如果跳过项此前没有任何账本 artifact，`check` 和 `status`
仍会分别报告 `none` / `unscanned`；二者都不表示 `pass`。

### 核心命令

| 命令 | 说明 |
|------|------|
| `init` | 初始化密钥，并为已覆盖 Skill 执行快速扫描 |
| `analyze <dir> --format json` | 只读分析当前内容，不创建或更新账本状态 |
| `scan <dir>` | 执行内置快速扫描并签名写入 manifest |
| `check <dir>` | 检测 Skill 文件是否漂移或被篡改 |
| `show <dir>` | 展示 latest/active 暴露摘要、用户决策、告警信息和 findings |
| `export <dir> --version latest --output <path>` | 导出签名 snapshot、manifest 和 findings 供审查 |
| `decide <dir> --action allow\|always_allow\|block\|rollback` | 记录用户决策并刷新 activation |
| `certify <dir> --findings <file>` | 导入外部扫描结果并签名写入 manifest |
| `list-scanners` | 列出已注册的内置扫描器 |
| `status` | 系统级健康概览（密钥、配置、聚合完整性） |
| `audit <dir>` | 查看版本历史与签名链 |
| `check --all` / `scan --all` | 对所有已注册 Skill 目录批量执行 |

`init --no-baseline` 只初始化密钥，不扫描 Skill。`rotate-keys` 在 help 中可见，
执行时明确报尚未实现（退出码 1，不修改密钥）。

### 快速示例

```bash
# 初始化密钥并为已覆盖 Skill 建立 baseline
agent-sec-cli skill-ledger init

# 检查完整性，不修改 ledger 元数据
agent-sec-cli skill-ledger check /path/to/skill

# 分析当前内容，不创建密钥、manifest、签名或事件
agent-sec-cli skill-ledger analyze /path/to/skill --format json

# 查看运行态暴露与用户决策状态
agent-sec-cli skill-ledger show /path/to/skill

# 导出隐藏的 latest 版本供审查，然后做出决策
agent-sec-cli skill-ledger export /path/to/skill --version latest --output /tmp/skill-review
agent-sec-cli skill-ledger decide /path/to/skill --action allow --reason "reviewed manually"

# 快速扫描并签名
agent-sec-cli skill-ledger scan /path/to/skill

# 系统健康概览
agent-sec-cli skill-ledger status
```

### SkillFS 对等认证

Skill Ledger 可使用 HMAC-SHA256 认证 SkillFS 集成的两个通信方向。agent-sec-core
侧使用以下环境变量：

| 环境变量 | 用途 |
|----------|------|
| `AGENT_SEC_SKILLFS_CONTROL_SOCKET` | 覆盖 Ledger resolver 查询的 SkillFS control socket |
| `AGENT_SEC_SKILLFS_CONTROL_AUTH_KEY_FILE` | 认证 control socket 上的 resolver 请求与响应 |
| `AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE` | 认证 daemon 接收的 SkillFS 变更通知 |

未配置 control 认证 key 时，control socket 不存在（`ENOENT`）仍保留 legacy host path
回退。配置 control key 后，socket 缺失、连接失败或认证失败都会 fail-closed，不会回退到
host path 或明文协议。配置 notify key 后，`skill_ledger.skillfs_notify_change` 同样必须使用
HMAC；daemon 的其他 method 继续兼容现有明文协议。

认证 key 路径必须是绝对路径，并指向 effective user 所有的普通非 symlink 文件，且不得
设置任何 group/other 权限位。key 文件原始长度必须为 32–4096 bytes。完整的双 key 部署和
容器卷要求见 Skill Ledger 用户手册。

内置 Qoder CLI plugin 为 `Skill` tool 注册 `PreToolUse` hook。hook 先从
`~/.qoder/skills/` 解析用户级 Skill，再从 `<cwd>/.qoder/skills/` 解析项目级
Skill，随后执行只读的 `skill-ledger check`，并按
`SKILL_LEDGER_MODE=observe|warn|ask|block`（默认 `ask`）处理结果。设置
`SKILL_LEDGER_HOOK_ENABLED=false` 可跳过 hook；旧值 `debug` 是 `observe` 的别名，
`deny` 是 `block` 的别名。每次
检查都会把 Qoder trace 标识写入安全审计日志。

设计文档：[`docs/design/SKILL_LEDGER_zh.md`](docs/design/SKILL_LEDGER_zh.md) · 用户指南：[Skill Ledger 用户手册](../../docs/user-guide/zh/agent-security/agent-sec-core/skill-ledger.md)

## Agent Capability 视图

`agent-sec-cli capabilities` 用于查看当前 CLI 进程可见环境变量推导出的 Qoder、Qwen Code、Codex、Cosh、OpenClaw 和 Hermes agent-sec hook capability 视图。

该命令不读取 OpenClaw、Hermes 或其他 Agent 的配置文件，也不解析 Agent home 目录。若希望结果尽量接近目标 Agent，请在启动目标 Agent 的同一 shell/container/service 环境中运行；即便如此，输出也只代表当前 CLI 环境变量视图，不能证明 hook 已在目标 Agent 进程中加载、注册或实际生效。Agent 配置中的 enabled、policy、timeout 等值仍可能让真实运行行为与该视图不同。

```bash
# 展示所有 agent 的所有 hook capability
agent-sec-cli capabilities

# 按 agent 过滤
agent-sec-cli capabilities --agent openclaw

# 按 capability 过滤
agent-sec-cli capabilities --capability code-scan

# 同时按 agent 和 capability 过滤，并输出 JSON
agent-sec-cli capabilities --agent hermes --capability pii-check --output json
```

支持的 capability 名称固定为：`code-scan`、`prompt-scan`、`pii-check`、`skill-ledger` 和 `observability`。CLI 过滤参数不接受 `scan-code` 或 `pii-scan-user-input` 等插件内部 ID。

对于 `observability`，该视图会对六种集成都应用 `OBSERVABILITY_TIMEOUT` 语义：默认值为 5 秒，非法值或非正数回退到 5，大于 5 的值封顶为 5。Hermes 插件配置仍可在未设置环境变量时指定更低的运行时 timeout；该配置不在此纯环境变量视图的解析范围内。

表格输出仅包含稳定的用户可见列：`CAPABILITY`、`ENABLED`、`MODE`、`SCAN_MODE`、`TIMEOUT(s)` 和 `DIAGNOSTICS`。JSON 输出保留同样的用户字段，并额外包含经过脱敏投影的 `env` 条目，其中只含 `effective` 和 `default`。两种格式都不会暴露 hook matcher 列表、source 标签、Agent config 内容、config 路径或原始环境变量值。诊断信息只说明哪个设置无效及 fallback 行为，不回显原始值。

对于 `prompt-scan`，`env` 条目还会上报 `PROMPT_SCANNER_L2_MODEL`：没有 hook 自己读取它，但每个 hook 都会调用 `scan-prompt` 子进程并由其解析 L2 后端，因此六种集成都会继承该变量。模型名只有原样展示才有意义，所以它保留大小写上报（并做转义与长度封顶）；上报的 `default` 与“不支持的后端”检查都取自 native 扫描引擎，而不是在此再存一份后端列表。引擎不支持的模型名会原样上报并附一条 diagnostic，因为引擎会在构造期报错、扫描直接失败。它没有对应的表格列，请用 `--capability prompt-scan --output json` 读取。

## Security Baseline

`agent-sec-cli harden` 封装 `loongshield seharden`；当未指定动作或 profile 时，
默认补齐 `--scan --config agentos_baseline`。

```bash
# 合规扫描
agent-sec-cli harden --scan --config agentos_baseline

# 预演修复动作
agent-sec-cli harden --reinforce --dry-run --config agentos_baseline

# 执行修复（需要 root）
sudo agent-sec-cli harden --reinforce --config agentos_baseline

# 查看完整的下游 loongshield 帮助
agent-sec-cli harden --downstream-help
```

## Observability

```bash
# 交互式下钻 TUI（需要交互式终端）
agent-sec-cli observability review

# 单会话复盘报告
agent-sec-cli observability report --last
agent-sec-cli observability report --session-id <id> --format json

# 打印公开的 observability record JSON Schema
agent-sec-cli observability schema
```

详见 [Observability 用户指南](../../docs/user-guide/zh/agent-security/agent-sec-core/QUICKSTART.md#observability可观测)。

## Security Events

安全事件会同时写入 JSONL 与 SQLite 存储。使用 `agent-sec-cli events` 查询该存储：

```bash
agent-sec-cli events --last-hours 24
agent-sec-cli events --category prompt_scan --output json
agent-sec-cli events --count-by category --last-hours 24
agent-sec-cli events --summary
```

详见 [Security Events 用户指南](../../docs/user-guide/zh/agent-security/agent-sec-core/QUICKSTART.md#security-events安全事件)。

## Agent Hook 环境变量

宿主 hook 矩阵由用户指南维护，以保持环境变量和宿主 mode 语义只有一个权威来源：
[Agent Hook 环境变量](../../docs/user-guide/zh/agent-security/agent-sec-core/QUICKSTART.md#agent-hook-环境变量)。

## 开发

```bash
# 构建全部组件（沙箱、CLI wheel、所有 adapter、skills、组件清单）
make build-all

# 单独构建
make build-sandbox
make build-cli

# 测试
make test               # Python + Rust 沙箱 + OpenClaw 插件
make test-python
make test-rust
make test-openclaw-plugin

# Lint 与格式化
make python-lint
make python-code-pretty

# 查看全部 target
make help
```

## 许可证

Apache License 2.0 — 详见 [LICENSE](../../LICENSE)。

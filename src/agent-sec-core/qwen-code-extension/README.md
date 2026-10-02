# Qwen Code extension

全本地、零 Token 成本地为 Qwen Code 提供 prompt 与 PII/凭据扫描、生命周期
Observability、已纳管 Skill 的 Skill Ledger 校验，以及 shell 命令执行前的本地
code scanner。扩展通过 Qwen Code 原生 command hook 挂载，不自行实现 HookRegistry、
事件聚合器或启动预检；policy hook 同步返回安全决策，Observability hook 异步记录生命周期事件。

协议依据是 Qwen Code 官方的
[Hooks 文档](https://qwenlm.github.io/qwen-code-docs/en/users/features/hooks/)；
extension manifest、变量替换和安装行为同时依据
[Extensions 文档](https://qwenlm.github.io/qwen-code-docs/en/developers/extensions/extension/)
及当前 Qwen Code 源码校验。

## 前置条件

- `qwen`、`python3` 和 `agent-sec-cli` 均在 `PATH` 中。
- 当前目录已被 Qwen Code 信任；Qwen Code 会拒绝从不受信任的工作区安装扩展。

当前实现与真实安装测试以 Qwen Code `0.19.9` 源码为基线；该版本声明需要
Node.js `>=22`。与仓库内其他插件部署脚本一致，本脚本仅通过 `command -v` 检查
`qwen`、`python3` 和 `agent-sec-cli` 是否存在，不绑定或推断其版本和接口实现。

存在性检查不能证明二进制来源。部署前应通过系统包管理、制品签名或校验和确认
`qwen`、`python3`、`node` 和 `agent-sec-cli` 来自受信源；运行中的 hook 也会从
Qwen Code 进程的 `PATH` 查找 `python3` 和 `agent-sec-cli`，因此该运行时 PATH
必须只包含受信目录。

## 部署

```bash
./qwen-code-extension/scripts/deploy.sh
```

脚本调用 Qwen Code 的 `extensions install`、`extensions update` 和
`extensions enable` 命令，安装到 user scope。可通过 `QWEN_BIN` 和 `QWEN_HOME`
覆盖 Qwen Code 可执行文件及配置目录。也可以把扩展目录作为第一个参数传入：

```bash
./qwen-code-extension/scripts/deploy.sh /path/to/qwen-code-extension
```

`qwen-extension.json` 的版本由正式 release 流程维护；部署脚本只在已安装版本与
manifest 版本不同时执行 `extensions update`。同版本本地开发请卸载后重新安装，
或直接运行下文 Hook 测试，不要单独占用正式版本号。

## Skill Ledger 保护

Skill Ledger hook 是同步的 `PreToolUse` hook，只处理模型调用 Qwen Code `skill` Tool
的场景。默认 policy 为 `ask`，在 exposure message 非空时请求用户确认。显式设置
环境变量后，可切换为静默审计、非阻断诊断、用户确认或阻断：

```bash
SKILL_LEDGER_MODE=observe qwen
SKILL_LEDGER_MODE=warn qwen
SKILL_LEDGER_MODE=ask qwen
SKILL_LEDGER_MODE=block qwen
```

支持的来源按 Qwen Code 优先级为项目 `.qwen/skills`、个人
`$QWEN_HOME/skills`（未设置 `QWEN_HOME` 时为 `~/.qwen/skills`）。Skill 必须先通过
`scan` 或 `certify` 加入 `managedSkillDirs`，之后才进入保护范围：

```bash
agent-sec-cli skill-ledger scan .qwen/skills/<skill>
agent-sec-cli skill-ledger scan "${QWEN_HOME:-$HOME/.qwen}/skills/<skill>"
agent-sec-cli skill-ledger show .qwen/skills/<skill>
agent-sec-cli skill-ledger show "${QWEN_HOME:-$HOME/.qwen}/skills/<skill>"
```

`scan` / `certify` 会 best-effort 记住技能目录。`show` 仅在 Skill 未纳管时返回
`managed=false`；不含该标记的正常 exposure summary 表示已纳管。hook 复用 `show`
的 exposure `message`：`message` 为空时不覆盖 Qwen Code 原有权限；非空时，
`observe` 记录后放行，`warn` 返回非阻断 `systemMessage` 后放行，`ask` 请求用户确认，
`block` 拒绝本次 Tool 调用。旧值 `debug` 仍作为 `observe` 的别名。`pass` / `warn`
通常不产生 message；`none` / `drifted` / `deny` / `tampered` 在没有既有用户放行决策
时会产生 message。

已验证的 Qwen Code 0.19.9 不在 TTY 渲染 hook 返回的非阻断 `systemMessage`。hook
仍会返回该消息，Qwen 会将其记录到 session debug 日志，并继续执行。这会影响 `warn`
以及 fallback 为 `warn` 的不支持边界；原生 `permissionDecision=ask/deny` 和可执行的
`block` 决策不受影响。

保护边界有意与 Cosh 保持一致：

- 未纳管 Skill 即使在 `block` 下也 fail-open。
- 只有 Qwen Code 可向模型暴露的磁盘 Skill 才进入 Ledger 校验。被
  `disable-model-invocation` 或 `skills.disabled` 隐藏的磁盘 Skill 会 fail-open，
  不会把自身 Ledger 状态错误套用到同名 command 或 MCP prompt。
- 直接 `/skill-name` 和同一输入中的多个 slash Skill 不产生 `skill` Tool HookInput，
  不受保护。
- extension、`.agents/skills` 和 Qwen bundled Skill 不受保护。
- 指向对应 `.qwen/skills` 根目录之外的符号链接不受保护。
- CLI 缺失、密钥初始化失败、超时、路径或 Qwen settings 不可访问、同层同名或 JSON
  异常均记录诊断并 fail-open。
- 不提供启动预检、后台扫描、缓存或配置自动修复。

缺少 Skill Ledger 密钥时，hook 会 best-effort 执行
`agent-sec-cli skill-ledger init --no-baseline`。`ask` 在 Qwen Code headless 或后台
subagent 等无法交互的场景会按 Qwen Code 规则退化为拒绝。

## Code Scanner 配置

| 环境变量 | 默认值 | 说明 |
| --- | --- | --- |
| `CODE_SCANNER_HOOK_ENABLED` | `true` | `false` 时在读取 hook input 和调用 CLI 前短路；非法值等价于未设置 |
| `CODE_SCANNER_MODE` | `observe` | 支持 `observe` / `ask` / `block`；`debug` 等价于 `observe`，`deny` 等价于 `block`；`warn` 及非法值等价于未设置，并向 stderr 写入 bounded diagnostic |
| `CODE_SCANNER_TIMEOUT` | `10` | 保留现有 CLI 超时环境变量 |

`ask` 和 `block` 复用现有 `permissionDecision=ask/deny` 交互，不新增 HookOutput 类型。scanner `warn` 与 `deny` 都进入所选交互。

## PII 扫描与阻断

| Qwen Code event | 扫描内容 | `scan-pii --source` | 阻断边界 |
| --- | --- | --- | --- |
| `UserPromptSubmit` | `prompt` | `user_input` | 可阻止 prompt 提交 |
| `PreToolUse` | `tool_input` | `tool_input` | 可阻止工具执行 |
| `PostToolUse` | `tool_response` | `tool_output` | `continue:false` 阻止正常结果的后续处理；不能撤销工具副作用 |
| `PostToolUseFailure` | `error` | `tool_output` | 仅扫描和审计；v0.19.9 不消费该事件的阻断字段 |
| `Stop` | `last_assistant_message` | `model_output` | 首次命中可要求模型重写 |
| `StopFailure` | `last_assistant_message` | `model_output` | 仅审计；Qwen Code 忽略该事件的输出 |

敏感原文只通过 stdin 传给
`agent-sec-cli scan-pii --stdin --format json --redact-output`，不会作为命令行参数。
非阻断告警和阻断理由只使用 `evidence_redacted`。CLI 缺失、超时、非零退出、非法
JSON、`error` 或未知 verdict 均 fail-open。

| 环境变量 | 默认值 | 说明 |
| --- | --- | --- |
| `PII_CHECKER_HOOK_ENABLED` | `true` | 仅识别 `true` / `false`；`false` 时在读取输入和调用 CLI 前短路 |
| `PII_CHECKER_MODE` | `observe` | `observe` 静默审计；支持 `warn` / `ask` / `block`；兼容 `debug` / `deny` 别名 |
| `PII_CHECKER_ENABLED` | `true` | 旧开关；未设置新开关时继续兼容 `false`、`0`、`no`、`off` |
| `PII_CHECKER_INCLUDE_LOW_CONFIDENCE` | `false` | 开启后传递 `--include-low-confidence` |
| `PII_CHECKER_TIMEOUT` | `5` | 子进程超时秒数，最大 8 秒；非法或非正值回退到 5 秒 |

例如，显式开启阻断后再部署或启动 Qwen Code：

```bash
export PII_CHECKER_MODE=block
export PII_CHECKER_TIMEOUT=5
./qwen-code-extension/scripts/deploy.sh
```

只有 scanner `deny` 会在 block 模式下阻断；scanner `warn` 按 hook policy 静默或告警。`PreToolUse` 的
observe/pass 结果不会返回 `permissionDecision: allow`，因此不会绕过 Qwen Code 原有的
工具权限确认。

`PostToolUse` 在工具成功执行后触发，无法撤销已经发生的副作用。Qwen Code `0.19.9`
只通过 `shouldStopExecution()` 检查 `continue:false`；单独返回 `decision:block` 不会
停止执行。命中后正常工具结果会被 Qwen 转为 hook-stopped error，不再按成功结果继续
处理。`PostToolUseFailure` 的消费路径只读取 additional context 和 artifacts，不读取
阻断字段，因此失败信息只能扫描并产生审计事件，仍进入既有错误处理链。

`Stop` 首次在 block 模式命中 `deny` 时要求模型移除敏感信息、使用占位符并重写；如果
重写后 `stop_hook_active=true` 仍命中，只输出脱敏告警而不再次阻断，避免 Stop 循环。
Qwen Code 当前没有 pre-render/output-transform hook，因此该处理是尽力阻断，不能保证
原始模型文本从未出现在终端或 transcript 中。

每次 source-specific 扫描由 `scan-pii` 产生一个脱敏 `pii_scan` SecurityEvent，并关联
可用的 session/tool call ID。Observability 对敏感指标的脱敏会另行产生
`source=observability` 的扫描事件，两类事件职责不同但使用相同关联上下文。

## 可观测事件映射

| Qwen Code event | agent-sec-core hook |
| --- | --- |
| `UserPromptSubmit` | `before_agent_run` |
| `PreToolUse` | `before_tool_call` |
| `PostToolUse` | `after_tool_call`（success） |
| `PostToolUseFailure` | `after_tool_call`（error/interrupted） |
| `Stop` | `after_agent_run`（success） |
| `StopFailure` | `after_agent_run`（failure） |

Qwen Code 当前没有与 `before_llm_call` / `after_llm_call` 对应的模型调用 hook，
所以本扩展不会伪造这两类记录。当前直接 HookInput 也没有贯穿 prompt、tool 和 stop
事件的稳定 run 标识，因此现阶段所有记录的 `runId` 统一使用全零 GUID；
`tool_call_id` 优先作为 `toolCallId`，缺失时回退到 `tool_use_id`。

Qwen Code 的工具结果回填会再次触发 `UserPromptSubmit`，其中仅包含 function response
的回填会被序列化为空 `prompt`。扩展直接检查 HookInput：缺失或空白的 `prompt` 不记录，
非空 `prompt` 记录为 `before_agent_run`。该判断不创建本地 active-run 状态，因此不会因
并发或缺失 `Stop` / `StopFailure` 而影响后续用户 prompt。

`SessionStart`、`SessionEnd`、compact、notification、subagent 和 todo 等官方事件目前
没有与 agent-sec-core Observability schema 含义一致的记录类型，因此不会被错误映射到
agent/tool run；后续应先扩展 schema，再单独挂载。

`agent-sec-prompt-scanner` 是同步安全 hook：默认 `PROMPT_SCANNER_MODE=observe`，只记录
扫描事件且不阻断；设置为 `deny` 后，`agent-sec-cli scan-prompt` 返回 `warn` 或 `deny`
时会向 Qwen Code 返回拒绝决策并阻断该 prompt。`PROMPT_SCANNER_TIMEOUT` 控制内部
`agent-sec-cli` 调用超时，默认 10 秒；外层 manifest 为 prompt scanner 预留 15 秒
command-hook 超时。`PROMPT_SCANNER_HOOK_ENABLED` 设为 `false` 时完全跳过 prompt scanner
（默认 `true`）。`PROMPT_SCANNER_SCAN_MODE` 控制扫描强度，`fast` / `standard` / `strict`
（默认 `standard`）。

`agent-sec-pii-checker` 也是同步安全 hook，但默认 `observe`；只有显式配置 block 且
scanner 返回 `deny`，才会按上述点位阻断。

`agent-sec-skill-ledger` 是同步安全 hook；只有已纳管 Skill 返回非空 exposure message，
且 policy 为 `ask` / `block` 时才改变 Qwen Code 的原有决策。

`agent-sec-observability` 仍异步执行并保持 fail-open：任何脚本、PII 扫描或记录写入异常
都不会改变 Qwen Code 的执行决策。敏感指标在写入前由本地 `scan-pii` 脱敏；脱敏失败时
直接丢弃对应敏感字段。Observability hook 默认开启；启动 Qwen Code 前设置
`OBSERVABILITY_HOOK_ENABLED=false` 可将其关闭，未设置或值无效时仍保持开启。修改后需
重启 Qwen Code。`OBSERVABILITY_TIMEOUT` 控制 agent-sec-cli 超时秒数，默认且最大为 5 秒；
空值、非法值、非正数或大于 5 的值使用 5 秒。

Code scanner hook 与 observability hook 独立挂载，作为同步
`PreToolUse` hook 仅处理 `run_shell_command`；默认 `CODE_SCANNER_MODE=observe` 不改变
工具执行，设置为 `ask` 或 `block` 时会按 Qwen Code 官方 `permissionDecision` 协议请求确认或拒绝本次命令。`deny` 继续作为 `block` 的兼容别名。敏感指标在写入前由本地 `scan-pii` 脱敏；脱敏失败时直接丢弃
对应敏感字段。

## 测试

```bash
QWEN_CODE_EXTENSION_E2E_DIR="$PWD/qwen-code-extension" \
  uv run --project agent-sec-cli pytest \
  tests/unit-test/qwen-code-extension \
  tests/e2e/qwen-code-extension -v
```

E2E 测试从 `qwen-extension.json` 读取并直接执行 command hook，使用独立进程和隔离的
`AGENT_SEC_DATA_DIR` 验证 PII source、脱敏输出、Qwen v0.19.9 输出协议、SecurityEvent、
Skill Ledger SecurityEvent、Observability JSONL、全零 `runId`、session/tool call 关联，
以及空 prompt 工具结果回填的过滤。它不安装或启动 Qwen Code。

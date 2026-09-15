# AgentSec Security Actions 权威参考

> PII 第一阶段 V2 已实现扩展见本文末尾专节；其版本化差异不修改 V1 oracle 基线。

| 属性 | 值 |
| --- | --- |
| 状态 | V1 Python capability 基线、compatibility fixtures 及 V2 CapabilityExecutor 目标 |
| 实现核对日期 | 2026-08-19 |
| 实现核对提交 | `fe58ed4b23b8` |
| action 数量 | 8 |

## 1. 文档地位

本文固化 security middleware 当前 8 个 action 的输入、输出、错误、审计和副作用，并为
V2 CapabilityExecutor 提供 oracle。未标记内容属于 **[CURRENT]**；只有进入 compatibility
inventory 的行为才是 **[PRESERVE V1]**。**[TARGET V2]** 必须与仓库内迁移总计划一致；
**[HISTORICAL]** 只作依据。Python backend 是 V1 oracle，不进入 V2 runtime。权威关系见
[`AGENT_SEC_RUST_MIGRATION_zh.md`](AGENT_SEC_RUST_MIGRATION_zh.md#1-文档状态与仓库内权威关系)。

公共 `ActionContext`、`ActionResult`、single-event lifecycle 和异常规则见
[`SECURITY_MIDDLEWARE_CONTRACT_zh.md`](SECURITY_MIDDLEWARE_CONTRACT_zh.md)。本文只描述
action-specific `params` 和 `data`。

### 1.1 类型规则

表格中的类型是受支持的契约输入。当前 Python 对少数错误类型可能发生隐式 coercion 或
抛出语言原生异常；这些越界输入不构成 Rust 必须复制的产品行为。V1 oracle harness 与
V2 protocol/action decoder 应通过同一 invalid-input fixture。

除明确写为 optional/default 的字段外，未知字段处理遵循各 action 小节。JSON key 顺序、
缩进和耗时不是语义；字段、值域、success、exit code、已知固定 error token、legacy
error-type 映射、审计 projection 和副作用是语义。公共错误目录的 CURRENT/TARGET 边界见
middleware contract。

### 1.2 公共执行解释

- `success=true`：backend 正常完成；不等于 verdict 为 pass。
- `success=false`：输入、依赖或 backend 执行失败。
- scanner 正常返回 `warn/deny` 时通常仍为 `success=true, exit_code=0`。
- `error` verdict 表示 scanner 无法完成，为 `success=false, exit_code=1`。
- 除下文专用 sanitizer 外，每次 action 的 request 和 result 会写入本地 SecurityEvent。
- 即使领域操作是只读，middleware lifecycle 仍会写 SecurityEvent/telemetry；因此调用
  middleware 本身不是纯函数。

## 2. 能力总览

| action | 领域读写 | 正常 verdict | 外部依赖 | 专用审计脱敏 |
| --- | --- | --- | --- | --- |
| `sandbox_prehook` | 只记录决策 | 无 | 无 | 否 |
| `harden` | scan 只读；reinforce 可修改系统 | 无 | `loongshield seharden` | 否 |
| `verify` | 读取 Skill、GPG key/signature | 无 | 系统 `gpg`/受信 key | 否 |
| `summary` | 读取 SecurityEvent store | 无 | SQLite/event reader | 否 |
| `code_scan` | 读取规则；LLM mode 调本地模型服务 | pass/warn/deny/error | rules、可选 Ollama | 否 |
| `prompt_scan` | 读取规则/调用本地模型服务 | pass/warn/deny/error | Rust scanner、可选 Ollama | 否 |
| `pii_scan` | 读取 built-in/custom rules | pass/warn/deny/error | 本地规则 | 是 |
| `skill_ledger` | 依 command 读取或修改 key/manifest/snapshot/activation | 六种 Ledger 状态 | filesystem、Ed25519、scanner | 是 |

### 2.1 **[TARGET V2][PENDING DEFINITION]** daemon method 映射

每个 action 通过显式注册的 daemon handler 暴露，而不是由 wire request 携带任意 action
名称。canonical mapping 为：

| action | daemon method |
| --- | --- |
| `sandbox_prehook` | `action.sandbox_prehook` |
| `harden` | `action.harden` |
| `verify` | `action.verify` |
| `summary` | `action.summary` |
| `code_scan` | `action.code_scan` |
| `prompt_scan` | `action.prompt_scan` |
| `pii_scan` | `action.pii_scan` |
| `skill_ledger` | `action.skill_ledger` |

该 mapping 与显式 allowlisted action RPC 方向一致，但 method 名、authorization、timeout
和 compatibility version 必须由 asc-daemon-protocol Definition Review 冻结后才成为正式
V2 contract。handler 的 `params` 直接使用下文对应 schema，不得增加第二套 daemon-only 默认值或
coercion。历史 commit `ef0d75f27c389434cf6f4361f5dbcdeaff42ab72` 的 `scan-prompt`
handler 是注册和三层 response 的实现依据；旧 method 名是否作为 alias 保留由调用方兼容
证据决定，不改变 canonical mapping。

`action.summary` 是 middleware `summary` action：使用 `hours/category/event_type` schema，
并在完成后写一条 summary SecurityEvent。它不是当前只读 dashboard query `sec.summary`；
后者使用 security-event filters/`latest_limit`、返回 dashboard schema，且不执行 middleware
lifecycle。两个 method 必须同时保留各自 contract，不能互为 alias。

daemon adapter 对非法输入的 response layer 必须遵循 daemon V1 的参数错误边界：wire
shape 错误返回 `bad_request`，本节定义的领域输入错误由 core 返回失败 `ActionResult`。

## 3. `sandbox_prehook`

### 3.1 目的

记录上游 sandbox 已作出的决策。当前 backend 不验证决策正确性，也不执行 sandbox 或
block；实际事件由 middleware lifecycle 写入。

### 3.2 Params

| 字段 | 类型 | 默认 |
| --- | --- | --- |
| `decision` | string | `""` |
| `command` | string | `""` |
| `reasons` | string | `""` |
| `network_policy` | string | `""` |
| `cwd` | string | `""` |

未知字段当前忽略。

### 3.3 Result

始终返回：

```json
{
  "success": true,
  "data": {
    "decision": "allow",
    "command": "ls -la",
    "reasons": "trusted command",
    "network_policy": "none",
    "cwd": "/work"
  },
  "stdout": "",
  "exit_code": 0,
  "error": "",
  "error_type": ""
}
```

`decision="deny"` 也不会令 ActionResult 失败；它是被记录的上游决定。

### 3.4 审计与副作用

默认 event projection 会保存上述 command/cwd 等字段。唯一领域副作用是审计记录；
backend 本身不执行命令。

## 4. `harden`（Security Baseline）

### 4.1 目的

执行 `loongshield seharden`，原样传递命令参数，并把稳定的 summary 和 per-rule 行解析成
结构化结果。用户文档中的概念名称是 Security Baseline；CLI command 保持 `harden`。

### 4.2 Params

支持两种互斥形式。

原始参数形式：

| 字段 | 类型 | 规则 |
| --- | --- | --- |
| `args` | list/tuple of string | 原样追加到 `loongshield seharden`；元素在边界归一为 string |

legacy 形式：

| 字段 | 类型 | 默认/值域 |
| --- | --- | --- |
| `mode` | string | `scan`；`scan/reinforce/dry-run` |
| `config` | string | `agentos_baseline` |

转换规则：

| mode | argv |
| --- | --- |
| `scan` | `--scan --config <config>` |
| `reinforce` | `--reinforce --config <config>` |
| `dry-run` | `--reinforce --dry-run --config <config>` |

无 params 等同于 legacy 默认。非空 `args` 不能和 legacy/未知 kwargs 混用；混用或未知
legacy 字段属于 invocation error，而不是 loongshield result。

### 4.3 Command resolution

1. 从 `PATH` 查找 `loongshield`；
2. 未找到时尝试可执行的 `/usr/sbin/loongshield`；
3. 仍未找到时不启动 subprocess。

实际 argv 恰好是 `[resolved-loongshield, "seharden", ...args]`，不经过 shell。stdout 和
stderr 合并捕获，ANSI color/style escape 被移除。

### 4.4 Result data

所有结果至少包含：

```json
{
  "argv": ["/usr/sbin/loongshield", "seharden", "--scan"],
  "raw_args": ["--scan"],
  "tool_path": "/usr/sbin/loongshield",
  "failures": [],
  "fixed_items": []
}
```

可解析时增加：

- `mode`、`config`；
- subprocess `returncode`；
- summary 计数 `passed/fixed/failed/manual/dry_run_pending/total`；
- `failures[]`、`fixed_items[]`，每项为 `rule_id/status/message`。

scan/dry-run 的非 pass 条目进入 `failures`。reinforce mode 中明确 `FIXED` 和兼容 legacy
fixed 行进入 `fixed_items`，真正失败保留在 `failures`。summary 报告存在 non-pass 但
无法解析明细时，必须增加一个 `status=UNKNOWN` 的 failure，不能静默丢失。

### 4.5 Success/error

| 条件 | success | exit_code | error_type |
| --- | --- | ---: | --- |
| subprocess return 0 | true | 0 | `""` |
| subprocess return 非 0 | false | 原 return code | `SubprocessError` |
| binary 缺失 | false | 127 | `FileNotFoundError` |
| spawn OSError | false | errno 或 1 | exception type |

非零 subprocess stdout 仍必须完整返回。binary 缺失的 error 必须给出安装/`PATH` 说明。

### 4.6 副作用

`scan` 通常只读；`reinforce` 可以修改系统配置，`dry-run` 不应实施修改。middleware 不为
该 action 提供幂等或回滚保证。daemon timeout/断连后不得自动本地重放。

## 5. `verify`

### 5.1 目的与 Params

验证 Skill 资产的 GPG 签名和完整性。

| 字段 | 类型 | 默认 |
| --- | --- | --- |
| `skill` | string/null | null，扫描 packaged `config.conf` 中全部目录 |

未知字段当前忽略。单 Skill 模式失败被收集为 failed item；加载受信 key/config 等全局
异常由 backend 转为产品失败。

### 5.2 Result

成功或发现验证失败时：

```json
{
  "passed": 2,
  "failed": 1
}
```

`data` 只公开计数；逐项名称和错误通过 `stdout` 的 `[OK]`/`[ERROR]` 文本输出。任一
failed item 导致 `success=false, exit_code=1`，但 `error_type` 为空，因为这是正常验证
结论，不是 backend exception。全部通过时 success true/exit 0。

全局 exception 返回 `success=false, exit_code=1`、空 data/stdout、
`error="Verification error: ..."` 和 exception type。

### 5.3 配置与副作用

默认读取 packaged `asset_verify/config.conf` 与 packaged trusted keys；验证过程可以调用
系统 GPG。领域操作只读，生命周期仍写审计事件。

## 6. `summary`

### 6.1 Params

| 字段 | 类型 | 默认 |
| --- | --- | --- |
| `hours` | finite number | 24 |
| `category` | string/null | null |
| `event_type` | string/null | null |

未知字段忽略。时间窗口是执行时刻向前 `hours * 3600` 秒，边界使用 UTC ISO-8601。

### 6.2 Result

```json
{
  "total_events": 8,
  "time_range": {"since": "...", "until": "..."},
  "by_category": {"sandbox": 5, "hardening": 3},
  "by_event_type": {"sandbox_prehook": 5, "harden": 3}
}
```

正常返回 `success=true, exit_code=0`，stdout 是 human-readable summary。当前 backend 不
捕获 reader exception，异常按 middleware unhandled-error path 传播。

查询发生在本次 summary SecurityEvent 写入之前，因此 `data` 不包含本次 summary event；
调用完成后 lifecycle 会新增一条 summary event。

## 7. `code_scan`

### 7.1 Params

| 字段 | 类型 | 默认/值域 |
| --- | --- | --- |
| `code` | string | `""`；空/纯空白产生 error verdict |
| `language` | string | `bash`；恰好 `bash/python` |
| `mode` | string | `regex`；`llm` 使用模型，其它当前值走 regex compatibility path |

未知字段当前忽略。新增实现应保留非 `llm` mode 的 regex 兼容行为，若未来收紧值域必须
版本化并修改 CLI validation。

### 7.2 Data schema

```json
{
  "ok": true,
  "verdict": "warn",
  "summary": "Detected 1 issue(s) in bash code: RULE-1",
  "findings": [
    {
      "rule_id": "RULE-1",
      "severity": "warn",
      "desc_zh": "...",
      "desc_en": "...",
      "evidence": ["..."]
    }
  ],
  "language": "bash",
  "engine_version": "...",
  "elapsed_ms": 1
}
```

verdict 值域严格为 `pass/warn/deny/error`；finding severity 只为 `warn/deny`。built-in
regex rules 当前可能只产生 pass/warn，但 deny 是 custom/LLM rule 的合法值，不能删除。

### 7.3 Success/error

| 情况 | success | exit_code | error_type |
| --- | --- | ---: | --- |
| pass/warn/deny | scanner `ok`（正常为 true） | 0 | `""` |
| error verdict | false | 1 | `CodeScanError` |
| unsupported language | false | 1 | `ErrUnsupportedLang`，data 为空 |

stdout 是 data 的 JSON。`AGENT_SEC_OLLAMA_MODEL` 选择 LLM model（默认 `warden`）；模型
service backend/base URL/timeout 使用共享 model-service 配置。

## 8. `prompt_scan`

### 8.1 Params

| 字段 | 类型 | 默认/规则 |
| --- | --- | --- |
| `text` | non-empty string | 必填，不能纯空白 |
| `mode` | string | `standard`；case-insensitive `fast/standard/strict/multi_turn` |
| `source` | string | `""` |
| `model` | string | `""`，由 scanner 使用默认模型 |
| `history` | JSON array | `[]`；只在 multi_turn 使用 |
| `assistant_response` | string | `""`；只在 multi_turn 使用 |

未知字段忽略。非 multi-turn 调用 Rust native `scan_prompt`；multi-turn 把 history 作为
JSON，并和 user text、assistant response 一起调用 Rust native `scan_multi_turn`。

### 8.2 Data schema 1.0

```json
{
  "schema_version": "1.0",
  "ok": false,
  "verdict": "deny",
  "risk_level": "high",
  "threat_type": "direct_injection",
  "confidence": 0.95,
  "summary": "...",
  "findings": [
    {
      "rule_id": "INJ-001",
      "title": "...",
      "message": "...",
      "evidence": "...",
      "category": "..."
    }
  ],
  "layer_results": [
    {"layer": "rule_engine", "detected": true, "score": 0.95, "latency_ms": 0.2}
  ],
  "engine_version": "...",
  "elapsed_ms": 1.2,
  "engine_init_ms": 0.2,
  "scan_ms": 1.0,
  "input_truncated": false,
  "input_bytes_scanned": 20,
  "degraded": false,
  "layers_failed": []
}
```

- verdict：`pass/warn/deny/error`。
- risk level：`low/medium/high/unknown`。
- threat type：`direct_injection/indirect_injection/jailbreak/unsafe/benign/not_scanned`；
  error payload 使用兼容值 `unknown`。
- `confidence` 只在检测到 threat 时存在。
- `elapsed_ms == engine_init_ms + scan_ms`，使用发布精度值计算。
- `elapsed_ms/engine_init_ms/scan_ms` 和
  `input_truncated/input_bytes_scanned/degraded/layers_failed` 在所有 schema 1.0 data payload
  中始终存在，包括 error payload；消费者可以直接按 `degraded` 执行 fail-safe 策略，不能以
  字段缺失表示完整扫描。
- `layers_failed` 是逐层失败清单，每项至少包含 `layer/error`；完整扫描时为空数组。top-level
  scanner failure 不伪造逐层失败项，原因保留在 `summary`。
- `ok` 表示 prompt 是否无 threat；因此正常 deny 的 data.ok 为 false，但 ActionResult
  success 仍为 true。

### 8.3 Success/error

pass/warn/deny 返回 `success=true, exit_code=0`。空输入、非法 mode 返回
`success=false, exit_code=1, error_type=ValueError`，data 为空。native module 缺失返回
schema 1.0 error payload 和 `NativeScannerUnavailable`。native exception 或 error verdict
返回 error payload、exit 1 和结构化 error type。

error payload 保留完整的 always-present 字段组；当前固定值为
`ok=false/verdict=error/risk_level=unknown/threat_type=unknown/confidence=0.0`、空
`findings/layer_results`、`elapsed_ms=engine_init_ms=scan_ms=0`、
`input_truncated=false/input_bytes_scanned=0`、`degraded=true/layers_failed=[]`。这里
`degraded=true` 表示没有完成扫描，是供调用方 fail-safe 决策的安全信号；空
`layers_failed` 只表示失败发生在 scanner 顶层而非某个已启动 layer，不能解释为完整扫描。

Prompt Scanner 当前由 Rust native engine 实现；Python backend 只是 adapter。这一 action
是其它 backend 迁移的跨语言参考。**[CURRENT]** daemon 不提供 `scan-prompt` 或
`action.prompt_scan`；**[TARGET V2][PENDING DEFINITION]** canonical method 候选是 `action.prompt_scan`，历史名称只在
兼容性证据要求时作为显式 alias。

## 9. `pii_scan`

### 9.1 Params

| 字段 | 类型 | 默认/规则 |
| --- | --- | --- |
| `text` | string/null | `""`；null 归一为空串 |
| `source` | string | `unknown` |
| `include_low_confidence` | boolean | false |
| `raw_evidence` | boolean | false |
| `redact_output` | boolean | false |
| `max_bytes` | positive integer/null | null 表示 middleware 不截断 |
| `input_truncated` | boolean | false，上游已截断标记 |
| `input_bytes_scanned` | non-negative integer/null | 上游实际扫描 byte 数 |

source 合法值：`user_input/tool_input/tool_output/model_output/observability/manual/unknown`；
其它值归一为 `unknown`。`max_bytes` 按 UTF-8 bytes 截取，不能截断在半个 UTF-8 codepoint。
注意 scanner module 的常量 1 MiB 不会自动用于 middleware；middleware 缺省是 unlimited。

### 9.2 Data schema

```json
{
  "ok": true,
  "verdict": "deny",
  "summary": {
    "total": 1,
    "by_type": {"api_key": 1},
    "by_category": {"credential": 1},
    "by_severity": {"deny": 1},
    "source": "tool_input",
    "bytes_scanned": 50,
    "truncated": false,
    "custom_rules": {"status": "absent"}
  },
  "findings": [
    {
      "type": "api_key",
      "category": "credential",
      "severity": "deny",
      "confidence": 0.99,
      "evidence_redacted": "sk-a...[REDACTED]...7890",
      "span": {"start": 8, "end": 40},
      "metadata": {"detector": "regex", "engine": "regex"}
    }
  ],
  "elapsed_ms": 1,
  "redacted_text": "optional"
}
```

- verdict：`pass/warn/deny/error`。
- finding category：`personal_data/credential/custom`。
- finding severity：`warn/deny`。
- confidence 小于 0.5 的 finding 默认排除；`include_low_confidence=true` 时保留。
- `raw_evidence=true` 时业务 result finding 可以增加原文 `raw_evidence`；这是敏感显式选项。
- `redact_output=true` 时增加 `redacted_text`。
- custom rules 固定路径为 `~/.config/agent-sec/pii-checker/rules.yaml`；文件非法时 built-in
  scanner fail-open 继续工作，并在 summary 以 sanitized invalid 状态报告。

### 9.3 Success/error

pass/warn/deny：`success=true, exit_code=0`。非法 text/max_bytes、scanner exception：返回
固定 error result，`success=false, exit_code=1`；summary 增加 `error/error_type`，
ActionResult 同时设置 error/error_type，stdout 不含 traceback。

### 9.4 强制审计最小化

业务调用方可以显式请求 raw evidence，但 SecurityEvent 永远删除原文、raw evidence 和
redacted text；只保存 text length/SHA-256、扫描选项和 sanitized finding。Rust backend
必须在 lifecycle 前使用相同 sanitizer。

## 10. `skill_ledger`

### 10.1 目的

`skill_ledger` 是一个 action，使用必填 `command` 分派多个领域操作。未知或空 command
返回 `success=false, exit_code=1, error_type=ValueError`。

完整 manifest、签名链、activation 和 scanner 语义由
[`SKILL_LEDGER_zh.md`](SKILL_LEDGER_zh.md) 定义；SkillFS/daemon 集成由
[`SKILL_LEDGER_SKILLFS_INTEGRATION_zh.md`](SKILL_LEDGER_SKILLFS_INTEGRATION_zh.md) 定义。

### 10.2 六种 Skill Ledger 状态

Skill Ledger 完整性状态恰好为以下六种，文档和实现不得删减：

| 状态 | 含义 |
| --- | --- |
| `pass` | manifest/签名/身份有效，live files 匹配，scanStatus 为 pass |
| `none` | 没有 Ledger artifact，或有效 manifest 的 scanStatus 为 none |
| `drifted` | manifest 已验真，但 live files 与已签名 fileHashes 不一致 |
| `warn` | manifest/文件匹配，扫描得到低风险告警 |
| `deny` | manifest/文件匹配，扫描得到高风险结论 |
| `tampered` | Ledger metadata、签名、身份、latest/历史绑定等真实性或完整性校验失败 |

批处理中的 `error` 是执行结果，不是第七种 Ledger 状态。

### 10.3 Commands 和 Params

| command | 参数 | 读写/说明 |
| --- | --- | --- |
| `init` | `baseline=true`, `passphrase=null`, `passphrase_requested=false`, `force_keys=false`, `scanner_names=null` | 创建/轮换 key；可为已覆盖 Skill 建 baseline |
| `init-keys` | `force=false`, `passphrase=null` | 低层兼容入口，生成/轮换 key |
| `check` | `skill_dir=null`, `all_skills=false` | 只读；单目录必填或 `all_skills=true` |
| `certify` | `skill_dir`, `findings`, `scanner=skill-vetter`, `scanner_version=null`, `delete_findings=false`, `all_skills=false`, `scanner_names=null` | 导入外部 findings，签名建版本；`all_skills` 必须 false、`scanner_names` 必须 null |
| `scan` | `skill_dir=null`, `all_skills=false`, `scanner_names=null`, `force=false` | 运行 built-in scanner，签名建版本 |
| `status` | `verbose=false` | keys/config/skills 聚合只读状态 |
| `audit` | `skill_dir`, `verify_snapshots=false` | 深度验证历史链；只读 |
| `list-scanners` | 无 | 列出全部 scanner |
| `decide` | `skill_dir`, `decision_action=null`, `target_version_id=null`, `reason=null`, `clear=false` | 写/清用户决策并刷新 activation |
| `show` | `skill_dir`, `policy=null` | exposure summary、latest/active/decision/findings |
| `export` | `skill_dir`, `version`, `output`, `policy=null` | 导出 snapshot/manifest/findings 到目标目录 |

command handler 当前忽略额外字段。CLI 层还会做一部分互斥/必填检查，但 core 不能依赖
CLI 是唯一调用方，必须保持表中 backend 校验。

`init/scan/certify/decide` 在 key 缺失时可以自动生成未加密 key，并返回 warning。
`SKILL_LEDGER_PASSPHRASE` 为非交互签名提供 passphrase；`XDG_DATA_HOME` 和
`XDG_CONFIG_HOME` 控制 Skill Ledger data/config root。

### 10.4 Common result

有结构化 data 时必须包含 `command`，其余字段由 domain operation 提供。例如：

```json
{
  "command": "check",
  "skillName": "weather",
  "status": "drifted",
  "added": ["new.py"],
  "removed": [],
  "modified": []
}
```

批量 `init/check/scan` 使用 `results` array。创建 key 的操作可以增加
`keyCreated/key/warnings`。JSON 业务字段保持现有 camelCase；只有 SecurityEvent 的
Skill Ledger result projection 递归转为 snake_case，且不得修改 ActionResult.data/stdout。

stdout 是 JSON 加末尾换行。部分 legacy 单目录 command 的 stdout 是未包含 `command`
的领域 body，而 data 包含 `command`；调用方应优先消费 data，兼容测试仍需保留 stdout。

### 10.5 Success/error 规则

- `check` 单目录：`deny/tampered` 为 success false/exit 1；其它六状态中的
  pass/none/drifted/warn 为 success true/exit 0。
- `check --all`：任一 `tampered/deny/error` 导致 success false/exit 1；只有 error item
  时设置 `SkillLedgerError`。
- `scan/init` batch：任一 item status error 导致 success false/exit 1。
- `scan/certify` 单目录：只要领域调用无 exception 就 success true；scan verdict
  warn/deny 是正常结果。
- `audit`：`valid` 决定 success 和 exit code。
- `status/decide/show/export/init-keys`：领域调用成功即 success true；exception 转为
  success false/exit 1 和 exception type。
- `list-scanners`：成功时 success true；当前 registry 加载 exception 走 middleware
  unhandled-error path，而不是返回 ActionResult failure。
- `decide` 非 clear 时缺少 action 返回 ValueError；rollback 等 action-specific 校验由
  domain core 负责。

### 10.6 Side effects 和并发

- `check/status/audit/list-scanners/show` 是领域只读。
- `init/init-keys` 写 key；force rotation 先归档旧 public key。
- `scan/certify` 写 config、signed manifest、version snapshot 和 latest。
- `certify(delete_findings=true)` 成功后删除输入 findings。
- `decide` 写 signed user decision，rollback 可以恢复文件并产生新版本；clear 也刷新
  activation。
- `export` 写调用方指定目录。

Rust 实现必须以领域事务、幂等键、跨进程锁或原子替换保护这些状态，因为 daemon request、
JobSupervisor、state migrator 或其它 repository consumer 可能并发。已发送请求发生 timeout
时 client 不能自动重复执行写 command。

`show.latestStatus="unmanaged"` 是“当前 daemon 不可管理该 root”的 exposure 诊断值，
不是第七种 Skill Ledger 完整性状态，也不进入六状态的用户决策分支。

### 10.7 Audit sanitizer

request 中非 null `passphrase` 必须写成 `[REDACTED]`。event result key 转 snake_case，
`scanStatus` 投影为 `verdict`；batch event verdict 取最严重状态。scanner finding
`metadata` 不做 key 改写。所有 projection 使用安全 copy，不能回写业务 data。

## 11. **[CURRENT]** V1 配置与资源来源

| 能力 | 主要来源 |
| --- | --- |
| SecurityEvent store | `AGENT_SEC_DATA_DIR` override；否则系统/user/安全临时目录 fallback |
| Prompt/Code LLM | `AGENT_SEC_MODEL_SERVICE_BACKEND/BASE_URL/TIMEOUT`，`AGENT_SEC_OLLAMA_MODEL` |
| PII custom rules | `~/.config/agent-sec/pii-checker/rules.yaml` |
| Skill Ledger config | `$XDG_CONFIG_HOME/agent-sec/skill-ledger/config.json` |
| Skill Ledger key/data | `$XDG_DATA_HOME/agent-sec/skill-ledger/` |
| Skill Ledger passphrase | `SKILL_LEDGER_PASSPHRASE` 或 CLI 交互后作为 params 注入 |
| Asset verify | packaged config 和 trusted GPG keys |
| Harden | `PATH`、`/usr/sbin/loongshield`、传给 loongshield 的 config name |

环境变量和路径进入 compatibility inventory；V2 不自动保留 HOME/XDG/per-user 布局。
**[TARGET V2]** 由 capability/config contract 统一解析领域配置，asc-daemon composition
root 注入依赖；agent-sec-cli 只处理终端交互和 RPC DTO，不读取领域状态或复制默认值。

## 12. V1/Rust action conformance

### 12.1 **[CURRENT][PRESERVE V1]** action 行为基线

每个 action 必须使用同一组语言无关 fixture，至少比较：

| ID | 范围 |
| --- | --- |
| SAR-001 | 默认参数、显式参数、未知/非法参数 |
| SAR-002 | pass/warn/deny/error 与 success/exit/error type 正交关系 |
| SAR-003 | data 完整 schema 与 stdout 可解析/可显示性 |
| SAR-004 | event category/result/details 和 correlation IDs |
| SAR-005 | PII/Skill Ledger 敏感字段永不进入 event/telemetry |
| SAR-006 | 文件、SQLite、key、manifest、snapshot、activation、外部进程副作用 |
| SAR-007 | backend exception、dependency missing、resource invalid 和 timeout |

### 12.2 **[TARGET V2]** Rust 执行路径验收

| ID | 范围 |
| --- | --- |
| SAR-008 | Python oracle 与 Rust CapabilityExecutor/action-runtime 比较完整 ActionResult、event 和副作用；daemon compatibility adapter 只比较 V1 projection |
| SAR-009 | 八个 action handler 候选的 method/params、authorization、未知字段、错误层级和失败 projection 经 protocol Definition Review 后与本参考一致 |
| SAR-010 | agent-sec-cli 不通过 PyO3/local backend 执行 action；daemon unavailable 返回稳定错误 |

各 action 的最低 fixture：

- sandbox：空字段、allow、deny、未知字段；
- harden：默认、scan/reinforce/dry-run、缺 binary、非零 exit、ANSI、各 summary 格式；
- verify：单 Skill pass/fail、全目录 mixed、key/config exception；
- summary：空库、filter、时间窗口、reader exception；
- code：bash/python、空输入、unsupported language、regex/LLM、四 verdict；
- prompt：四 mode、空输入、native unavailable、model exception、完整 1.0 schema；
- PII：七 source、低置信度、UTF-8 byte 截断、raw/redact、custom rules invalid、四 verdict；
- Skill Ledger：11 command、六状态、batch severity、key lifecycle、write failure、并发和
  crash recovery。

## 13. 当前实现证据

- action inventory：`agent-sec-cli/src/agent_sec_cli/security_middleware/router.py`。
- adapters：`security_middleware/backends/*.py`。
- Code Scanner schema：`agent_sec_cli/code_scanner/models.py`、`scanner.py`。
- Prompt Scanner schema：`agent-sec-cli/crates/prompt-scanner/src/result.rs` 和
  `security_middleware/backends/prompt_scan.py`。
- PII schema/redaction：`agent_sec_cli/pii_checker/models.py`、`scanner.py`、`audit.py`。
- Skill Ledger：`agent_sec_cli/skill_ledger/core/`、`signing/`、`scanner/`。
- action characterization：`tests/unit-test/security_middleware/backends/`。

## PII 第一阶段 V2 替代契约

**[TARGET V2，已实现]** `PiiScanner` 和 `PiiScanExecutor` 保留本文 PII 的 11 类检测、
校验、置信度、去重排序、Unicode 字符 span、重叠保留与合并脱敏语义。
顶层 V1 字段保留；新增信息仅放入 `summary`：`execution_status`、`coverage`、
`input_sha256`、`scanned_input_sha256`、`scanned_bytes` 和 `ruleset_id`。
`coverage` 使用 complete/partial/unavailable；partial 时 verdict 仍仅聚合已有 findings。
输入摘要对应检测器收到的文本，不冒充调用前被截断的完整操作内容。

本文 V1 用户目录/每次扫描重载规则由版本化 V2 中央配置替代：默认
`/etc/agent-sec/pii-checker/rules.yaml`，daemon `--pii-rules` 可选绝对路径，启动编译、重启生效。
默认缺失为 absent，显式读取失败或任一无效规则使自定义集合 invalid，内置检测继续、coverage partial。
V2 使用 fancy-regex，回溯上限 1,000,000、循环预算 200 ms；不承诺 20 ms 中断单次匹配。
100 条自定义发现之后首次省略命中会停止后续匹配；不支持的 Python 语法返回 invalid_regex，不改写。

差分和限制由 capability 的 `tests/compatibility.rs`、`tests/custom_rules.rs`、冻结 122 个
V1 合成用例及单测验证。完整差异、未来 Evidence 边界和回滚见
[PII 两阶段设计](PII_V2_MIGRATION_zh.md)。保留 V1 实现独立回滚。

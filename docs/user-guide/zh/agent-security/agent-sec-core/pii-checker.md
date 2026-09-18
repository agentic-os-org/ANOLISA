# PII Checker 用户使用指南

[English](../../../en/agent-security/agent-sec-core/pii-checker.md)

PII Checker 用于检测 Agent 输入和输出中的个人数据与凭据。它返回结构化 verdict，生成安全的
evidence 和可选脱敏文本，并记录经过清理的 Security Event，供审计和 Observability 关联使用。

## 使用随包 Skill

安装 V1 `agent-sec-cli` 且 Agent 能发现 `pii-checker` Skill 后，可以要求它检查指定文件中的
个人信息或凭证，或生成脱敏副本。Skill 报告脱敏证据，仅在用户要求时改写输入文件。
扫描完成且没有命中，不代表内容一定不含敏感信息。

RPM 安装通过 `agent-sec-skills` 分发此 Skill，ANOLISA raw 包将其放入共享 Skill 目录。
Cosh-NG 会发现该目录；OpenClaw 和 Hermes adapter 声明了 `pii-checker`，用于分发到各自的
Skill 目录。其他 Agent 安装需通过其自身的发现路径提供此 Skill。

## 扫描文本

必须且只能提供一种输入来源：内联文本、标准输入或 UTF-8 文件。

```bash
# 内联文本
agent-sec-cli scan-pii --text "contact alice@example.com"

# 标准输入
printf '%s' 'token=secret-value-1234567890' \
  | agent-sec-cli scan-pii --stdin --redact-output

# UTF-8 文件
agent-sec-cli scan-pii --input ./agent-output.txt --format text
```

常用选项：

| 选项 | 作用 |
|------|------|
| `--format json\|text` | 选择结构化 JSON 或可读文本；默认是 `json` |
| `--redact-output` | 返回 `redacted_text`；不会修改输入文件 |
| `--include-low-confidence` | 包含低于默认置信度阈值的 finding |
| `--raw-evidence` | 仅在本地 CLI 输出中包含原始 evidence |
| `--max-bytes N` | 最多扫描 `N` 个 UTF-8 字节，并标记结果已截断 |
| `--source SOURCE` | 标记审计上下文，例如 `user_input` 或 `tool_output` |

支持的 source 包括 `user_input`、`tool_input`、`tool_output`、`model_output`、
`observability`、`manual` 和 `unknown`。

## 内置检测

内置检测器结合正则匹配、格式校验和基于上下文的置信度调整。

| 类别 | 类型 | 默认严重级别 |
|------|------|--------------|
| 个人数据 | `email`、`phone_cn`、`credit_card`、`cn_id` | `warn` |
| 凭据 | `private_key`、`bearer_token`、`api_key`、`jwt` | `deny` |
| 阿里云凭据 | `aliyun_access_key_id`、`aliyun_access_key_secret` | `deny` |
| 敏感字段 | `generic_secret_field` | `deny` |

信用卡号、中国身份证号和 JWT candidate 在生成 finding 前会经过格式校验。周边安全关键词可能提高
置信度，`example`、`dummy`、`test`、`sample` 等测试标记可能降低置信度。低于默认 `0.5` 阈值的
finding 会被忽略，除非使用 `--include-low-confidence`。

## Verdict 与脱敏

Scanner 将 findings 聚合为一个 verdict：

| Verdict | 含义 |
|---------|------|
| `pass` | 置信度过滤后没有 finding |
| `warn` | 存在 finding，但没有 `deny` 严重级别 |
| `deny` | 至少一个 finding 的严重级别为 `deny` |

每个 finding 包含类型、类别、严重级别、置信度、span、detector metadata 和脱敏 evidence。
`--redact-output` 还会返回扫描文本的脱敏副本。重叠 finding 会全部保留，其重叠 span 会合并并只
替换一次；不同 span 发生重叠时，合并后的完整范围会被完全脱敏，避免较短命中留下敏感后缀。

`--raw-evidence` 仅用于本地排查，原始 evidence 永远不会写入 Security Event。各宿主集成消费相同的
verdict 和 finding schema；宿主只观察 finding 还是阻断操作，由该宿主配置的 PII policy 决定。

## 宿主 Hook Policy

设置 `PII_CHECKER_HOOK_ENABLED=false` 可完全跳过宿主 PII hook。启用后，
有原生提示能力的宿主支持 `observe`、`warn`、`ask`、`block`，默认 `observe`。Hermes
只支持 `observe` / `block`；旧 `warn` / `ask` 会降级为 `observe` 并写宿主诊断，绝不
把 advisory 注入助手最终回复。Hermes 的 `block` 只能在 `pre_tool_call` 对 `deny` 生效；
模型输出通过 `post_llm_call` 执行 audit-only 扫描，但由于 Hermes 没有插件可用的
pre-stream output gate，不会修改或阻断模型输出。其它宿主或 hook 事件无法执行确认/阻断时
可 fallback 为 `warn`；后置 hook 不会声称撤销已经发生的
外部副作用。环境变量 policy 优先于 Hermes/OpenClaw capability 配置。`debug` 映射为
`observe`，`deny` 映射为 `block`。仅 Qwen Code 在未设置 `PII_CHECKER_HOOK_ENABLED` 时
额外兼容旧开关 `PII_CHECKER_ENABLED`。

`PII_CHECKER_HOOK_ENABLED` 和 `PII_CHECKER_MODE` 被全部六个宿主读取。另有两个变量只被部分
宿主读取：

| 环境变量 | 默认值 | 读取该变量的宿主 |
|----------|--------|------------------|
| `PII_CHECKER_TIMEOUT` | `5` | Qoder、Codex、Qwen Code（Qwen Code 上限为 8 秒） |
| `PII_CHECKER_INCLUDE_LOW_CONFIDENCE` | `false` | Qoder、Qwen Code |

cosh、Hermes 和 OpenClaw 不读取这两个环境变量。Hermes 可通过 capability 配置同时支持
这两项（`timeout` 和 `include_low_confidence`）。OpenClaw 只支持
`piiIncludeLowConfidence`；PII scanner CLI 超时固定为 10 秒。cosh 使用固定超时，且从不
请求低置信度 finding。

宿主 Agent 在加载插件时读取这些变量。修改后需重启承载该 hook 的 Agent 进程；
hook 和 agent-sec-core 并不是需要单独重启的 policy 服务。

scanner verdict `deny` 描述扫描风险，hook policy `block` 决定 adapter 是否执行阻断。

## 自定义正则规则

PII Checker 可从一个固定的用户级文件加载业务自定义类型：

```text
~/.config/agent-sec/pii-checker/rules.yaml
```

YAML 顶层是数组。每条规则包含唯一的自定义类型、一个正则表达式和可选严重级别。

```yaml
- type: dogfood_order_no
  regex: '(?i)(?<=order_no[=:])DFT-[A-Z0-9]{8}'
  severity: warn

- type: dogfood_customer_token
  regex: 'DFT-[A-Z0-9]{16}'
  severity: deny
```

| 字段 | 必填 | 说明 |
|------|------|------|
| `type` | 是 | 小写 snake_case 自定义类型，在文件内唯一 |
| `regex` | 是 | 单个正则表达式；flag 使用 `(?i)` 等内联语法 |
| `severity` | 否 | `warn` 或 `deny`；默认 `deny` |

正则的完整匹配范围就是 finding 和脱敏范围，普通捕获组和命名捕获组不会改变该范围。如果正则同时
匹配字段名和值，二者都会被脱敏；如需完整匹配只覆盖值，请使用 lookaround。同一个类型的多种格式
需要通过正则 `|` 合并，同一个 type 不能定义多条规则。

自定义 finding 固定使用 category `custom`、confidence `1.0`、detector `custom_rule` 和 engine
`regex`。它会使用 `[DOGFOOD_ORDER_NO_REDACTED]` 这类稳定类型标记进行完全脱敏，并进入与内置
finding 相同的 verdict、policy、Security Event 和 Observability 链路。

自定义规则路径不支持 CLI 参数、环境变量、XDG 覆盖、系统级文件或多文件合并。

## 自定义规则校验与运行时限制

整份自定义规则集以原子方式接受或拒绝。

| 限制或规则 | 值 |
|------------|----|
| 文件大小上限 | 256 KiB |
| 规则数量上限 | 100 |
| 单条正则长度上限 | 2,048 个字符 |
| 正则分组嵌套深度上限 | 64 |
| Type 格式 | `^[a-z][a-z0-9_]{0,63}$` |
| Severity | `warn` 或 `deny` |
| 单条规则匹配超时 | 20 ms |
| 单次扫描自定义匹配总预算 | 200 ms |
| 单次扫描自定义 finding 上限 | 100 |

未知 YAML 字段、重复 type、内置类型名、非法正则以及能在空字符串上产生匹配的正则都会让整份
自定义规则集无效。运行时遇到的其他零长度匹配会被忽略。

`deny` 规则先于 `warn` 规则执行，同一 severity 内保持文件顺序。100 条上限只限制输出的自定义
finding，不会停止后续规则的评估；只有额外的有效命中被省略时，`truncated` 才会变为 `true`。
单规则或总时间限制仍可能停止剩余自定义规则，并通过独立状态记录。

文件内容发生变化后，下一次扫描会自动校验并编译新内容。如果新版本无效，不会继续使用上一版有效
规则；内置检测仍保持工作，`scan-pii` 也会正常完成。

## 自定义规则状态

每次默认扫描都会在 `summary.custom_rules` 中返回经过清理的自定义规则状态：

```json
{
  "custom_rules": {
    "status": "loaded",
    "rule_count": 2,
    "runtime_error_count": 0,
    "budget_exhausted": false,
    "truncated": false
  }
}
```

文件不存在时 `status` 为 `absent`；校验成功时为 `loaded`，空数组也属于加载成功；读取、YAML
解析、schema 校验或正则编译失败时为 `invalid`。无效状态包含经过清理的 `error_code`；已加载或
无效内容可能包含其 SHA-256 摘要。运行时计数器不会包含输入文本或正则内容。直接执行 `scan-pii`
时还会向 stderr 输出经过清理的无效配置告警，同时保持成功退出。

当前可能出现的 `error_code` 如下：

| 错误码 | 含义 |
|--------|------|
| `read_error` | 规则文件无法读取 |
| `file_too_large` | 规则文件超过 256 KiB |
| `invalid_utf8` | 规则文件不是有效的 UTF-8 |
| `invalid_yaml` | YAML 内容无法安全解析 |
| `top_level_not_list` | YAML 顶层不是数组 |
| `too_many_rules` | 文件包含超过 100 条规则 |
| `invalid_rule_schema` | 规则缺少字段、包含未知字段、字段类型错误或使用不支持的字段值 |
| `invalid_rule_type` | 规则 type 不符合要求的命名格式 |
| `duplicate_rule_type` | 同一个自定义 type 出现多次 |
| `reserved_rule_type` | 自定义 type 与内置 PII 类型冲突 |
| `invalid_regex` | 正则无法编译、分组嵌套超过 64 层，或加载期校验超时 |
| `regex_matches_empty_text` | 正则可在空字符串上产生零长度匹配 |
| `load_error` | 未预期的加载器错误已按 fail-open 模式处理 |

## Security Event 与 Observability

每次扫描都会进入现有 `pii_scan` Security Event 链路。Event 包含 source、verdict、summary、
finding type、severity、category、span 和脱敏 evidence，不包含自定义规则路径、正则表达式或原始
敏感命中值。

自定义规则无效时，宿主 hook 保持 fail-open，不新增宿主侧告警。hook 调用以 Security Event 中
经过清理的 `summary.custom_rules` 状态作为结构化审计来源。

Observability 使用现有 trace context 和输入 hash 与 Security Event 建立关联，不重复存储 finding
明细。

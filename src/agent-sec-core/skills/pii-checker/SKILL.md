---
name: pii-checker
description: 使用 agent-sec-cli 检查文本、UTF-8 文件或日志中的个人信息与凭证，报告脱敏证据，并按需生成脱敏文本。当用户要求检查敏感信息、查找泄露的密钥或对内容脱敏时使用；查询已发生的安全事件使用 security-observability。
---

# PII Checker

复用 V1 `agent-sec-cli scan-pii` 的本地检测规则，检查个人信息和凭证。
支持邮箱、中国手机号、身份证号、银行卡号、API Key、Bearer Token、JWT、
私钥及自定义规则。仅检查用户指定的内容，不主动扩大到整个目录或历史会话。

## 调用

已知文件路径时直接扫描文件，无需先将原文读取到对话中：

```bash
agent-sec-cli scan-pii --input /absolute/path/to/input.txt --source manual --format json
```

用户要求脱敏时追加 `--redact-output`：

```bash
agent-sec-cli scan-pii --input /absolute/path/to/input.txt --source manual --format json --redact-output
```

内容来自文本或上游工具时，通过工具的标准输入接口或已有数据流传给下面的命令；
不要启动无人提供输入的交互式读取：

```bash
agent-sec-cli scan-pii --stdin --source manual --format json
```

- `--input` 只接受单个 UTF-8 文本文件，不解析目录、PDF、图片或压缩包。
- `--input`、`--stdin`、`--text` 三者必须且只能选一个。敏感正文优先走文件或
  stdin，避免放入命令参数；不要将待扫描文本拼接为 shell 代码。
- 替换路径时优先使用参数数组；必须经 shell 时，对整个路径使用可靠的 shell
  quoting（例如 `shlex.quote`），不能只在任意路径两侧加单引号。
- 普通主动检查使用 `--source manual`。此标签用于审计，不会更改输入或 Hook 策略。
- `--include-low-confidence` 可用于用户要求的更广泛复核，包含默认被过滤的低置信度
  命中；这些分值是启发式评分，不是经过校准的概率。
- 默认扫描完整输入。只有明确限定范围时才设置正整数 `--max-bytes`，并报告截断。
- CLI 不可用、不支持 `scan-pii`、输入不可读或命令失败时，报告未完成检查；
  不以模型自行猜测代替扫描结果，不安装软件或更改安全配置来绕过失败。

## 解释结果

解析 JSON 的 `ok`、`verdict`、`summary`、`findings`；不能只看退出码。
`warn` 和 `deny` 也可能返回退出码 0，表示扫描完成。

| 字段 / 值 | 解释与处理 |
|---|---|
| `ok=false` 或 `verdict=error` | 扫描失败，本次未完成检查 |
| `verdict=pass` | 在本次规则、置信度过滤和扫描范围内未检出问题 |
| `verdict=warn` | 存在告警级命中，没有 `deny` 级命中 |
| `verdict=deny` | 存在 `deny` 级命中；这是扫描判定，不代表已经阻断或撤销凭证 |
| `summary.truncated=true` | 只扫描了部分输入，不能给出全文结论 |
| `summary.custom_rules.status=invalid` | 自定义规则未生效，内置规则的结果仍可报告 |
| `summary.custom_rules.runtime_error_count>0`、`budget_exhausted=true` 或 `truncated=true` | 自定义规则执行或结果不完整，必须说明覆盖限制 |

未配置自定义规则（`status=absent`）是正常状态，不算扫描失败。
JSON 缺失、解析失败、关键字段缺失或出现未知 verdict 时，报告无法取得有效判定。
`pass` 只表示未检出，不能承诺不存在敏感信息或内容可以安全外发。

报告实际扫描范围、判定、`summary.total` / `summary.by_type` 的命中统计，
必要时列出 finding 的 `type`、`severity`、`span` 和 `evidence_redacted`。
不要使用 `--raw-evidence`，也不要从原文或对话历史补回已脱敏的值。

## 脱敏与审计

只在用户要求脱敏时返回 `redacted_text`；它只替换检出的内容。
如果扫描或自定义规则不完整，要说明脱敏结果也不完整，不能把截断的片段当作全文。
`--redact-output` 不修改原文件；仅在用户要求保存或替换文件时执行对应写入。

扫描会记录经过脱敏的 `pii_scan` 安全事件。此 Skill 不调整 Hook 开关、阻断策略
或自定义规则配置；历史安全事件的查询与复盘由 `security-observability` 处理。

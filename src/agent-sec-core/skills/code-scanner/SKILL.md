---
name: code-scanner
description: 扫描 Bash / Python 代码片段中的安全风险，返回结构化 JSON 扫描结果。当用户要求检查代码安全性时使用。
---

# Code Scanner

基于正则的轻量级代码安全扫描引擎，支持 Bash 和 Python，内置 25 条检测规则。

## 调用方式

```bash
agent-sec-cli scan-code --code '<source_code>' --language <bash|python>
```

## 输入

| 参数 | 类型 | 必选 | 说明 |
|------|------|------|------|
| `code` | string | 是 | 待扫描的源代码字符串，支持多行 |
| `language` | string | 是 | 编程语言，取值：`bash` 或 `python` |

## 输出

返回 JSON 格式的 `ScanResult` 对象：

| 字段 | 类型 | 说明 |
|------|------|------|
| `ok` | bool | 扫描是否正常完成（无内部错误） |
| `verdict` | string | 最终判定，见下方 Verdict 说明 |
| `summary` | string | 人类可读的扫描摘要 |
| `findings` | Finding[] | 命中的规则列表，无命中时为空数组 |
| `language` | string | 输入的语言 |
| `engine_version` | string | 与 agent-sec-cli 版本一致 |
| `elapsed_ms` | int | 扫描耗时（毫秒） |

### Finding 结构

| 字段 | 类型 | 说明 |
|------|------|------|
| `rule_id` | string | 规则 ID，如 `shell-recursive-delete` |
| `severity` | string | 严重级别：`warn` / `deny` |
| `desc_zh` | string | 中文描述 |
| `desc_en` | string | 英文描述 |
| `evidence` | string[] | 匹配到的代码片段列表 |

### Verdict 说明

| 值 | 含义 |
|------|------|
| `pass` | 未检测到安全风险 |
| `warn` | 存在告警级风险，建议关注 |
| `deny` | 存在阻断级风险，应阻止执行 |
| `error` | 扫描引擎内部错误 |


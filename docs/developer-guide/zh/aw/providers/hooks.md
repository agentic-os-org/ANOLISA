# 编写自定义 Hook

[English](../../../en/aw/providers/hooks.md)

## 布尔规则

简单规则读取一个标准化工具事件，stdout 输出 JSON `true` 或 `false`，退出 0。诊断写 stderr。
保存为 `/absolute/check.py`，把路径换成自己文件的绝对路径。

```python
import json
import sys

event = json.load(sys.stdin)
print(json.dumps("AW_DENY_TEST" in json.dumps(event["tool"]["input"])))
```

将以下字段合入完整配置的 `spec`。若已有同名 Provider 或事件，合并所需步骤并保留原规则。

```yaml
providers:
  command:
    transport:
      type: stdio
      location: agent
      argv: [/usr/bin/python3, /absolute/check.py]
    timeout_ms: 1000
    max_output_bytes: 32
events:
  tool.before:
    enabled: true
    required: true
    steps:
      - id: check-command
        provider: command
        effects: [block]
        reason_code: demo_marker_denied
        on_error: block
```

`true` 请求步骤声明的效果，`false` 不请求效果。省略 protocol 使用 `aw-hook/v1alpha1`，省略 operation 使用 `invoke`。
脚本在事件发生时运行；非零退出、超时、非法或超限输出按 `on_error` 处理。布尔输出最多为 32 字节与配置上限的较小值。

按[编辑教程](../reference/configuration-editor.md)可从空配置跑通完整示例。生成新文件时，在[安装指南](../../../../user-guide/zh/aw/installation.md)的完整 `aw config init` 命令末尾追加 `--provider command --check /usr/bin/python3 --effect block -- /absolute/check.py`。保留安装目录、配置、状态目录和 Agent 参数，并替换脚本路径。

## 复用原生命令

已有框架脚本使用 `protocol: native-hook/v1alpha1`、`config: {}`，步骤填写 `native: {}` 和 `on_error`，不填写 operation/effects。
它接收原生回调 stdin，输出与退出状态由所选 Adapter 解释。Qoder 的退出码 2 可阻断工具，其他框架按各自合同处理。
`transport.argv` 是字面参数数组；需要 Shell 语法时显式指定 `/bin/sh -c`。

## 扩展为 Provider

需要返回结构化候选效果或校验私有配置时，使用 [Provider 协议](protocol.md)。
三种形式的步骤均受事件预算、工具选择器和 Adapter 能力限制。新规则按[原生验收](../validation.md)验证允许、阻断、故障与清理。

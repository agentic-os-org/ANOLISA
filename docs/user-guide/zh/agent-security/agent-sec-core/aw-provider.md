# 通过 AW Provider 使用代码安全检查

[English](../../../en/agent-security/agent-sec-core/aw-provider.md)

AW Provider 复用本地 Bash 和 Python 安全规则，不使用模型、不消耗 Token。
一份 Provider 配置可以按任意标准化工具名选择代码字段：执行前返回扫描结果或
请求阻断；执行后观察工具后事件，不重新扫描或修改工具结果。

该入口由源码构建的 Rust V2 `agent-sec-cli` 提供，Python V1 CLI 尚无此命令。
Agent 原生接线与效果采用仍由 AW 适配器负责；Provider 成功返回不代表 Agent
已采用其效果。

## 准备 Provider

在仓库根目录构建：

```sh
cargo build --manifest-path src/agent-sec-core/v2/Cargo.toml --locked -p asc-cli
export PATH="$PWD/src/agent-sec-core/v2/target/debug:$PATH"
printf '%s\n' '{"api_version":"aw-provider/v1alpha1","request_id":"describe-1","method":"describe"}' \
  | agent-sec-cli aw-provider
```

`describe` 和 `validate_config` 不需要运行中的 sec daemon。实际代码扫描需要
[已有 V2 daemon 部署](skillsec-v2.md)。通过
`--socket /run/agent-sec-core/daemon.sock` 选择端点；端点不可用时，CLI 不会启动
daemon 或改为本地扫描。`--timeout-ms` 限制内部 daemon 调用（默认 5000 ms），
同时受 AW 此次调用剩余 `budget_ms` 限制。

## 配置 AW

可使用完整的 [aw.yaml 示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/agent-sec-core/v2/examples/aw.yaml)。
Provider 命令为：

```yaml
argv: [agent-sec-cli, --socket, /run/agent-sec-core/daemon.sock, aw-provider]
```

确保命令解析到上述 Rust 二进制。私有配置对象放在
`spec.providers.security.config` 中，AW 原样传入：

```yaml
version: 1
mode: observe
tools:
  shell: {language: bash, input_pointer: /command}
  python: {language: python, input_pointer: /code}
```

| 字段 | 取值与行为 |
|---|---|
| `version` | 必填整数 `1`。 |
| `mode` | 必填 `observe` 或 `block`，没有隐含的默认阻断。 |
| `tools` | 必填对象，包含 1–128 个与 `event.tool.name` 精确匹配的映射。 |
| `tools.<name>.language` | `bash` 或 `python`，使用全部内置 regex 规则。 |
| `tools.<name>.input_pointer` | `event.tool.input` 内的 RFC 6901 JSON Pointer，选中值必须为字符串；空指针选中整个 input。 |

未知字段、不支持的语言及非法指针会导致配置校验失败。工具名和指针的 UTF-8
长度上限为 1024 字节，工具名区分大小写。适配器可能对原生工具名进行标准化，
应以实际 `event.tool.name` 配置，不能默认将 `Bash`、`exec`、`shell` 视为同一名称。
嵌套输入以及包含 `/`、`~` 的字段名使用标准 JSON Pointer 转义。

未映射工具返回 `observe / tool_unmapped`，表示**没有执行扫描**。已映射工具的
指针缺失或未选中字符串时返回 `invalid_tool_input`。该 Provider 可映射任意
工具名，但当前安全分析只覆盖 Bash 和 Python 源代码，不扫描任意工具输出、
PII、提示词或文件。

## 选择操作与失败行为

| AW 事件 | Provider 操作 | 结果 |
|---|---|---|
| `tool.before` | `scan_code` | `observe / code_pass`、`observe / code_risk` 或 `block / code_risk`。 |
| `tool.after` | `observe_tool` | `observe / tool_observed`，不调用扫描器。 |

`mode: block` 在扫描器返回 `warn` 或 `deny` 时请求阻断，与已有 Code Scanner
Hook 显式启用 `enable_block` 的阈值一致。这要求 AW step 的 `effects` 包含
`block`；示例已声明该能力，但保留 `mode: observe`，需要显式修改才启用阻断。
干净结果、跳过和工具后事件还需准入 `observe`。策略阻断属于成功结果，进程退出码
为零；它只增加限制，干净扫描不会覆盖 Agent 自身权限检查。

扫描失败（`scan_error`）、daemon 失败（`daemon_error`）、非法扫描输出
（`invalid_scan_result`）、通信失败（`daemon_transport_error`）和超时
（`deadline_exceeded`）都返回 `status: error`，不携带候选效果。AW 根据
`on_error` 处理，示例使用 `report`。若需要更严格的执行前策略，且 Agent
适配器支持阻断，可显式设置 `on_error: block`。故障不会伪装成成功安全判断，
请求不会自动重试。

暂不支持 `ask`、输入/结果替换、最终安全检查和 OS 强制执行。
`observe_tool` 只确认收到工具后事件，不判断工具是否成功或结果是否正确。

## 审计与停用

AW 记录 Provider 调用、配置版本、候选效果及失败状态。sec daemon 保留原有
扫描审计生命周期。Provider 响应只包含固定原因码，不将源代码、原始命中内容、
daemon 诊断或工具结果复制到 stdout/stderr。AW 输入摘要经校验后，在成功调用的
响应中原样回传。

停用时，从 `aw.yaml` 删除相应 event steps 和 Provider 对象即可，不改变既有
sec-core Hook、daemon 状态或扫描规则；回退不需要停止 sec daemon。

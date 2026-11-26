# Headless 模式

[English](../../../en/cosh-ng/core/headless-mode.md)

安装后的 `cosh` 可以运行一次有界、非交互 Core 任务。独立的
`cosh-core --headless` 继续保留面向现有客户端和前端适配器的 JSONL 协议。

## 公开单次任务

```bash
cosh --headless '解释 Linux load average'
cosh --headless --output json - < prompt.txt
cosh --headless --workspace "$PWD" --file-tools=read '查找启动入口'
cosh --headless --workspace "$PWD" --file-tools=write --exec=host \
  '复现失败测试，修复并重跑'
```

完整参数可用 `cosh --headless --help` 离线查看。任务输入是一个非空
UTF-8 参数；传 `-` 时从非 TTY stdin 读到 EOF，上限 1 MiB。直接传问题
不会消费 stdin。

文件工具默认 `none`。`read` 开启读取、grep、glob 和目录列表，`write`
额外开启编辑和写入。两种授权都要求已存在的 `--workspace`；Core 文件
操作固定在该工作区内，并拒绝路径和符号链接逃逸。公开模式不会隐式加载
全局或项目的 `.copilot-shell/CONTEXT.md`。

`--exec=host` 独立开启接受程序名和 argv 数组的 `run_command` 工具，
同样要求 `--workspace`，并从该目录、以调用 UID 启动。**这不是 Cosh
沙箱。** 命令仍可按该账号权限修改文件、访问工作区外路径和联网。命令
在无控制终端的新会话中运行，stdin 关闭，输出有界，并受进程组回收
监督。

命令非零退出会作为工具结果交回模型，允许修复和重试。`cosh` 的终态
描述整次 Agent 运行，不证明调用方自己的验收命令已通过；CI 应独立运行
该检查。

stdout 默认只写最终文本。`--output json` 输出一个含 `schema_version`、
`status`、`error_code` 和 `answer` 的对象。退出码 `0` 表示 `completed`；
`1` 覆盖 Provider、授权、策略、预算、超时和清理失败；`2` 表示输入错误。
缺凭据返回 `auth_required`，必须补充输入返回 `user_input_required`，
需要人审的工具返回 `approval_required`，不发送控制请求，也不派发同批
后续调用。
任务取消信号处理器安装失败时，返回 `runtime_unavailable` 和退出码 `1`，
不会联系 Provider；这与策略文件加载失败不同。

文本和只读任务默认超时 2 分钟，写入或宿主执行默认 5 分钟。
`--timeout` 接受秒或分钟，上限 30m，并覆盖结果输出。输出管道阻塞或断开
时非零退出，调用方须将缺失或不完整结果视为未知。SIGINT、SIGTERM 会取消
任务并清理受监督命令组；输出通道可用时 JSON 返回 `cancelled`。
SIGKILL 和主动脱离受监督组的进程不在此保证内。
`--max-turns` 接受 1–50，默认 12。每次任务另有固定的累计 30 次工具调用预算。

公开模式不发现或启动项目运行配置、Skills、Hooks、MCP 和 Extensions，
也不写可恢复 transcript。它使用受信用户／系统 Provider 配置，不进入
交互登录。管理员可在受保护的 `/etc/copilot-shell/config.toml` 设置
`[headless] allow_host_exec = false`。环境声明
`AGENT_SEC_COSH_SANDBOX_ALL_SHELL=1` 或 `true` 也会在联系 Provider 前
拒绝 `--exec=host`。强制系统配置不可读、非法或来源不可信时返回
`policy_unavailable`。
公开任务会同时落实适用的系统、用户和显式选择的审计策略；新增策略
只能收窄上述授权。非空 `COSH_AUDIT_POLICY` 必须指向可读的普通策略文件，
缺失或悬空链接返回 `policy_unavailable`。宿主命令同时按 `cosh/run_command`
和既有 `shell` 规则检查精确 argv，程序按传入形式及 basename 匹配。
这些检查不分析已授权解释器内部执行的命令，也不提供沙箱隔离。
审计记录策略决策版本，不记录原始 argv。Shell 入口还会在启动任务前核对
同目录 Core 的包版本，拒绝不匹配的安装。

## Core JSONL 一次性 prompt

```bash
cosh-core --headless "Check disk usage; do not modify anything"
```

Core 会流式输出事件，写入 `assistant` 消息，最后输出 `result` 对象。脚本需要不同设置时，可以使用 `--model`、`--approval-mode`、`--tools` 或 `--allowed-tools`。

## 长驻客户端

启动进程，并且每行发送一个 JSON 对象：

```bash
cosh-core --headless
```

```json
{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize"}}
{"type":"user","message":{"role":"user","content":"List files in current directory"}}
```

首个响应是初始化确认，随后是系统摘要：

```json
{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{...}}}}
{"type":"system","subtype":"init","session_id":"...","session_resumable":true,"model":"...","tools":[...]}
```

只有 `session_resumable` 为 `true` 时才保存 `session_id`。客户端应持续读取 `stream_event`，直到消息结束，再处理最终的 `assistant` 和 `result` 消息。

## 控制请求（Control request）

Core 可能因为工具决策或用户输入而暂停。使用相同的 request ID 回复 `control_response`：

```json
{"type":"control_request","request_id":"req-1","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"df -h"},"tool_use_id":"toolu-1"}}
{"type":"control_response","response":{"subtype":"success","request_id":"req-1","response":{"behavior":"allow","toolUseID":"toolu-1"}}}
```

Core 还可能发出 `ask_user`、`auth_required` 和可选的 `shell_evidence` 请求。Core 等待执行工具前由工具策略要求的审批、`ask_user` 的回答或 `shell_evidence` 的响应时，发送 `interrupt` 可取消当前请求。

```json
{"type":"control_request","request_id":"interrupt-1","request":{"subtype":"interrupt"}}
```

在 `initialize` 中将 `request.capabilities.can_handle_turn_cancelled` 设为 `true`，中断上述等待时就会收到带有 `error_code: "turn_cancelled"` 的 `result`。收到结果后，可以在同一个进程中发送下一条用户请求。要关闭进程，请在空闲时发送 `shutdown`。

```json
{"type":"control_request","request_id":"stop-1","request":{"subtype":"shutdown"}}
```

缺少凭据时，使用所选 `provider_id`、对应表单的 `values` 和 `persist` 回复 `auth_required`。可选 Provider 见 [Providers](providers.md)。

## 恢复或压缩会话

```bash
cosh-core --headless --resume <session-id>
cosh-core --headless --resume <session-id> --compact
```

ID 的作用域是工作空间。未传入 `--workspace <path>` 时，工作空间使用当前目录。设置 `session.auto_persist = false` 后，历史只保存在内存中，此类会话不能恢复。

## 结果与错误

成功轮次以类似下面的 result 结束：

```json
{"type":"result","subtype":"success","is_error":false,"session_id":"...","duration_ms":1234}
```

成功时省略可选的 `result` 字段。

处理一般错误结果前，先检查 `error_code: "turn_cancelled"`。失败结果使用 `is_error: true` 并包含 `errors`。会话加载或持久化失败还会包含 `session_error_code` 和 `session_error_phase`。无效 JSONL 输入会输出错误 result，并以非零状态退出。stdout 只保留协议消息，诊断信息写入其他日志。

结果消息和 v1 兼容行为见开发者 [IPC 协议参考](../../../../../src/cosh-ng/docs/design/zh/ipc-protocol.md#cosh-core-headless-jsonl-取消)。

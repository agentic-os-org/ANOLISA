# Claude SDK

[English](../../../en/aw/agents/claude-sdk.md)

从 `src/aw` 运行源码示例；先按[接入教程](../integration.md)构建 AW。

## 启动 Claude SDK worker

已验证基线为 Claude Agent SDK 0.2.88 及其 bundled CLI 2.1.161。其他版本沿用
原生 client、Hook、身份与 transport 生命周期检查。Adapter 记录已安装的 SDK 版本，启动一个前台
Python script 或 `-m` module，要求 worker 公共工厂显式使用
`aw_claude.create_client(options)`。模型认证继续由 SDK 配置。AW 为启动的 worker
通过 `PYTHONPATH` 提供此模块，不会自动向任意 SDK 应用注入 Hooks。

源码示例从 `src/aw` 运行，先准备选定的 Python 环境。固定 MCP 版本是为了处理已
复现的 MCP 2.x 不兼容问题：

```bash
python3 -m venv target/claude-sdk-venv
target/claude-sdk-venv/bin/pip install -r tests/claude-sdk-requirements.txt
```

复制 `crates/aw-service/examples/aw.claude-sdk.yaml` 为自己的配置，将 Agent argv
解释器改为 `target/claude-sdk-venv/bin/python` 的绝对路径。保留前台示例 worker 路径，
或替换为业务 script。在公共工厂完成普通 MCP tools、权限、resume 与既有 Hooks
配置后使用以下模式：

```python
from aw_claude import create_client

async with create_client(options) as client:
    await client.query(prompt)
    async for message in client.receive_response():
        handle_message(message)
```

```bash
target/debug/aw run --config /absolute/aw.claude-sdk.yaml --agent claude -- 'Use echo to return hello.'
target/debug/aw status --config /absolute/aw.claude-sdk.yaml
target/debug/aw stop --config /absolute/aw.claude-sdk.yaml
```

Claude SDK 事件预算必须为 1..55,000 ms。每个 `input.submit`、`tool.before` 或 `tool.after` 路由
优先使用 `events.<name>.budget_ms`，未设置时使用 `execution.default_event_budget_ms`。
`aw run` 在静态配置校验后检查此 Adapter 专有限制。

worker options cwd 必须等于启动 cwd。真实 SDK initialization 握手接受 callbacks
后才发布 readiness。AW 在 `PreToolUse` 观察或拒绝，`PostToolUse` 与
`PostToolUseFailure` 均供 after 观察。被拒绝工具不补造 after。既有 Hook veto 和
原生权限继续有效。Python SDK 的既有 pre-hook `updatedInput` 或包装后的 permission
`updated_input` 发生修改时明确拒绝。CLI 加载的 settings 和插件 Hook 独立调度，
可以在 AW 的公共 `PreToolUse` 检查后改写普通工具参数；改写后的最终参数在执行前
不会再经过 AW 检查。前置策略仅覆盖该 Hook 收到的输入；需要最终参数强制检查的应用
须另行采用经过验收的执行边界。启用
`tool.before` 时，非空 `PermissionRequest` matchers 在初始化前被拒绝；请改用
受支持的 `PreToolUse` 或 `can_use_tool` callbacks。启用 `tool.before` 时也拒绝调用方
指定 `permission_prompt_tool_name`；受保护 `can_use_tool` 的 SDK 内部 stdio 权限
通道继续支持。调用方
`cli_path` 覆盖、缺失 bundled binary 均被拒绝。worker 不得 detach，也不得在一个
launcher binding 中运行多个 SDK clients/任务；Gunicorn 和业务部署需要自己的任务
生命周期接线。

SDK 0.2.88 + CLI 2.1.161 实测：MCP 工具失败（SDK handler 合同的
`is_error`，映射为线上 `isError`）与原生 Bash 执行失败一样以
`PostToolUseFailure` 到达钩子；after 观察仍不能分类所有业务失败。身份、取消与验证限制见
[Adapter 合同](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/docs/design/zh/adapters/claude-sdk-adapter.md)。

工厂保留 `setting_sources`（包括原生默认发现）、内联或文件形式的 `settings`、
本地 `plugins` 和 SDK/HTTP/stdio `mcp_servers`。SDK 按原生顺序加载原始来源并发现
项目 Skills；AW 沿用其加载逻辑，配置文件内容保持不变。上文的 callback 组合与运行时
限制继续有效。`extra_args` 仍只允许 `strict-mcp-config: None`；settings 和 MCP 使用
SDK 对应选项传入。模型和认证配置不变。配置兼容范围与 Hook 效果分别验收，
Skills/plugins 内部执行的命令仍需单独核对覆盖边界。
常驻客户端在接受 query 前读取 CLI 合并后的 settings。Anthropic Messages 中继沿用
合并环境中的模型地址、代理和 TLS 配置，认证仍由 CLI 处理；会话内 settings 覆盖将
模型响应路由到 AW，原配置文件保持不变。原生配置控制或中继路由确认失败时，connect
直接失败。该路径要求已验收的 SDK 0.2.88 / bundled CLI 2.1.161 配置控制接口。
中继在 connect 时固定上游环境；修改模型地址、代理或 TLS 配置后请创建新客户端。
当前 sec-core Provider 提供提交、工具 PII 和 Skill 检查操作。Provider 操作和
Adapter 能力分别准入：`scan_code` 和 `scan_tool_input` 检查 Claude callback 输入；
`input.submit` 支持下述文本工厂路径；强制输出 `on_error: withhold_result` 仍在 worker 启动前被拒绝。
原生 Skill 发现继续可用；sec-core `check_skill` 的解析合同目前验收于 Qoder SDK 和
QwenPaw，Claude 尚无对应解析合同。共享配置前请核对 [Provider 指南](../providers/sec-core.md)。

直接任务 worker 模式每进程只允许一个 client；多个任务客户端使用[常驻入口](../deployment/resident.md)。

当前 `adapter: claude-sdk` 支持上述受限前台 worker 工厂；`aw agent connect` 对该
Adapter 不提供持久安装命令，所有 `--native-*` overrides 均不支持。

### 启用 Bash PreTool 与 PostTool 优化

使用 [Tokenless 共享配置](https://github.com/agentic-os-org/ANOLISA/blob/main/providers/tokenless/examples/aw.yaml)，
选择 `claude` target，替换 executable、worker 与状态目录的绝对路径。源码示例
`claude-worker.py` 除 echo 工具外显式开放并允许原生 Bash；组合业务工厂时保留应用
自己的权限策略。Provider 安装与统计设置见 [Tokenless 指南](../../../../user-guide/zh/aw/tokenless.md)。

PreTool 阻断原始 Bash 尝试，将重试参数返回模型；新调用重新经过所有策略及原生权限。
PostTool 只替换完成、非图片且未中断的 Bash stdout。完成观察器和自定义步骤放在唯一
最终 `replace_result` 前，看到原始结果；stderr 与其余原生字段保留。失败/report
保留原始结果。原生 HTTP MCP 工具投影模型可见字符串（structuredContent 序列化串，
或纯文本结果的单一文本块），替换以该字符串在下一模型请求生效；多块与媒体结果
保持原样。原生回调仍缺少可靠业务错误状态，MCP 错误分类继续依赖
[受管 MCP 工厂](../deployment/resident.md)。

固定版本原生测试由[贡献指南](../../../../../src/aw/CONTRIBUTING_zh.md)统一运行。
常驻客户端与受管 MCP 见[常驻部署](../deployment/resident.md)。

## 提交前检查用户文本

使用 [Claude 提交配置](../../../../../src/aw/crates/aw-service/examples/aw.claude-submission.yaml)
启用 `scan_prompt` 和 `scan_user_input`。替换程序及 socket 路径，单独启动 sec-core，
保留应用现有 options。示例选择常驻入口；前台 worker 可以沿用原 argv。
默认配置在原生提交前拒绝风险及扫描故障。在 `query` 或常驻 `connect(prompt)` 外捕获
`aw_submission.SubmissionDenied`；异常返回后，同一已连接客户端可以接收下一次输入。
拒绝的输入没有可供消费的响应流。

| 启用检查后的工厂路径 | 支持的输入 |
| --- | --- |
| 前台 `async with create_client(options)` 后调用 `query` | 字符串或单消息异步迭代器；每个 worker 一个客户端 |
| 常驻 `connect(prompt)` 和 `query` | 字符串或单消息异步迭代器；上一轮排空后保留会话继续使用 |
| 异步消息 content | 字符串，或非空的 `{type: "text", text: "..."}` 文本块列表 |

异步信封必须包含 `type: "user"`，以及仅包含 `role: "user"` 和 `content` 的 `message`。
可选字段为 `session_id`、`uuid` 和空值 `parent_tool_use_id`。AW 在扫描前读完并关闭
迭代器；拒绝多消息、媒体、未知字段、空白文本及编码后 AW 输入超过 256 KiB 的提交。
会话标签必须非空白且不超过 512 个 UTF-8 字节。移除 `input.submit` 后沿用原有输入模式。

所有步骤共享不可变快照、轮次身份和 Service 事件预算。扫描时以换行连接文本块，
交给原生 SDK 的消息保留原始文本块；字符串原样转交。检查先于原生 `UserPromptSubmit`
Hook，保留已有 Hook、原生拒绝和设置。这些 Hook 添加的上下文及后续改写超出本次输入
快照范围。扫描对象为本次用户文本，覆盖范围不含完整会话或最终模型 Prompt。

已审计且可恢复的 Provider 故障遵循 `on_error`：`block` 拒绝本次输入；`report` 记录
故障，在事件预算仍可用时继续。两种故障策略下风险拒绝均有效。协议、身份、scope、
审计故障及无法确认的清理会阻止后续提交。取消等待扫描进程和事件清理后再传播，
清理成功后允许下一轮。已接收输入必须排空响应后再 query；输入准入期间不能接收响应。
并发 disconnect 会取消并等待准入完成。

原生 `connect(str)` 与 `can_use_tool` 并用时仍遵循 SDK 限制；先不带 Prompt 连接再
调用 `query`，或使用受支持的异步消息。

Service 在客户端/binding 释放前保留有界防重放记录。每个常驻客户端最多接纳 1,024 次
受检提交，应在达到上限前创建新客户端。前台 binding 与旧工具事件共用这部分容量。
每次检查后均释放活动输入事件及扫描调用。

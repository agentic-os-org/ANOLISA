# Claude SDK

[English](../../../en/aw/agents/claude-sdk.md)

从 `src/aw` 运行源码示例；先按[接入教程](../integration.md)构建 AW。

## 启动 Claude SDK worker

使用 Claude Agent SDK 0.2.88 及其 bundled CLI 2.1.161。Adapter 启动一个前台
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

Claude SDK 事件预算必须为 1..55,000 ms。每个 `tool.before` 或 `tool.after` 路由
优先使用 `events.<name>.budget_ms`，未设置时使用 `execution.default_event_budget_ms`。
`aw run` 在静态配置校验后检查此 Adapter 专有限制。

worker options cwd 必须等于启动 cwd。真实 SDK initialization 握手接受 callbacks
后才发布 readiness。AW 在 `PreToolUse` 观察或拒绝，`PostToolUse` 与
`PostToolUseFailure` 均供 after 观察。被拒绝工具不补造 after。既有 Hook veto 和
原生权限继续有效。既有 pre-hook `updatedInput` 或 permission `updated_input`
发生修改时明确拒绝，因为并发 callbacks 无法保证 AW 检查最终参数。启用
`tool.before` 时，非空 `PermissionRequest` matchers 在初始化前被拒绝；请改用
受支持的 `PreToolUse` 或 `can_use_tool` callbacks。启用 `tool.before` 时也拒绝调用方
指定 `permission_prompt_tool_name`；受保护 `can_use_tool` 的 SDK 内部 stdio 权限
通道继续支持。调用方
`cli_path` 覆盖、缺失 bundled binary 均被拒绝。worker 不得 detach，也不得在一个
launcher binding 中运行多个 SDK clients/任务；Gunicorn 和业务部署需要自己的任务
生命周期接线。

CLI 2.1.161 对 SDK MCP `isError: true` 响应仍发 `PostToolUse`，callback 数据不含
该 flag，因此 after 观察不能分类所有业务失败。原生 Bash 执行失败使用
`PostToolUseFailure`。身份、取消与验证限制见
[Adapter 合同](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/docs/design/zh/adapters/claude-sdk-adapter.md)。

工厂要求 `setting_sources=[]`、无 `settings` 覆盖、无 CLI plugins。`extra_args`
仅允许 `strict-mcp-config: None`；SDK 原生 `strict_mcp_config` 也保留。模型和认证
配置不变。通过 settings sources 发现项目 skills 的应用，需要先明确接入设计才能
使用该首版受限 Bridge。直接任务 worker 模式每进程只允许一个 client；多个任务客户端使用[常驻入口](../deployment/resident.md)。

当前 `adapter: claude-sdk` 支持上述受限前台 worker 工厂；`aw agent connect` 对该
Adapter 不提供持久安装命令，所有 `--native-*` overrides 均不支持。

### 启用 Bash PreTool 与 PostTool 优化

使用 [Tokenless 共享配置](https://github.com/agentic-os-org/ANOLISA/blob/main/providers/tokenless/examples/aw.yaml)，
选择 `claude` target，替换 executable、worker 与状态目录的绝对路径。源码示例
`claude-worker.py` 除 echo 工具外显式开放并允许原生 Bash；组合业务工厂时保留应用
自己的权限策略。Provider 安装、RTK 要求与统计设置见 [Tokenless 指南](../../../../user-guide/zh/aw/tokenless.md)。

PreTool 阻断原始 Bash 尝试，将重试参数返回模型；新调用重新经过所有策略及原生权限。
PostTool 只替换完成、非图片且未中断的 Bash stdout。完成观察器和自定义步骤放在唯一
最终 `replace_result` 前，看到原始结果；stderr 与其余原生字段保留。失败/report
保留原始结果。该原生回调路径的 MCP tools 仍可观察，缺少可靠业务错误状态；使用[受管 MCP 工厂](../deployment/resident.md)获得结果压缩接点。

固定版本原生测试由[贡献指南](../../../../../src/aw/CONTRIBUTING_zh.md)统一运行。
常驻客户端与受管 MCP 见[常驻部署](../deployment/resident.md)。

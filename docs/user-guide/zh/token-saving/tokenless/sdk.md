# Tokenless Python SDK

[English](../../../en/token-saving/tokenless/sdk.md)

Tokenless 提供两层 Python SDK：

| 层级 | 包 | 用途 |
|------|----|------|
| 通用 SDK | `anolisa-tokenless` | 接入任意 Agent 生命周期、执行单项 Tokenless 操作或查询统计 |
| AgentScope 集成 | `anolisa-tokenless-agentscope` | 把通用 SDK 挂载到受支持的 AgentScope 1.x 和 2.x 生命周期 API |

AgentScope 层依赖完全相同版本的通用 SDK，并把 Tokenless 操作交给通用 SDK 执行；
它不是另一套压缩实现。本页介绍两层的关系。AgentScope 详细用法放在
[AgentScope SDK 集成](sdk/agentscope.md) 子文档，产品 Plugin 仍放在
[Agent 集成](framework-integration.md)。

## 第一层：通用 SDK

`anolisa-tokenless` Wheel 让 Python 应用可以在进程内运行 Tokenless。把 Tokenless 接入
Agent 生命周期时使用 `TokenlessSdk`。不需要接入生命周期、只想执行某一项具体操作时
使用 `TokenlessRuntime`，例如单独压缩一个响应或恢复一条 Stash 内容。只查询统计时
使用 `TokenlessStats`。

### 从 GitHub Release 安装

从 [v0.7.14](https://github.com/alibaba/anolisa/releases/tag/tokenless/v0.7.14) 开始，
Tokenless GitHub Release 会附带官方 SDK Wheel。Wheel 需要 CPython 3.11 或更高版本，
请根据目标系统选择原生 `anolisa-tokenless` Wheel：

| 系统 | Release 产物 |
|------|--------------|
| Linux x86_64 | `anolisa_tokenless-<version>-cp311-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64.whl` |
| Linux aarch64 | `anolisa_tokenless-<version>-cp311-abi3-manylinux_2_17_aarch64.manylinux2014_aarch64.whl` |
| macOS Apple 芯片 | `anolisa_tokenless-<version>-cp311-abi3-macosx_11_0_arm64.whl` |

下文的生命周期 API 示例要求 Tokenless 0.8.0。在 Linux x86_64 上把 v0.8.2 安装到虚拟环境：

```bash
python3 -m venv .venv
. .venv/bin/activate
python -m pip install \
  "https://github.com/alibaba/anolisa/releases/download/tokenless/v0.8.2/anolisa_tokenless-0.8.2-cp311-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64.whl"
```

Linux 产物面向兼容 `manylinux_2_17` 的 glibc 发行版，不支持 Alpine Linux 等 musl 发行版。
Release 同时提供 `SHA256SUMS-python-wheels.txt`，可用于校验下载内容。

Wheel 包含原生 Tokenless Runtime 和匹配的 RTK 可执行文件，不需要 `tokenless` CLI、系统
RTK 或独立 TOON 可执行文件。

### 从源码构建

本仓库仅支持在 Linux 上从源码构建。请在 Tokenless 组件目录中构建，并确保系统可发现
CPython 3.11 或更高版本的开发环境：

```bash
make python-wheel
python3 -m venv /tmp/tokenless-sdk
/tmp/tokenless-sdk/bin/pip install target/wheels/anolisa_tokenless-*.whl
```

`make python-wheel` 默认通过 `uvx` 提供 Maturin。请先安装
[`uv`](https://docs.astral.sh/uv/)，也可以直接使用 `PATH` 中已有的兼容 Maturin：

```bash
make python-wheel MATURIN=maturin
```

Pip 会以展开形式安装 Wheel，从而为命令改写提供 Wheel 内置 RTK 所需的稳定可执行路径。

### 选择 API

| API | 职责 | 适用场景 |
|-----|------|----------|
| `TokenlessSdk` | 生命周期集成 | 把 Tokenless 接入 Agent 框架的 Model 调用和工具调用阶段 |
| `TokenlessRuntime` | 单项操作 | 直接压缩一个 Schema、响应或 TOON Payload，或恢复一条 Stash 内容 |
| `TokenlessStats` | 统计查询 | 读取状态、汇总、最近记录、记录详情、Diff 和 Session 对比 |

新接入 Agent 框架时建议使用 `TokenlessSdk`。它持有一个 `TokenlessRuntime`，通过
`sdk.runtime.data_dir` 暴露相同状态目录，并在查询统计时延迟创建 `sdk.stats`。

### 完整生命周期示例

下面的示例会压缩模型可见工具 Schema、通过 PostTool 处理一次成功的 API 结果，并恢复
一个通过 Marker 授权的 Stash Payload。Core 负责压缩策略和 TOON 选择；SDK 只转换四种
生命周期操作。

```python
import asyncio
import json
import tempfile
from pathlib import Path

from anolisa_tokenless import (
    Attribution,
    BeforeModelCapabilities,
    BeforeModelRequest,
    ContentOrigin,
    OutputOptimization,
    PostToolCapabilities,
    PostToolRequest,
    RecoveryMethod,
    ResultKind,
    RetrieveRequest,
    TokenlessConfig,
    TokenlessSdk,
    ToolResultStatus,
)


async def main() -> None:
    with tempfile.TemporaryDirectory(prefix="tokenless-sdk-") as data_dir:
        sdk = TokenlessSdk(
            TokenlessConfig(
                data_dir=Path(data_dir),
                rtk_enabled=False,
            )
        )
        model_attribution = Attribution("my-agent", "session-42")
        tool = {
            "type": "function",
            "function": {
                "name": "lookup",
                "description": "Detailed lookup instructions. " * 100,
                "parameters": {"type": "object", "properties": {}},
            },
        }

        model_result = await sdk.before_model(
            BeforeModelRequest(
                tools=(tool,),
                visible_context="",
                capabilities=BeforeModelCapabilities(
                    replace_tools=True,
                    recovery=RecoveryMethod.tool("tokenless_retrieve"),
                ),
                attribution=model_attribution,
            )
        )
        print([item.get("function", {}).get("name") for item in model_result.tools])

        original = json.dumps(
            {"items": [{"name": "same", "value": index} for index in range(300)]}
        )
        result = await sdk.post_tool(
            PostToolRequest(
                result_kind=ResultKind.TOOL,
                tool_name="api",
                content=original,
                status=ToolResultStatus.SUCCESS,
                content_origin=ContentOrigin.API_RESPONSE,
                output_optimization=OutputOptimization.NONE,
                capabilities=PostToolCapabilities(
                    replace_output=True,
                    recovery=RecoveryMethod.tool("tokenless_retrieve"),
                    replace_with_text=True,
                ),
                attribution=Attribution("my-agent", "session-42", "tool-7"),
            )
        )
        print(result.disposition, len(original), len(result.output))

        next_model = await sdk.before_model(
            BeforeModelRequest(
                tools=(),
                visible_context=result.output,
                capabilities=BeforeModelCapabilities(True, RecoveryMethod.tool("tokenless_retrieve")),
                attribution=model_attribution,
            )
        )
        visible_markers = next_model.visible_markers
        if visible_markers:
            marker_hash = next(iter(visible_markers))
            recovered = await sdk.retrieve(
                RetrieveRequest(marker_hash, visible_markers, model_attribution)
            )
            print(f"recovered {len(recovered.payload)} characters")


asyncio.run(main())
```

`TemporaryDirectory` 让示例可以独立运行，并会在退出时删除状态。生产环境应使用稳定、可写
的绝对 `data_dir`，并为每个租户或安全边界使用不同目录。

SDK 把生命周期值视为不可变契约，不修改调用方持有的 Schema、参数或工具结果。请保留
每个操作的 Response，并把其中的显式状态传递到下一个宿主边界。

### 四个生命周期接缝

#### Model 调用前

```python
request = await sdk.before_model(
    BeforeModelRequest(
        tools=tuple(model_tools),
        visible_context=visible_context,
        capabilities=BeforeModelCapabilities(True, RecoveryMethod.tool("tokenless_retrieve")),
        attribution=attribution,
    )
)
```

`before_model()` 仅在 `RecoveryMethod.tool(name)` 下允许 Schema 的可恢复截断，Integration
必须已经注册该静态 Tool。`RecoveryMethod.shell()` 允许 PostTool 命令恢复，但不启用 Schema
截断；`RecoveryMethod()` 表示不可恢复。Core 从转换后的工具和可见 Context 收集完整的 Shell
指令、名称匹配所声明 Tool 的指令，以及历史 `<<tokenless:HASH>>` Marker，返回排序去重的小写
Hash；孤立 Hash 不构成授权依据。Core 不发布 Agent Tool。Tool 名称限 1–64 个 ASCII 字母、
数字、下划线或连字符。

#### 工具调用前

```python
call = await sdk.pre_tool(
    PreToolRequest(
        tool_name="shell",
        arguments={"command": "grep needle large.log"},
        command_field="command",
        capabilities=PreToolCapabilities(
            replace_arguments=True,
            block_and_suggest=False,
        ),
        attribution=Attribution("my-agent", "session-42", "tool-8"),
    )
)
```

Core 只处理显式指定的 `command_field`。如果 RTK 产生改写，Response 的 Action 为
`replace_arguments`，参数包含 Wheel 内置 RTK 路径，并返回 `output_optimization=rtk`。
应执行返回参数，并把该优化状态传给 PostTool。关闭 RTK 是 Adapter 的选择：
`TokenlessConfig.rtk_enabled` 为 false 时不要调用 `pre_tool()`。

#### 工具调用后

```python
result = await sdk.post_tool(
    PostToolRequest(
        result_kind=ResultKind.TOOL,
        tool_name=tool_name,
        content=model_visible_text,
        status=ToolResultStatus.SUCCESS,
        content_origin=ContentOrigin.API_RESPONSE,
        output_optimization=call.output_optimization,
        capabilities=PostToolCapabilities(True, RecoveryMethod.tool("tokenless_retrieve"), True),
        attribution=attribution,
    )
)
```

`content_origin` 必须来自工具注册契约，不得从结果文本推断。Core 统一路由 Retrieve 输出、
错误、中断或拒绝、RTK 已优化输出和普通成功输出，并返回最终内容、Disposition、操作轨迹、
可恢复性、Token 数量、Stash Key 与可选诊断上下文。Adapter 应透传中间 Streaming Chunk，
只对最终模型可见文本调用 PostTool。

#### 受 marker 约束的恢复

```python
payload = await sdk.retrieve(
    RetrieveRequest(marker_hash, current_before_model.visible_markers, attribution)
)
```

恢复接受完整 Marker 或 24 位十六进制字符，并对照当前 BeforeModel Response 返回的精确
Marker 集合授权。该集合应被视为一次 Model Call 的状态，不要累计 Session 历史中出现过的
所有 Marker。`RetrieveResponse.payload` 是 byte-exact 内容，Adapter 不得把它再次送入
PostTool。

### 配置

```python
config = TokenlessConfig(
    data_dir="/absolute/path/to/tenant-tokenless-data",
    retrieve_tool_name="tokenless_retrieve",
    rtk_enabled=True,
)
```

`data_dir` 必须是可写的绝对路径。每个租户或安全边界应使用不同目录；
`TOKENLESS_DATA_DIR` 只是进程级回退。`retrieve_tool_name` 为 AgentScope 等 Framework Layer
选择 Integration 自有的 Tool 名称，Integration 通过恢复能力将该名称声明给 Core。
名称必须由 1–64 个 ASCII 字母、数字、下划线或连字符组成。这也收紧了已有
`retrieve_tool_name` 配置的校验：含点号、冒号或空格的名称必须在升级前重命名。
`rtk_enabled` 控制 SDK 是否为 PreTool
解析 Wheel 内置 RTK。压缩阈值、内容检测、TOON 选择、诊断、授权和 Stash 策略都属于 Core
行为，不是 Python 配置。

### Runtime 直接调用示例

不需要由 `TokenlessSdk` 协调 Agent 生命周期、希望直接执行 Tokenless 操作时，使用
`TokenlessRuntime`。先为数据目录创建一个 Runtime：

```python
import json
import re
from anolisa_tokenless import TokenlessRuntime

runtime = TokenlessRuntime("/absolute/path/to/tokenless-data")
```

#### 压缩响应

```python
original_response = json.dumps(
    {"items": [f"record-{index:04d}" for index in range(200)]}
)
response_result = runtime.compress_response(
    original_response,
    truncate_arrays_at=32,
    agent_id="my-agent",
    session_id="session-42",
    tool_use_id="tool-7",
    require_reversible=True,
)
model_visible_response = response_result.output
print(response_result.disposition, response_result.before_tokens, response_result.after_tokens)
```

#### 压缩工具 Schema

```python
tool_schema = {
    "type": "function",
    "function": {
        "name": "lookup",
        "description": "Detailed lookup instructions. " * 100,
        "parameters": {"type": "object", "properties": {}},
    },
}
schema_result = runtime.compress_schema(
    json.dumps(tool_schema),
    agent_id="my-agent",
    session_id="session-42",
)
model_visible_schema = json.loads(schema_result.output)
```

#### 编码为 TOON

```python
records = {
    "items": [
        {"name": f"item-{index:04d}", "status": "ready"}
        for index in range(100)
    ]
}
toon_result = runtime.compress_toon(
    json.dumps(records),
    agent_id="my-agent",
    session_id="session-42",
    tool_use_id="tool-8",
)
model_visible_text = toon_result.output
```

如果 TOON 不能减少预估 Token 数量，`compress_toon()` 会保留原始 JSON。

#### 恢复 Stash 内容

低层响应或 Schema 压缩默认使用 Shell 恢复指令：

```python
hash_match = re.search(r"If needed, run in shell: tokenless retrieve ([0-9A-Fa-f]{24})(?![\w-])", response_result.output)
if hash_match is not None:
    recovered_content = runtime.retrieve(hash_match.group(1))
    print(recovered_content)
```

`retrieve()` 可以接收 24 个字符的 Hash 或历史 Marker。直接调用 Runtime
时，需要由调用方决定允许恢复哪些 Marker；`TokenlessSdk.retrieve()` 会把当前 BeforeModel
Marker 集合交给 Core 授权。

Runtime 的输入输出都是字符串。下游应直接使用各个 `CompressionResult.output`；需要了解
输入是否以及如何变化时，再检查它的 `disposition`、Token 数量和 Stash 字段。

### 查询统计

```python
from anolisa_tokenless import TokenlessStats

stats = TokenlessStats("/absolute/path/to/tokenless-data")
status = stats.status
summary = stats.summary()
recent = stats.list(limit=20)

print(status.database_path, summary.total.tokens_saved)
if recent:
    record = stats.show(recent[0].id)
    change = stats.diff(record_id=record.id)
```

Session 总览使用 `stats.diff(session_id="...")`；单次工具生命周期使用
`stats.diff(session_id="...", tool_use_id="...")`；dry-run 与 active Session 对比使用
`stats.compare("baseline-session", "tokenless-session")`。

Token 数量是估算值，并且只有产生正向节省的操作才会记录。`list()`、`summary()` 和
`compare()` 不返回保存内容；`show()` 和详细 `diff()` 结果可能包含敏感工具输入或输出。
公开查询 API 不会清空数据或修改设置，但打开客户端时可能创建或迁移 `stats.db`，因此选定
的数据目录必须可写。

## 第二层：AgentScope 集成

`anolisa-tokenless-agentscope` 把通用 SDK 生命周期映射到 AgentScope。应用代码使用
`TokenlessAgentScope`，不需要自行调用 `before_model()`、`pre_tool()`、`post_tool()` 和
`retrieve()`。该集成还会把 AgentScope Session 与 Tool Call 归属传入
通用 SDK。

支持版本、构建安装、1.x/2.x/App 完整示例、配置、恢复边界和验证见
[AgentScope SDK 集成](sdk/agentscope.md)。Claude Code、OpenCode 等产品 Adapter 与这两层
Python SDK 都是不同的接入方式。

## 验证两层 SDK

构建通用 SDK Wheel 并运行 installed-wheel 测试：

```bash
make python-wheel
make test-python-runtime
```

根据 [子文档](sdk/agentscope.md#验证集成) 中的命令单独验证 AgentScope 层。

## 相关文档

- [Agent 集成](framework-integration.md)
- [AgentScope SDK 集成](sdk/agentscope.md)
- [CLI 参考](cli-reference.md)
- [效果度量](measuring-savings.md)
- [配置与数据隐私](configuration-and-privacy.md)
- [Runtime 设计](../../../../../src/tokenless/docs/design/runtime-library_zh.md)

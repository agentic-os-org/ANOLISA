# Tokenless Python SDK

[中文版](../../../zh/token-saving/tokenless/sdk.md)

Tokenless provides two Python SDK layers:

| Layer | Package | Purpose |
|-------|---------|---------|
| Framework-neutral SDK | `anolisa-tokenless` | Integrate any Agent lifecycle, invoke individual Tokenless operations, or query statistics |
| AgentScope integration | `anolisa-tokenless-agentscope` | Attach the framework-neutral SDK to the supported AgentScope 1.x and 2.x lifecycle APIs |

The AgentScope layer depends on the exact same version of the framework-neutral SDK and delegates
Tokenless operations to it; it is not a separate compression implementation. This page introduces
both layers. Detailed AgentScope usage lives in the
[AgentScope SDK integration](sdk/agentscope.md) child document, while product plugins remain in
[Agent integration](framework-integration.md).

## Layer 1: Framework-neutral SDK

The `anolisa-tokenless` wheel lets Python applications run Tokenless in process. Use
`TokenlessSdk` when integrating Tokenless into an Agent lifecycle. Use `TokenlessRuntime` when you
do not need lifecycle integration and only want to invoke a specific operation, such as compressing
one response or retrieving one Stash entry. Use `TokenlessStats` only for statistics queries.

### Install from GitHub Release

Official SDK wheels are attached to Tokenless GitHub Releases starting with
[v0.7.14](https://github.com/alibaba/anolisa/releases/tag/tokenless/v0.7.14). They require
CPython 3.11 or later. Select the native `anolisa-tokenless` wheel for the target system:

| System | Release asset |
|--------|---------------|
| Linux x86_64 | `anolisa_tokenless-<version>-cp311-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64.whl` |
| Linux aarch64 | `anolisa_tokenless-<version>-cp311-abi3-manylinux_2_17_aarch64.manylinux2014_aarch64.whl` |
| macOS Apple silicon | `anolisa_tokenless-<version>-cp311-abi3-macosx_11_0_arm64.whl` |

The lifecycle API examples below require Tokenless 0.8.0. Install v0.8.3 on Linux x86_64
into a virtual environment:

```bash
python3 -m venv .venv
. .venv/bin/activate
python -m pip install \
  "https://github.com/alibaba/anolisa/releases/download/tokenless/v0.8.3/anolisa_tokenless-0.8.3-cp311-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64.whl"
```

The Linux assets target glibc-based distributions compatible with `manylinux_2_17`; they do not
support Alpine Linux or other musl-based distributions. The Release also includes
`SHA256SUMS-python-wheels.txt` for download verification.

The wheel contains the native Tokenless runtime and the matching RTK executable. It does not need
the `tokenless` CLI, a system RTK binary, or a separate TOON executable.

### Build from source

Source builds in this repository are Linux-only. Build the SDK from the Tokenless component
directory with a discoverable CPython 3.11 or later development environment:

```bash
make python-wheel
python3 -m venv /tmp/tokenless-sdk
/tmp/tokenless-sdk/bin/pip install target/wheels/anolisa_tokenless-*.whl
```

`make python-wheel` uses `uvx` to provide Maturin by default. Install
[`uv`](https://docs.astral.sh/uv/) first, or use a compatible Maturin already on `PATH`:

```bash
make python-wheel MATURIN=maturin
```

Pip installs the wheel in unpacked form, which gives the packaged RTK executable the stable path
required by command rewriting.

### Choose an API

| API | Role | Use it for |
|-----|------|------------|
| `TokenlessSdk` | Lifecycle integration | Connect Tokenless to the model-call and tool-call stages of an Agent framework |
| `TokenlessRuntime` | Individual operations | Directly compress one schema, response, or TOON payload, or retrieve one Stash entry |
| `TokenlessStats` | Statistics queries | Read status, summaries, recent records, record details, diffs, and session comparisons |

`TokenlessSdk` is the recommended integration surface for a new agent framework. It owns one
`TokenlessRuntime`, exposes the same state directory through `sdk.runtime.data_dir`, and creates
`sdk.stats` lazily when statistics are queried.

### Complete lifecycle example

This example compresses a model-visible tool schema, routes a successful API result through
PostTool, and recovers one marker-authorized Stash payload. Core owns compression policy and TOON
selection; the SDK only translates the four lifecycle operations.

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

`TemporaryDirectory` keeps the example self-contained and deletes its state on exit. In production,
use a stable, writable absolute `data_dir`, with a different directory for every tenant or security
boundary.

The SDK treats lifecycle values as immutable contracts and does not modify caller-owned schemas,
arguments, or tool results. Keep each operation response and carry its explicit state into the next
host boundary.

### The four lifecycle seams

#### Before a model call

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

`before_model()` permits recoverable schema truncation only with `RecoveryMethod.tool(name)`;
the integration must already have registered that static Tool. `RecoveryMethod.shell()` permits
PostTool command recovery but does not enable schema truncation. `RecoveryMethod()` means no
recovery. Core scans transformed tools and visible context for complete shell instructions,
instructions naming the declared Tool, and historical `<<tokenless:HASH>>` markers. It returns
sorted, deduplicated lowercase hashes; an isolated hash does not authorize retrieval. Core never
publishes an Agent tool. Tool names allow 1–64 ASCII letters, digits, underscores, or hyphens.

#### Before a tool call

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

Core considers only the explicitly named `command_field`. If RTK has a rewrite, the response uses
`replace_arguments`, contains the packaged RTK path, and reports `output_optimization=rtk`. Execute
the returned arguments and carry that optimization value into PostTool. A disabled RTK is an
adapter choice: when `TokenlessConfig.rtk_enabled` is false (the default), `pre_tool()`
returns the original arguments with `passthrough` and `output_optimization=none` without resolving RTK.
Set `rtk_enabled=True` to opt in; a non-empty `TOKENLESS_RTK_ENABLED` overrides this setting
(`1`, `true`, or `yes`, case-insensitively, enable it; other non-empty values disable it).

#### After a tool call

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

Set `content_origin` from the tool's registered contract; do not infer it from result text. Core
routes Retrieve output, errors, interrupted or denied calls, RTK-optimized output, and ordinary
successful output. It returns the final output plus disposition, operations, recoverability, token
counts, Stash keys, and optional diagnostic context. Adapters should pass intermediate streaming
chunks through and call PostTool only for final model-visible text.

#### Marker-scoped retrieval

```python
payload = await sdk.retrieve(
    RetrieveRequest(marker_hash, current_before_model.visible_markers, attribution)
)
```

Retrieval accepts a complete marker or 24 hexadecimal characters and authorizes it against the
exact marker set returned by the current BeforeModel response. Treat that set as model-call state
instead of accumulating every marker ever seen in a session. `RetrieveResponse.payload` is the
byte-exact content; adapters must not send it through PostTool again.

### Configuration

```python
config = TokenlessConfig(
    data_dir="/absolute/path/to/tenant-tokenless-data",
    retrieve_tool_name="tokenless_retrieve",
    rtk_enabled=False,
)
```

`data_dir` must be absolute and writable. Use a different directory for every tenant or security
boundary; `TOKENLESS_DATA_DIR` is only a process-wide fallback. `retrieve_tool_name` selects the
integration-owned tool name for framework layers such as AgentScope; they declare that name to
Core through the recovery capability. Names must contain 1–64 ASCII letters, digits, underscores,
or hyphens. This also tightens validation of existing `retrieve_tool_name` configurations: names
containing dots, colons, or spaces must be renamed before upgrading.
`rtk_enabled` controls whether the SDK resolves packaged RTK for PreTool. Compression thresholds,
content detection, TOON selection, diagnostics, authorization, and Stash policy are Core behavior
and are not Python configuration.

### Direct Runtime examples

Use `TokenlessRuntime` when the caller does not need `TokenlessSdk` to coordinate an Agent lifecycle
and wants to invoke Tokenless operations directly. Create one Runtime for the data directory:

```python
import json
import re
from anolisa_tokenless import TokenlessRuntime

runtime = TokenlessRuntime("/absolute/path/to/tokenless-data")
```

#### Compress a response

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

#### Compress a tool schema

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

#### Encode JSON as TOON

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

`compress_toon()` keeps the original JSON when TOON would not reduce the estimated token count.

#### Retrieve stashed content

Low-level response or schema compression uses shell recovery instructions by default:

```python
hash_match = re.search(r"If needed, run in shell: tokenless retrieve ([0-9A-Fa-f]{24})(?![\w-])", response_result.output)
if hash_match is not None:
    recovered_content = runtime.retrieve(hash_match.group(1))
    print(recovered_content)
```

`retrieve()` accepts a 24-character hash or a historical marker. Direct Runtime callers
must decide which markers are authorized for retrieval; `TokenlessSdk.retrieve()` sends the current
BeforeModel marker set to Core for authorization.

Runtime inputs and outputs are strings. Use each `CompressionResult.output` as the exact downstream
value, then inspect its `disposition`, token counts, and Stash fields when the caller needs to
understand whether and how the input changed.

### Query statistics

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

Use `stats.diff(session_id="...")` for a session overview,
`stats.diff(session_id="...", tool_use_id="...")` for one tool lifecycle, and
`stats.compare("baseline-session", "tokenless-session")` for a dry-run versus active comparison.

Token counts are estimates, and only operations with positive savings are recorded. `list()`,
`summary()`, and `compare()` do not return stored content; `show()` and detailed `diff()` results may
contain sensitive tool input or output. The public query API does not clear data or change settings,
but opening it may create or migrate `stats.db`, so the selected data directory must be writable.

## Layer 2: AgentScope integration

`anolisa-tokenless-agentscope` maps the framework-neutral lifecycle to AgentScope. Application code
uses `TokenlessAgentScope` instead of calling `before_model()`, `pre_tool()`, `post_tool()`, and
`retrieve()` itself. The integration also carries AgentScope session and
tool-call attribution into the generic SDK.

See [AgentScope SDK integration](sdk/agentscope.md) for supported versions, build and installation,
complete 1.x/2.x/App examples, configuration, retrieval boundaries, and validation. Product adapters
such as Claude Code and OpenCode are separate from both Python SDK layers.

## Validate both SDK layers

Build the framework-neutral wheel and run its installed-wheel tests:

```bash
make python-wheel
make test-python-runtime
```

Validate the AgentScope layer with the commands in its
[child document](sdk/agentscope.md#validate-the-integration).

## Related documents

- [Agent integration](framework-integration.md)
- [AgentScope SDK integration](sdk/agentscope.md)
- [CLI reference](cli-reference.md)
- [Measuring savings](measuring-savings.md)
- [Configuration and data privacy](configuration-and-privacy.md)
- [Runtime design](../../../../../src/tokenless/docs/design/runtime-library.md)

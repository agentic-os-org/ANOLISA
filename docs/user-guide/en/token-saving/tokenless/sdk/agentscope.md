# Tokenless AgentScope SDK Integration

[中文版](../../../../zh/token-saving/tokenless/sdk/agentscope.md)

`anolisa-tokenless-agentscope` is the AgentScope-specific layer above the framework-neutral
`anolisa-tokenless` SDK. It maps AgentScope lifecycle APIs to `TokenlessSdk` and depends on the
exact same package version; it does not implement compression separately.

Read the [Python SDK overview](../sdk.md) first when choosing between the generic SDK and this
AgentScope layer. Product adapters such as Claude Code and OpenCode are documented separately in
[Agent integration](../framework-integration.md).

## Supported AgentScope versions

| AgentScope version | Supported entry point |
|--------------------|-----------------------|
| 1.0.11 through 1.0.x | Tokenless Toolkit plus `install(..., session_id=...)` |
| 2.0.0 through 2.0.2 | Direct Agent construction with `integration.tools` and `integration.middlewares` |
| 2.0.3 through 2.0.x | Direct Agent construction or App through `integration.app_options()` |

## Install

The AgentScope integration wheel requires the exact same version of the native Runtime wheel.
Install both assets from the same Tokenless GitHub Release in one command. For example, install
[v0.8.4](https://github.com/alibaba/anolisa/releases/tag/tokenless/v0.8.4) on Linux x86_64:

```bash
python3 -m venv .venv
. .venv/bin/activate
python -m pip install \
  "https://github.com/alibaba/anolisa/releases/download/tokenless/v0.8.4/anolisa_tokenless-0.8.4-cp311-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64.whl" \
  "https://github.com/alibaba/anolisa/releases/download/tokenless/v0.8.4/anolisa_tokenless_agentscope-0.8.4-py3-none-any.whl"
```

For Linux aarch64 or macOS Apple silicon, replace the native Runtime URL with the matching asset
listed in the [SDK overview](../sdk.md); keep both package versions identical.

### Build from source

Alternatively, build and install both same-version wheels from a source checkout:

```bash
make python-wheel agentscope-wheel
python -m pip install \
  target/wheels/anolisa_tokenless-*.whl \
  target/wheels/anolisa_tokenless_agentscope-*.whl
```

## AgentScope 1.x

AgentScope 1.x uses a Tokenless Toolkit. Its regular and MCP registration paths also cover tools
added after construction:

```python
from agentscope.agent import ReActAgent
from anolisa_tokenless import ContentOrigin
from tokenless_agentscope import TokenlessAgentScope, TokenlessConfig, ToolContract

integration = TokenlessAgentScope(
    TokenlessConfig(
        data_dir="/absolute/path/to/tenant-tokenless-data",
    ),
    tool_contracts={
        "application_tool": ToolContract(ContentOrigin.API_RESPONSE),
    },
)
toolkit = integration.create_toolkit()
toolkit.register_tool_function(application_tool)
agent = ReActAgent(..., toolkit=toolkit)
integration.install(agent, session_id="conversation-id")
```

## AgentScope 2.x

Pass the retrieval Tool and middleware when constructing the Toolkit and Agent. This works from
2.0.0 and does not depend on mutable Toolkit APIs introduced in later patch releases:

```python
from agentscope.agent import Agent
from agentscope.tool import Toolkit
from anolisa_tokenless import ContentOrigin
from tokenless_agentscope import TokenlessAgentScope, TokenlessConfig, ToolContract

integration = TokenlessAgentScope(
    TokenlessConfig(
        data_dir="/absolute/path/to/tenant-tokenless-data",
        # retrieve_tool_name="tenant_tokenless_retrieve",
    ),
    tool_contracts={
        "application_tool": ToolContract(ContentOrigin.API_RESPONSE),
    },
)
toolkit = Toolkit(tools=[*application_tools, *integration.tools])

agent = Agent(
    ...,
    toolkit=toolkit,
    middlewares=integration.middlewares,
)
```

The existing `TokenlessMiddleware` 2.x API remains available for compatibility. New code should
use `TokenlessAgentScope` to avoid patch-specific Toolkit mutation and automatic Tool collection
behavior.

## AgentScope App

AgentScope App is supported from 2.0.3. `app_options()` derives an isolated Tokenless data directory
for every user/agent/session below the configured absolute base directory:

```python
from agentscope.app import create_app
from tokenless_agentscope import TokenlessAgentScope, TokenlessConfig

integration = TokenlessAgentScope(
    TokenlessConfig(data_dir="/srv/tokenless-tenants"),
)
app = create_app(..., **integration.app_options())
```

`app_options()` supplies one Middleware factory. AgentScope publishes that Middleware instance's
static Retrieve Tool through `list_tools()` and persists Marker authorization in
`AgentState.middle_context`.

AgentScope 2.0.0 through 2.0.2 support direct Agent construction only; their App APIs do not
provide both Middleware-owned Tool publication and persisted Middleware state.

## Configuration and behavior

Set a unique `retrieve_tool_name` in `TokenlessConfig` if the application already defines
`tokenless_retrieve`; App assembly does not expose other tools to its factory for a preflight
collision check.

The integration includes explicit contracts for known AgentScope shell, file, and API tools. Pass a
`tool_contracts` mapping for every custom tool. `ToolContract` requires one `ContentOrigin`:
`COMMAND_OUTPUT`, `FILE_CONTENT`, or `API_RESPONSE`. Set `command_field` only on a
`COMMAND_OUTPUT` contract whose arguments may be rewritten by RTK. Unknown custom tools fail at
registration in AgentScope 1.x and at the model boundary in AgentScope 2.x; output text is never
used to guess origin.

`TokenlessConfig` contains only `data_dir`, `retrieve_tool_name`, and `rtk_enabled`. Compression
thresholds, content detection, TOON selection, error diagnosis, marker authorization, and Stash
policy are owned by Rust Core.

The integration passes intermediate streaming chunks through unchanged, preserves framework
objects, and transforms only copied call arguments and final model-visible text. Tokenless keeps
the original whenever an optimization fails or does not make the UTF-8 result strictly smaller.
`DataBlock` values are never changed.

The integration exposes a retrieval Tool named `tokenless_retrieve` by default. Its declaration is
static and remains in the model tool list across calls, so Marker visibility does not churn the
tool list. It accepts only a complete marker or exact 24-character hexadecimal hash retained for
the current model call. Retrieve output bypasses PostTool.

Pass a separate absolute `data_dir` for every user or tenant. `TOKENLESS_DATA_DIR` is only a
process-wide fallback and must not be shared by multiple tenants. Retrieval does not work across
nodes. Stash entries expire after the current fixed one-hour TTL.

Both AgentScope version paths enable schema compression, RTK command rewriting, response
compression, TOON, retrieval, environment-error guidance, and per-call attribution. The platform
wheel contains RTK and links TOON directly, so it does not search for system helper binaries. Tool
Ready remains hard-disabled.

## Validate the integration

Run the installed-wheel and supported-version matrix tests from a source checkout:

```bash
make test-agentscope-integration
```

Then exercise one successful, compressible tool response in the application. Confirm that the
middleware returns the smaller result and that `tokenless_retrieve` can recover marker-scoped
content from the same `data_dir`.

## Related documents

- [Python SDK overview](../sdk.md)
- [Agent integration](../framework-integration.md)
- [Configuration and data privacy](../configuration-and-privacy.md)
- [Troubleshooting](../troubleshooting.md)

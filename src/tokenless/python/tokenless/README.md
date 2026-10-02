# anolisa-tokenless

Self-contained CPython SDK for the BeforeModel, PreTool, PostTool, and marker-authorized Retrieve
lifecycle operations.

The package is built from the ANOLISA monorepo and supports CPython 3.11 or later on the platform
targeted by its wheel. The pinned RTK executable is included in the wheel; no Tokenless binary is
required on `PATH`. See the
[Tokenless Python SDK guide](https://github.com/alibaba/anolisa/blob/main/docs/user-guide/en/token-saving/tokenless/sdk.md)
for source-build prerequisites, lifecycle contracts, configuration, and runnable examples. This
package is the framework-neutral SDK layer. The same-version `anolisa-tokenless-agentscope` package
builds the AgentScope-specific layer on top; its detailed attachment steps are in the
[AgentScope SDK integration guide](https://github.com/alibaba/anolisa/blob/main/docs/user-guide/en/token-saving/tokenless/sdk/agentscope.md).
The [Tokenless component README](https://github.com/alibaba/anolisa/blob/main/src/tokenless/README.md)
provides the CLI, adapter, and source-build overview.

```python
import asyncio
import json

from anolisa_tokenless import (
    Attribution,
    ContentOrigin,
    OutputOptimization,
    PostToolCapabilities,
    PostToolRequest,
    RecoveryMethod,
    ResultKind,
    TokenlessConfig,
    TokenlessSdk,
    ToolResultStatus,
)


async def main() -> None:
    sdk = TokenlessSdk(
        TokenlessConfig(
            data_dir="/absolute/path/to/tokenless-data",
            rtk_enabled=False,
        )
    )
    original = json.dumps({"items": list(range(300))})
    result = await sdk.post_tool(
        PostToolRequest(
            result_kind=ResultKind.TOOL,
            tool_name="api",
            content=original,
            status=ToolResultStatus.SUCCESS,
            content_origin=ContentOrigin.API_RESPONSE,
            output_optimization=OutputOptimization.NONE,
            capabilities=PostToolCapabilities(True, RecoveryMethod(), True),
            attribution=Attribution("my-agent", "session-42", "tool-7"),
        )
    )
    print(result.output)


asyncio.run(main())
```

Declare `RecoveryMethod.shell()` only when the Agent can run `tokenless retrieve HASH` through
its existing shell tool. For a registered static Tool, use `RecoveryMethod.tool(actual_name)`;
the output then instructs the model to call that Tool with `hash_or_marker=HASH`. The default
`RecoveryMethod()` permits only candidates that need no retrieval. Schema recovery requires
the static Tool method. Historical angle-bracket markers remain readable, but new output uses
optional `If needed` instructions.

The public `TokenlessStats` client provides typed, read-only status, summary, recent-record,
record-detail, structured-diff, and session-comparison queries over the Runtime's `stats.db`.
Token counts are estimates and only operations with positive savings are recorded. Record details
and detailed diffs can contain sensitive stored tool content. Read-only describes the API surface:
opening the client follows CLI initialization and may create or migrate `stats.db`, so the data
directory must be writable. `limit=None` for summary or comparison reads at most the newest 10,000
records. Session and tool-use diffs also read at most the newest 10,000 matching records;
comparisons should pass a dry-run session before an active Tokenless session.

# AgentScope SDK

[中文版](../../../zh/aw/agents/agentscope.md)

Run source examples from `src/aw`; build AW with the [integration tutorial](../integration.md) first.

## Start AgentScope SDK

The verified standalone baseline is AgentScope 2.0.6; QwenPaw's verified runtime
uses AgentScope 2.0.10. Other releases can use the same public factory and middleware
contract. `argv` selects the SDK environment's Python and a `.py` application or
`-m MODULE`. AW records the installed version, supplies `aw_agentscope` and a
protected runtime configuration, and awaits a receipt after the real public
factory has installed middleware.

From `src/aw`, with that environment's `python3` on PATH:

```bash
target/debug/aw run --config crates/aw-service/examples/aw.agentscope.yaml --agent agentscope
target/debug/aw status --config crates/aw-service/examples/aw.agentscope.yaml
target/debug/aw stop --config crates/aw-service/examples/aw.agentscope.yaml
```

The example uses a deterministic offline model and a local Echo tool. It needs no
credentials. Its before Boolean Hook returns false (no block match) and after
returns true (observe). Change the before command to `printf true` to deny the
tool; the native final response becomes denied and after observes that denial.
For a resident worker, apply policy changes between replies with `aw update`.

For an existing application, use `AwBridge().create_agent(...)` in **every** public
factory, passing the existing middleware list unchanged. Existing trusted
argument injection must precede the appended AW middleware. A readiness receipt
covers the constructed Agent, not other factories or direct dispatch routes.
The [factory contract](../../../../../src/aw/docs/design/en/adapters/agentscope-adapter.md) details
ordering, budgets, invocation identity and coverage.

Before waits and can block before a tool side effect. After awaits one final
`ToolResponse`, preserving success, error or interruption without repeating
stream chunks. Native parameter/permission rejections before `on_acting`, direct
Toolkit calls and external execution requests need separate integration. The
launcher owns one foreground worker per `aw run`; multiple workers use separate
launches on the same daemon. Forking Gunicorn supervisors and automatic child
attachment remain outside this factory contract. Tool invocation records are retired after each call; each factory reply releases its Scope at turn completion.

For PreTool retry suggestions and final PostTool compression, copy the
[SDK lifecycle example](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.agentscope-lifecycle.yaml),
replace all paths and map your Toolkit's exact tool names/argument pointers in
sec-core. It runs security checks before Tokenless PreTool, observes the original
completed response, then compresses one successful text slot. Keep custom/native
after Hooks before the final compression step; with replacement enabled AW runs
this chain in declared order. Native and observation Hooks cannot replace the
response; only the final admitted structured replacement step can change its
model-visible text. Reported Hook failures retain the original input for subsequent
steps. Observe-only chains keep native reverse unwind order.

PreTool denies the original call and returns full retry arguments to the model;
each new attempt passes native permissions and sec-core again. An attributed RTK
retry avoids double compression even with a new call ID. See [Tokenless behavior](../../../../user-guide/en/aw/tokenless.md)
for text-slot eligibility, failure handling and telemetry configuration. Invalid
final-response replacement directives report failure and retain the original result.

## Start from the application

A Python application can prepare its own AW instance before importing `AwBridge`
and continue to use its normal startup command. The existing service must use
this application's `aw.yaml`, with an `agentscope` target selecting the same
Python environment and entrypoint. Add this preparation once and restart the
application:

```python
import json
import os
import subprocess
import sys

instance = json.loads(subprocess.check_output(
    ["aw", "instance", "prepare", "--agent", "agentscope"],
    text=True, timeout=30,
))
os.environ.update(instance["environment"])
sys.path[:0] = os.environ["PYTHONPATH"].split(os.pathsep)
from aw_agentscope import AwBridge
```

Construct every protected Agent with `AwBridge().create_agent(...)`, as above.
Readiness follows actual Agent construction; preparation alone does not mark the
worker ready. Preparation or factory failures must stop protected dispatch.

From the directory containing `aw.yaml`, start the application normally. Use
another terminal to inspect it and apply changes from the same configuration:

```bash
python /absolute/application/worker.py
aw status --config aw.yaml
aw instance update --agent agentscope
```

Pause new replies and fully drain active replies before updating. The next reply
adopts the updated input and tool routes through the existing resident factory.
For multiple workers, select one with `aw instance update --instance INSTANCE`.
After all reply streams and execution leases close, release the instance in the
application's `finally` block:

```python
subprocess.run(
    ["aw", "instance", "release", "--binding", instance["binding"]],
    check=True, timeout=10,
)
```

Each OS worker prepares its own instance after fork and constructs its own bridge.
A bridge or prepared binding inherited from a parent process cannot admit work.

## Check user input before a reply

Enable `input.submit` in the [lifecycle configuration](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.agentscope-lifecycle.yaml)
to run sec-core `scan_prompt` and `scan_user_input` before memory insertion or
model dispatch. The factory uses AgentScope 2.0.6's native `on_reply` middleware
and a public `Agent` subclass for reply lifetime. Existing application middleware
keeps its order; AW checks the effective input passed by that middleware.

| Input to `reply` / `reply_stream` | Input check |
| --- | --- |
| Native `Msg` with role `user` | All `TextBlock.text` values, joined with newlines |
| `list[Msg]` | All user text blocks, in message order |
| System and assistant messages (including tool results); existing state/memory | Excluded from new user text |
| `None`, native confirmation, interruption and external-result events | Native continuation; no new submission |
| Strings, arbitrary objects, user media or non-text blocks | Rejected before execution with `ValueError` |

Keep history in the Agent state; a user message explicitly passed again counts
as a new submission. Direct private `_reply` calls and nested sub-Agent dispatch
are unsupported when checks are enabled. Concurrent replies on the same Agent
raise `CallbackFailure`; distinct Agent instances may run concurrently in one
foreground worker with isolated turn scopes. With no input route, the factory
retains native input semantics; each reply still uses its own Scope.

A policy denial raises `aw_submission.SubmissionDenied`; it makes no model
request and inserts no input into memory. Provider errors obey `on_error: block`
or `report`. Unsupported input shapes always fail before execution. After cleanup,
the same Agent can accept another input. Each public reply call creates one AW
turn ID and Scope, including pure model replies. All input steps share that turn
and budget; tool calls use its ledger. Normal return, denial, exceptions and
cancellation release it. An unconfirmed close blocks further scope allocation
until the bounded cleanup retry succeeds.

Consume `reply_stream` fully or explicitly close it in the consuming task when
stopping early. Wrap it with `contextlib.aclosing`:

```python
from contextlib import aclosing
from agentscope.message import UserMsg
from aw_submission import SubmissionDenied

try:
    async with aclosing(agent.reply_stream(UserMsg("user", "hello"))) as stream:
        async for event in stream:
            handle(event)
except SubmissionDenied as error:
    show_rejection(str(error))
```

A parked native reply and its later continuation are separate AW turns. Completed
`tool.after` routes with `on_error: withhold_result` suppress intermediate tool
chunks and publish the checked final content after all checks and invocation
cleanup. Both `reply_stream` consumers and the model receive the adopted text or
the withheld placeholder. Targets with no strict after route retain native intermediate chunks.

## Task-private tool credentials

Pass `execution_factory` to `AwBridge.create_agent` to acquire one application
lease per reply, including replies without `input.submit`. Use the shared
`aw_private.PrivateExecution` contract. Each tool must explicitly pass
`execution_environment()` when creating its subprocess or client.

```python
import asyncio
from contextlib import asynccontextmanager
from aw_private import PrivateExecution, execution_environment

@asynccontextmanager
async def execution_factory():
    credentials = await credential_service.acquire()
    try:
        yield PrivateExecution(
            environment=credentials.environment,
            hook_environment=credentials.public_environment,
            secrets=credentials.secrets,
            redact=credentials.redact,
        )
    finally:
        await credentials.revoke()

agent = bridge.create_agent(
    name="worker", model=model, toolkit=toolkit,
    execution_factory=execution_factory,
)
# Inside an application tool:
process = await asyncio.create_subprocess_exec(command, env=execution_environment())
```

`credential_service`, `model`, `toolkit` and `command` are application-owned.
The synchronous redactor must cover complete result content and metadata and be
idempotent. AW withholds tool chunks, then redacts the completed `ToolResponse`
before public Hooks and model delivery. Stream consumers receive one final chunk
after all Hooks and invocation retirement complete. Native arguments and the worker's
global environment remain unchanged. Hooks receive only `hook_environment`.

AW reserves the reply before acquisition and releases it after native tool drain,
child Scope closure and confirmed credential revocation. Acquisition and revocation
each have a 60-second limit. A failure or timeout in revocation retains occupancy
and rejects subsequent replies until worker restart; late completion remains owned
and is never dispatched as a new task. See the [lease contract](../../../../../src/aw/docs/design/en/adapters/private-execution.md#agentscope-reply-factory).

## Update a resident worker

Ordinary `aw run` supports policy updates after all `reply` / `reply_stream` calls
and their cleanup complete. Keep the Agent and its state; the next reply obtains
an immutable policy snapshot before checking input or dispatching tools. Existing
application middleware retains its ordering. Adding or removing input routes and
tool routes takes effect together.

```bash
aw update --config current.yaml --agent agentscope --policy next.yaml
aw status --config current.yaml
```

Pause dispatch before updating. Model execution, active tool tasks and unconfirmed
cleanup keep the instance busy. Explicitly close partially consumed reply streams.
A lost scope-open response fences later replies until the worker is restarted; a
confirmed child close with an unconfirmed reservation release is retried before
the next reply. Each acknowledged reply scope owns bounded callback observation
and drains it before close. AgentScope source identity remains unverified, so
coverage stays unknown.
See [policy updates](../deployment/policy-updates.md) for eligible configuration
fields and revision selection.

## Run multiple OS workers

Start each worker with its own `aw run`, using the same configuration and runtime
user. Each worker constructs its Agent through `AwBridge.create_agent()` and has
its own instance identity, readiness receipt and launch directory.

Run this command in each worker's terminal:

```bash
aw run --config current.yaml --agent agentscope
```

List the ready instances and update one selected worker after pausing its task
dispatch. Replace `INSTANCE` with that worker's `instance_id` from status:

```bash
aw status --config current.yaml
aw update --config current.yaml --instance INSTANCE --policy next.yaml
```

Stopping one launcher terminates and drains its worker; other workers retain their
identity, policy and sessions. Stop the shared daemon after every worker exits.
Business-owned prefork/Gunicorn workers need independent registration and factory
adoption; inheriting one `AwBridge` across OS processes is unsupported.

## Skill trust

Enable `check_skill` for `native:agentscope:Skill` through the
[shared security configuration](../../../../user-guide/en/aw/security.md#check-skills-before-loading).
AW resolves the actual Toolkit selection, retains its native duplicate precedence,
and pins the checked object until the invocation finishes. AgentScope 2.0.6
skips BOM manifests. Direct file reads and automatic context injection require
separate policy boundaries.

# Policy CLI User Guide

[中文版](../../zh/agent-sec-core/policy-cli.md)

Use `agent-sec-cli` to manage revisioned Policy Templates and immutable Scope
assignments through `asc-daemon`. Each Scope saves the selected Policy revision's
complete content. The daemon discovers matching process instances, creates their
Bindings, and reconciles deployment automatically. Binding commands are read-only.

These commands are available in the V2 CLI; the released Python CLI does not yet
include them. Policy, Scope and Binding state persists in SQLite across daemon restarts. Scope acceptance does not
mean protection has taken effect. Upgrade CLI and daemon together: Scope update,
Scope revision arguments and manual Binding mutations have been removed.

## Connect to the daemon

Use the absolute socket path supplied by your deployment administrator. The examples
below assume `SOCKET` contains that path and your user is authorized to administer Policies.
An unauthorized caller receives `permission_denied`. The CLI does not start the daemon
or grant permissions.

```bash
agent-sec-cli --socket "$SOCKET" policy list
```

## Policy database location

With no nonempty `AGENT_SEC_DATA_DIR`, the daemon checks two Policy database paths:
A is `/var/log/agent-sec/policy-state.db`; B is
`/var/lib/agent-sec/policy/policy-state.db`. It selects A when A's directory is
safe, otherwise B when A's ancestors are unsafe and B's directory is safe. This handles group-writable log
ancestors such as Ubuntu's `/var/log` without changing their permissions.
Only Policy storage can fall back; security-event and observability paths stay unchanged.

Before creating anything, the daemon checks both locations for existing databases:

- If both contain a database, startup fails rather than choosing or merging them.
- If the directory selection would bypass a database at the other location, startup
  fails. This includes A becoming safe again after B has been used.
- Otherwise, the daemon opens the selected existing database or initializes it.
  Corruption, incompatible schema, lock contention and I/O errors never select another database.

After validating ancestors, an existing leaf directory owned by the daemon UID
is restricted to `0700` before selection. For example, `/var/log/agent-sec` at
`0755` is repaired and stays at A; `/var/log` at `0775` permits fallback to B.
Ancestors are never chmodded. A foreign-owned leaf or a failed permission repair
is a startup error, not a reason to select B.

Directory diagnostics identify the failing path, actual UID/GID/mode and required
permissions. Conflicts identify both database paths and require restoring the
previous directory configuration or an explicit offline migration. Existing data
is never automatically moved, deleted or rebuilt. Unreadable paths and orphaned
SQLite sidecars are errors, not proof that the database is absent.

A nonempty `AGENT_SEC_DATA_DIR` in the daemon's environment selects a fixed data
directory, without default A/B discovery or fallback. Keep it private (`0700`,
owned by the daemon UID); database and auxiliary files must be `0600`. Ancestors
must be owned by root or the daemon UID and must not be group/other writable,
except for root-owned sticky directories. No path component may be a symlink.

## Correlate local logs

V2 uses native OpenTelemetry for local request correlation. `--trace-context`
retains the existing flat Agent metadata input; place it before command names and
other options' non-option values. Optional `--otel-context` accepts a version 1
JSON carrier with `traceparent`, `tracestate` and `baggage`. The explicit flat
Agent fields take precedence when both are supplied.

```bash
RUST_LOG=info agent-sec-cli --trace-context '{"session_id":"session-123","agent_name":"openclaw"}' \
  --otel-context '{"version":1,"traceparent":"00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"}' \
  --socket "$SOCKET" policy list
```

`RUST_LOG=info` enables bounded JSON correlation diagnostics on stderr; configure
the daemon environment separately to see its records. Default warn leaves these
records disabled. Diagnostics may be dropped under back-pressure; command results
and errors retain their existing output and exit-code semantics.
No public OTLP exporter or exporter/sampling/batch configuration is provided.
`OTEL_*` settings cannot enable export or change the fixed local sampling policy.
`--otel-context` is incoming context, not an export destination.
The CLI requires the new daemon with carrier support; upgrade the daemon first.

## Intent: propose a candidate from a task

`agent-sec-cli intent propose` analyzes task intent and outputs an unapproved
composable Cedar policy candidate for human review of exact paths, directory subtrees,
actions and conditions. It only outputs a candidate: it does not persist policy-management
records, bind an execution process, or deploy enforcement. The Policy commands
below use the existing JSON template format and cannot directly import this Cedar
candidate.

The LLM receives only the task text and the unified `seccore.cedarschema`,
and directly returns `{"policy":"...","unresolved":[]}`. Rust parses the
original Cedar, checks absolute path expressions and runs official Strict
validation; it appends no rules. The `asc.intent.draft.v1` draft preserves the
original response and task provenance. Neither validation nor an empty
`unresolved` proves complete or correct interpretation; unresolved requirements
are model-reported.
CLI rebuilds the expected artifact from the draft with its embedded schema,
in addition to checking versions, provenance, TTL and hashes. Upgrade CLI and daemon
together. The generation instructions in
[`policy-generation.md`](../../../../src/agent-sec-core/v2/crates/asc-intent/prompts/policy-generation.md)
are embedded at build time and tagged `cedar-policy@1`; human review remains required.

An administrator must set `ASC_INTENT_ENABLED=true` and `DASHSCOPE_API_KEY` in the
daemon startup environment. Configuration changes take effect after a restart.
`deepseek-v4.1-flash` remains the default because it responds quickly;
explicitly setting `ASC_INTENT_MODEL` is optional. Actual latency varies with model,
task and service. Intent is not tied to DeepSeek: set `ASC_INTENT_MODEL` to
another available model, such as `qwen-plus`, without changing the response protocol
or Cedar checks. The default API base is `https://dashscope.aliyuncs.com/compatible-mode/v1`.
Set `ASC_INTENT_BASE_URL` to use another OpenAI-compatible HTTPS service; despite its
historical name, `DASHSCOPE_API_KEY` must then contain that service's API key.
The service must accept the JSON chat-completions request parameters and return a
complete policy/unresolved response within the output and deadline limits.

`ASC_INTENT_THINKING` accepts `disabled` (default, sends the DashScope extension
`enable_thinking: false`) or `provider` (omits that extension). Use `provider` for
services that do not support the extension. For example, to select another model
in the daemon startup environment:

```bash
export ASC_INTENT_MODEL=qwen-plus
export ASC_INTENT_THINKING=provider
```

Invalid configuration or model responses make intent unavailable without preventing
other daemon features. Automatic model selection and local-model fallback are not
implemented; a local small model is planned to reduce cloud costs.

This feature sends only the task text to a cloud model and may incur charges.
No file contents, directory listings or Git changes are collected, and the daemon
does not open caller paths. Absolute paths and other sensitive text inside the task
are sent; review the task first.

```bash
agent-sec-cli intent propose \
  --task "Allow reading and writing the src directory tree; keep config/important.txt unchanged. Do not execute programs or use the network."
```

Only `--task` is required: it uses the default daemon endpoint and text output.
The model must express task targets as canonical absolute paths; they may refer to
files not yet created. Relative or ambiguous targets belong in `unresolved`, and
users cannot supply extra path or protection rules. `--socket` or
`AGENT_SEC_DAEMON_SOCKET` overrides the endpoint; a failed connection never falls
back to a local model.

### Policy and safety boundaries

- Use only the unified schema: `file.read`, `file.write`, `file.exec` and
  `network.use`. Modification requests carry `WriteOperation` values
  `write/create/delete/rename/setattr`; a delete prohibition is not immutability.
  `file.read` means content reading, not listing names or reading metadata.
- Model output is the whole grant: no permit or forbid is appended and no model
  rule is overridden. When a task says "do not delete a.txt", the model writes that
  forbid itself instead of the user supplying a protection list.
- Paths use `resource.path` and `context.input.program/cwd`, not File UIDs.
  The frontend accepts canonical absolute literals with `==`/`!=`, or `like`
  patterns whose only wildcard is the trailing directory `/*`. Dynamic and
  noncanonical path expressions, unbound templates, model-supplied Subject/File
  identities and annotations other than optional `@id` are rejected. Path
  attributes must be read directly from those receivers; a record projection such
  as `{r: resource}.r.path` is rejected, and an action that cannot apply to the
  resource type fails validation, matching policy admission.
- The prompt requires necessary identities, conditions, approvals and obligations
  that cannot be expressed to remain in `unresolved`, without unconditional permits
  replacing them. Tool invocation must not be substituted with program execution;
  a bare program name must not be guessed through PATH. This is a model instruction,
  not a deterministic completeness or conflict check.
- Conditions can use schema fields such as `program`, `cwd`, indexed `args` with
  `arg_count`, and optional `mode/ownerUid/ownerGid`. Optional fields require `has`
  guards. Argument index 0 represents `argv[1]`, excluding `argv[0]`.
- Cedar defaults to Deny without a matching permit; forbid overrides permit.
  Ordinary permits do not restrict other policies. An exclusive requirement must
  be expressed with native complement forbids, including its conditions; a write
  complement must remain specific to the requested operation.
- Pure prohibitions must not add permits. No sensitive-path blacklist, execution
  ban or network ban is added implicitly. Inspect the actual Cedar against the task;
  neither Strict nor model-reported `unresolved` proves that every requirement is met.

### Inspect the result

Default `--format text` displays the original task, model Cedar draft, `unresolved`,
validated Cedar, safety notes and expiry. `--format cedar`
prints the candidate Cedar with unresolved requirements in comments; `--format json` prints
the complete result. Cedar control characters are escaped only while stdout is a terminal;
redirected or piped output keeps the model text unchanged. If the model policy is empty,
`artifact.text` is empty. No format prompts for input or requests approval.
Communication defaults to 45 seconds; the model has at most 30 seconds, unaffected
by increasing `--timeout-ms`. `--ttl` accepts `1..=1800` seconds, default `1800`,
starting when the daemon receives the request, without automatic renewal.

The model JSON response is limited to 16 KiB; `unresolved` has at most 128 nonempty
items, each at most 4096 bytes. Final Cedar has at most 1024 policies and 256 KiB
before Strict validation. The complete result is limited to 512 KiB.
Exceeding any budget fails the whole request; output is never truncated.

Any `unresolved` entry is printed with the candidate; nothing is left to confirm.
A task the model or the schema cannot express is a normal result rather than a
failure: the CLI prints the model draft with every `reasons` entry, emits no Cedar
and exits `0`. Ctrl-C exits `130`. No run installs, records or requests an approval.

Requests use `asc.intent.request.v1`; candidates use `asc.intent.candidate.v1`
with `status: "CANDIDATE"` and `authorization_required: true`. The candidate retains
`draft` (original `policy`, `unresolved` and task provenance), TTL, hashes and `notes`.
`artifact` contains `language`, `text` and required `schema_sha256`. A version mismatch
fails instead of adapting; regenerate existing candidates with a matching CLI and daemon.

`candidate_hash` covers the complete candidate except the hash itself; it is not
a signature or approval credential. CLI also reconstructs the final artifact from
the draft, detecting replaced model output even if a hash is recomputed. Official
Strict proves schema conformance, not task completeness,
trusted identity or interception of actual file operations.

| Intent result | Exit status |
|---------------|-------------|
| `candidate`, with or without `unresolved` | `0` |
| `needs_clarification` (draft and reasons, no candidate) | `0` |
| Input, transport, configuration, model or Cedar validation error | `1` |
| Candidate had already expired when it was received | `2` |
| Interrupted (Ctrl-C) | `130` |

The exit status reports whether the command ran, not whether the task could be
expressed; read `outcome` and `reasons` for that.

This Cedar schema has only `file.read`, `file.write`, `file.exec` and `network.use`,
not TTL or process identity. Trusted approval, specific process binding and expiry
revocation remain downstream work. Enforcement must also normalize paths, handle
symlinks/hardlinks and namespaces, authorize both rename source and destination,
and ensure complete argument indices consistent with `arg_count` and valid
attribute values.
This command does not establish these conditions and must not be described as
blocking `rm`.

## Manage Policies

Create a JSON template file such as `policy.json`:

```json
{
  "specVersion": "0.1",
  "rules": [
    {
      "effect": "block",
      "category": "file",
      "action": "write",
      "target": {"type": "file", "path": "/workspace/important/**"},
      "where": {"operation": {"eq": "delete"}},
      "because": "Protect important files from deletion"
    }
  ]
}
```

A PolicyTemplate is a reusable policy that multiple Scopes can select by ID/revision.
Its `rules` describe actions, typed targets, decisions, conditions and optional reasons.
The current resource format is `{"type":"file","path":"..."}`; use one rule per path.
File read/write/exec rules, logical conditions, history predicates, and block/allow/
require_confirmation decisions can be saved when valid. Network and AgentHook target
formats are not yet defined and cannot be saved.

For native JSON, the PEP Translator supports only block + file/write + operation=delete
rules without history. Unsupported rules fail the entire Binding with a bounded code
such as `RULE_1_UNSUPPORTED_EFFECT`; no supported subset is sent. Quotes, backslashes
and control characters in `because` are rejected during translation because the target
DSL has no escape syntax; ordinary Unicode text is preserved. An omitted reason uses
the Translator's default. Policy creation success does not establish that the target
PEP (Policy Enforcement Point) supports these rules.

The generated DSL expresses deletion prevention. Actual delete-only kernel behavior
requires separate validation; the current ActPlane backend shares its unlink/write
operation mapping. Old fixed-kind JSON and development databases containing it are
not migrated or automatically rebuilt; clean up old deployments with the compatible
binary before arranging a fresh database.

```bash
agent-sec-cli --socket "$SOCKET" policy create --name "protect files" --file policy.json
agent-sec-cli --socket "$SOCKET" policy get --policy-id "$POLICY_ID" --revision 1
agent-sec-cli --socket "$SOCKET" policy list --limit 100 --offset 0
agent-sec-cli --socket "$SOCKET" policy update --policy-id "$POLICY_ID" --name "protect files v2" --file policy-v2.json
agent-sec-cli --socket "$SOCKET" policy delete --policy-id "$POLICY_ID" --revision 2
```

Use the ID and revision returned by successful commands. These are reference
examples, not a sequential script. Create a Scope while the desired Policy revision
is current. Both create and update require a name and complete template; update is
not a partial patch. A changed name or template increments the revision; identical
content does not. Only the current complete record is retained. Policy get/delete
require its exact revision; an old revision returns `not_found`. File paths are
resolved relative to the CLI working directory.

Updating or deleting a Policy does not change existing Scopes or their Bindings.
Delete the Scope to withdraw its assignments. A known deleted Policy ID retains its
revision counter; updating it recreates content at the next revision. An unknown ID
cannot be updated.

## Cedar Policy source

The same `policy create` and `policy update` commands accept a JSON source
envelope in `--file`. Save this example as `cedar-policy.json`:

```json
{
  "language": "cedar",
  "content": {
    "schema": {"schemaId": "seccore.system", "revision": 1},
    "text": "forbid(principal, action == SecCore::Action::\"file.write\", resource) when { context.input.operation == SecCore::WriteOperation::\"delete\" && resource.path == \"/workspace/important\" };"
  }
}
```

```bash
agent-sec-cli --socket "$SOCKET" policy create --name "Cedar protection" --file cedar-policy.json
agent-sec-cli --socket "$SOCKET" policy get --policy-id "$POLICY_ID" --revision 1
agent-sec-cli --socket "$SOCKET" policy update --policy-id "$POLICY_ID" --name "Cedar protection v2" --file cedar-policy-v2.json
```

Get/list return the saved original text and exact schema reference. Invalid
syntax, schema or common-subset constructs return `invalid_argument` without
changing stored policy content or revisions. Unknown schema revisions are rejected
without fallback. Native templates remain supported in their existing shape.

Policy creation success does not establish target PEP support. The current PEP
Translator supports a Cedar `forbid` subset for deletion, ordinary-write
restrictions and exact-file read denial. Read denial uses `file.read` with an
exact file path and requires AgentSight's `file_read_protection` capability;
directory-tree read denial is unsupported. See [AgentSight's read-protection
boundaries](../agentsight/integrations.md#deny-secret-file-reads).
Unsupported semantics reject the entire translation. Existing
Scope/Binding snapshots retain their saved Policy revision and schema when a
Policy is updated or deleted. Source can be reloaded into an in-memory PST;
neither AST nor PST is stored in SQLite. An older binary cannot read new Cedar
records, so preserve a pre-upgrade database backup when planning rollback.
The known SDK stack-overflow limit for deeply nested source remains unresolved;
use administrator-reviewed policies rather than untrusted generated input.

Sandbox runtime authorization uses the same Policy objects with
`schema: {schemaId: "seccore.system", revision: 1}`. The OS authorization PDP evaluates
Agent subjects, `process.spawn` and supported permit advice. Select the baseline and
optional task Policy IDs in the daemon's authorization configuration, as shown in
the [sandbox guide](sandbox.md#use-policy-authorization). Each evaluation reads
their current revisions together. Successful updates affect subsequent requests;
missing or deleted policies stop new authorization. Existing Scope/Binding
snapshots and accepted decisions retain their own bound versions.

## Manage Scopes

Create an assignment with a Policy ID and exact revision, plus one selector:
`--process-name` for an exact Linux process name (1–15 bytes), `--executable` for an
exact normalized absolute executable path, `--cmdline-arg` for an exact normalized
absolute path that appears as one whole command-line argument, or `--pid` for one
process instance. Name, executable and command-line selectors discover existing and
future matching processes. PID selection pins the first observed instance and never
follows PID reuse. Cgroup assignments are currently rejected.

Use `--cmdline-arg` for script Agents that run under a shared interpreter such as
node or python, where the process name and executable identify only the interpreter.
Pass the command path users run, as printed by `which`. A process matches when any
argument in `/proc/PID/cmdline` equals the path byte for byte. Symbolic links are
not resolved, and substrings or prefixes do not match. For example, running
`qodercli` from a shell produces `node /usr/local/bin/qodercli ...`, which
`--cmdline-arg /usr/local/bin/qodercli` selects. Starting the script by another
path, such as its resolved `.js` file or a relative path, does not match. Other
processes of the same user that pass the path as an argument, such as an editor
opening it, are also selected. Command-line arguments are set by the process, so
this selector identifies processes within the owner UID; it does not authenticate
them.

**Breaking change:** `scope create --cgroup-id` has been removed. Cgroup assignments
were not supported. Scripts that pass this option now fail locally with exit code 2;
select the intended processes with `--pid`, `--process-name`, or `--executable` instead.

```bash
agent-sec-cli --socket "$SOCKET" scope create --process-name openclaw --policy-id "$POLICY_ID" --policy-revision 1
agent-sec-cli --socket "$SOCKET" scope create --cmdline-arg "$(which qodercli)" --policy-id "$POLICY_ID" --policy-revision 1
agent-sec-cli --socket "$SOCKET" scope get --scope-id "$SCOPE_ID"
agent-sec-cli --socket "$SOCKET" scope list --limit 100 --offset 0
agent-sec-cli --socket "$SOCKET" scope delete --scope-id "$SCOPE_ID"
agent-sec-cli --socket "$SOCKET" scope retry --scope-id "$SCOPE_ID"
```

Scope has no revision and cannot be updated. Its `policySnapshots` contain the
complete selected Policies. If a concurrent Policy update or deletion wins before
Scope admission, creation fails with `not_found`; it never selects another revision
silently. Once admitted, the saved snapshot also applies to future matching instances.
The CLI assigns one Policy; the RPC accepts 1–32 distinct Policies per Scope.

To replace an assignment, create a new Scope, inspect its Bindings until the required
instances are `READY`, then delete the old Scope. This is not an atomic switch.

Deletion closes discovery admission, stops its worker and asynchronously removes
owned deployments. The response is `{"scopeId":"...","completed":false}` while
cleanup remains, or `completed:true` after removal. Until then the Scope remains
`DELETING`, with failed Bindings visible. Repeating delete preserves retry budgets.
Use `scope retry` to retry terminal failures after resolving their cause; it does not
change the assignment or restart work that is already running. Completed deletion and deletion of unknown Scope IDs both return `completed:true`,
including after daemon restart. Other Scopes and the source Policy are unaffected.

## Inspect Bindings

The daemon creates one Binding per Scope, Policy and process instance. Each record
contains its Policy snapshot, Scope ID and selector, pinned process identity, and
current status. Query `spec.scope.scopeId` to identify the owning assignment.

```bash
agent-sec-cli --socket "$SOCKET" binding list --limit 100 --offset 0
agent-sec-cli --socket "$SOCKET" binding get --binding-id "$BINDING_ID"
```

Application progresses from `PENDING_APPLY` through `APPLYING` to `READY`.
Permanent or exhausted failures appear as `APPLY_FAILED` or `DELETE_FAILED` with
`status.error`; query commands still exit successfully. Process exit or selector
mismatch retires the Binding through the same cleanup path as Scope deletion.
Cleanup retains uncertain deployment outcomes until absence is confirmed. Automatic
retries are bounded; `scope retry` retries failed owned work. There is no `--wait`.

## Common options

| Option | Purpose |
|--------|---------|
| `--socket PATH` | Required absolute daemon socket path; accepted before or after the subcommand |
| `--timeout-ms N` | Positive unsigned 32-bit integer, default `5000`; connection, sending and receiving share one time budget |
| `--limit N` | List page size, `1..=1000`; default `100` |
| `--offset N` | List offset, unsigned 32-bit integer; default `0` |
| `--help` | Help at the root, command group or operation level; no daemon connection |
| `--version` | Root-level version output; no daemon connection |

Options accept `--key value` and `--key=value`. Quote names and paths containing spaces;
for a value beginning with `--`, use `--key=--value`. Repeated options are rejected.
List commands return one `{items,total}` page. `total` is the count before pagination;
request subsequent pages explicitly, advancing offset by the actual number of returned
items. Pages may stop early at the 3 MiB encoded item budget.

## Output and errors

| Result | Output | Exit status |
|--------|--------|-------------|
| Success | Method result as formatted JSON on stdout; stderr is empty | `0` |
| Daemon rejection | JSON `{requestId,error:{code,message}}` on stderr; stdout is empty | `1` |
| File, connection, response or output failure | Error explanation on stderr | `1` |
| Invalid command-line arguments | Usage error on stderr | `2` |

Template files are limited to 4 MiB and must contain valid JSON without unknown or duplicate
fields. The complete encoded request and response each have a separate 4 MiB limit, including
the line delimiter. Passing the file-size check does not guarantee the assembled request fits;
an oversized request is rejected before sending. The daemon additionally rejects a
serialized Policy or complete Scope larger than 1 MiB before storing it.

The timeout covers daemon communication, including waiting for daemon processing. It excludes
file reading, request encoding, response decoding and output. The CLI never retries automatically.
A timeout or invalid/missing response after sending does not prove the request was not executed;
inspect current state before submitting another change.

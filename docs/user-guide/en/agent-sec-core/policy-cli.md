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

## Built-in credential injection (unreleased V2)

`agent-sec-cli inject-credential` combines fake generation, Policy creation and
Scope assignment for the packaged GitHub Cedar template. Existing step-by-step
commands remain available. This source-tree feature is not a released Gateway
installation path; it requires a matching V2 CLI/daemon and installed gateway assets.

```bash
sudo agent-sec-cli inject-credential --builtin github \
  --real-token-file /root/github.token \
  --owner agentuser --cmdline-arg /usr/local/bin/my-agent
```

Run as root, select a non-root Agent owner (`--owner` or `--owner-uid`), and target
that Agent with exactly one of `--cmdline-arg`, `--executable` or `--process-name`.
For script Agents, use their full command path, not a shared interpreter selector.
The real token file must be private and owned by root or the owner; every ancestor
must be trusted and non-writable by group/others. Keep the real token inaccessible
to the Agent. The root-owned packaged template is never changed or embedded in the CLI.

The JSON result contains IDs, `fakeTokenFile`, gateway status and teardown commands,
never token values. When the run used a non-default daemon socket, every returned
teardown, recovery and next-step command includes the same `--socket`. A second
concurrent run fails immediately with a lock error and creates no records; retry after
the first finishes. Generated fakes default to the owner's
`.config/agent-sec/gateway/`; `--output-dir` selects another owner-writable directory.
`--fake-token-file` instead uses an existing private fake. The original real file
is unchanged; staged copies are removed after successful registration. Admission
failure retains root-only recovery files; follow the error's manual recovery and
cleanup commands. Keep the delivered fake for Agent startup. Configure `GH_TOKEN`/`GITHUB_TOKEN` yourself; no `agent.env` is written.

The command never enables the gateway. Enable it separately with
`sudo agent-sec-cli gateway enable`, then obtain the shared CA with
`agent-sec-cli gateway ca` and configure the Agent's TLS trust. Binding acceptance
alone does not establish traffic protection. `--wait-seconds N` waits for existing
matching processes to reach READY; timeout still prints the result and exits 1.
`--dry-run` reads only the template, without token reads or daemon calls.
To revoke injection, execute the returned Scope deletion command, wait for cleanup,
then delete the Policy. See the [Gateway contract (Chinese)](../../../../src/agent-sec-core/docs/design/V2_SECRET_GATEWAY_DESIGN_zh.md#22-scope-准备与发现)
for readiness, response-rewrite limits and partial-failure recovery.

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
records, bind an execution process, or deploy enforcement. After human review, save
the candidate Cedar text to a file and submit it with `policy create --file` as
shown below; Policy admission still validates the source and schema.

The LLM receives only the task text and the unified `seccore.cedarschema`,
and directly returns `{"policy":"...","unresolved":[]}`. Rust checks absolute path expressions in the draft and runs official Cedar Strict validation.
A malformed policy/unresolved JSON response or a repairable Cedar error gets one admission retry
within the original deadline. The `asc.intent.draft.v1` draft preserves the original response and task provenance. Neither validation nor an empty
`unresolved` proves complete or correct interpretation; unresolved requirements
are model-reported. Once a draft passes validation, compatibility findings accompany its
original rules. A confirmation that would widen a grant returns the original draft for clarification.
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
Proposal generation does not inspect host paths, link targets, file contents, directory listings or Git changes. Review the task text
before sending it, including any absolute paths.

```bash
agent-sec-cli intent propose \
  --task "Allow writes only under /srv/reports. Ask before launching a command that requests writes to /srv/reports/final." \
  --format cedar > task.cedar
```

Only `--task` is required: it uses the default daemon endpoint and text output.
The model must express task targets as canonical absolute paths; they may refer to
files not yet created. Relative or ambiguous targets belong in `unresolved`, and
users cannot supply extra path or protection rules. `--socket` or
`AGENT_SEC_DAEMON_SOCKET` overrides the endpoint; a failed connection never falls
back to a local model.

### Policy and safety boundaries

- Use only the unified schema: `file.read`, `file.write`, `file.exec`, `process.spawn` and
  `network.use`. Modification requests carry `WriteOperation` values
  `write/create/delete/rename/setattr`; a delete prohibition is not immutability.
  `file.read` means content reading, not listing names or reading metadata.
- Task candidates contain the validated model rules. The command-mode sandbox hook
  derives a read-only shell launch baseline. Set `AGENT_SEC_COSH_SANDBOX_RUNTIME_PERMISSIONS`
  to an absolute Cedar preset path when additional dependencies are required.
- Paths use `resource.path` and `context.input.program/cwd`, not File UIDs.
  The frontend accepts canonical absolute literals with `==`/`!=`, or `like`
  patterns whose only wildcard is the trailing directory `/*`. Dynamic and
  noncanonical path expressions, unbound templates, model-supplied Subject/File
  identities and unknown annotations are rejected. Task permits may carry
  `@advice` with an `escalate` verdict and a nonempty message to request confirmation. Path
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
  For “allow writes under a directory,” check for a scoped permit. For “allow writes
  **only** under a directory,” also check for a forbid covering writes outside it.
  Other active policies can grant those writes if the forbid is missing. Any
  operation or condition limiting the requirement must also limit the forbid.
  A prohibition within that scope can target a particular file or operation.
- The model leaves purely prohibitive requirements without additional permits. No sensitive-path
  blacklist, execution ban or network ban is added implicitly. Inspect the actual Cedar against the task;
  neither Strict nor model-reported `unresolved` proves that every requirement is met.

### Inspect the result

Default `--format text` displays the task, `unresolved` and a rule-by-rule review derived from
the validated Cedar, followed by the full Cedar and expiry. The review includes conditions and
confirmation messages. Rules with unsupported conditions appear in Cedar. The same review is
available in `candidate.notes` in JSON output. `--format cedar`
includes unresolved requirements, per-requirement coverage and any confirmation
conditions or compatibility findings as Cedar comments. Mixed principal headers are rejected.
The complete candidate is checked against the OS PDP filesystem projection and the AgentSight
file-policy translator. A rejection is reported without rewriting the validated rules; `--format json` prints
the complete result. Cedar control characters are escaped only while stdout is a terminal;
redirected or piped output keeps the model text unchanged. An empty model draft grants no permissions.
For OS requests, the PDP confirms network access at command launch; the sandbox projects an ordinary network grant to a process-wide network mode. Tasks requiring approval per connection remain unresolved.
Review the `HUMAN GATE` notes for gated paths and whole-network access, and
`COVERAGE` for the consumers each statement shape can support. No format prompts for
input or requests approval.
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

TTL and trusted process identity are outside the task policy. Trusted approval,
specific process binding and expiry revocation remain downstream work. Enforcement must also normalize paths, handle
symlinks/hardlinks and namespaces, authorize both rename source and destination,
and ensure complete argument indices consistent with `arg_count` and valid
attribute values.
This command does not establish these conditions and must not be described as
blocking `rm`.

### Propose for a single hook command

`agent-sec-cli intent propose-command --command "<command text>" [--cwd <dir>]` serves
hook integration: it sends only the hook's own `command` and `cwd` and returns the
unapproved Cedar for that one command. The input is any executable command line, not
only shell built-ins: `python3 train.py` and `node app.js` are analyzed like
`rm -rf build`, and only the command text differs.
The entry point is a `PreToolUse` call from the shell-class tool, whose input carries
that command line: `run_shell_command`/`shell` in cosh-ng (`sandbox-guard` with matcher
`^(run_shell_command|shell)$` in `cosh-extension.json`), `Bash` in Qoder and Codex, and
`run_shell_command` in Qwen Code. Python has no separate tool entry and is submitted
through it as well. These payloads supply `tool_input.command`; cosh-ng also supplies
`tool_name` and the session `cwd`, while a host without `cwd` must pass `--cwd`
explicitly.

Proposals request `process.spawn` for PATH utilities by program name, such as
`cat` in `cat a.txt`, alongside their file or network actions. The executable
path depends on the runtime's PATH and is left as a `file.exec` caveat when unknown.

For simple `cat src/*.txt > out/all.txt` and `tar -czf out/a.tgz -C src .` commands,
Intent derives data permissions from literal operands locally. Bash expands the glob.
The request covers the input directory for reading, and the existing output file (cat) or
archive output directory (tar) for writing. If that directory is the working directory
or an ancestor, the request covers only the archive file. For tar, the local rule recognizes `.` after
`-C DIR` or `--directory DIR`. Named inputs such as `README.md` go to the model, since
the command alone does not establish their file type. PDP evaluates the request against PAP.

For cosh-ng, these are the fields this hook point carries today, and only `command` and
`cwd` reach the candidate:

| Field | Use |
|-------|-----|
| `tool_input.command` | The command text and the only policy source; it carries no argv, environment or `PATH` lookup result |
| `cwd` | Explains relative paths and is resolved to a canonical absolute path; it is not a permission scope |
| `tool_name` | Identifies the shell-class tool and is not written to the candidate |
| `session_id`, `run_id`, `tool_use_id` | Correlate the call for logging and audit |
| `hook_event_name`, `timestamp`, `transcript_path`, `skill_context` | Protocol fields; cosh-ng's `transcript_path` is a placeholder |

The payload carries no argv, process environment, `PATH` lookup result, absolute
executable path, redirection target or network endpoint, so the candidate can only
state what the command literal determines. Obtaining more requires changing the hook
implementation (parse the command line, read the environment or resolve links in the
hook and pass new fields); this interface does not fill the gap, and the CLI does not
infer it locally.

Other `PreToolUse` calls do not apply: `Read`/`Write`/`Edit`/`Grep`/`Glob`/`Skill`/
`Task`/MCP tools pass structured arguments with no command line and need a separate
tool-argument-to-permission mapping that this interface does not provide yet but is
planned; user-prompt, post-tool and session-end hook points carry no command text either.
If an earlier hook in the same chain rewrote `tool_input.command`, for example a
sandbox wrapper, pass the original command instead.
`--cwd` defaults to the CLI process's current working directory and is resolved to a canonical
absolute path; it explains relative paths and is not a permission scope. The command is never
executed and no file content is read.

```bash
agent-sec-cli intent propose-command \
  --command 'read -r line < report.csv' \
  --cwd /work
```

Output is JSON by default because callers parse this stream:
`{"outcome":"candidate","policy":"Cedar text","caveats":["..."]}` or
`{"outcome":"needs_clarification","reasons":["..."]}`. `caveats` lists known gaps
that do not block the candidate, such as an unresolved PATH lookup or unknown script
behavior, and approval must review them; the array is empty when there are none.
`--format text` groups the candidate by read/write/exec/spawn/network with `none` for
absent categories, then lists `Not covered by this candidate`, and falls back to the
exact Cedar when a condition cannot be summarized. `--format cedar` writes `caveats` as
`// Not covered:` comments; a clarification prints only comments, so no permit can be
mistaken for a result. When a required file target depends on shell parameter expansion,
such as in `printf x > "$TARGET"` or `rm -rf "$DIR"`, supply the literal target path.
The result is `needs_clarification` with a reason and no partial policy. Commands with no
file, process or network action, such as `pwd` or `echo` without redirection, can
produce an empty candidate. It grants nothing and keeps its `caveats`. The CLI
re-runs Cedar validation before rendering. Both outcomes exit `0`; an input the daemon
would reject exits `2` without a request.

The `--format cedar` output is the unapproved candidate text; approval and execution stay
outside this subcommand. The candidate covers only the command's own demand, so the program,
cwd and loader paths are not included and a caller must add them explicitly; the CLI adds
nothing implicitly. The PDP restricts the request to the PAP-managed Cedar policy set:
`effective = request ∩ PAP policy grants`, with matching forbids taking precedence.

This path does not read or rewrite base policies, does not compute `effective` locally, and
does not execute the command.

## Intent init: generate Cedar locally from explicit rules

`agent-sec-cli intent init` compiles explicit allow/deny rules into validated
Cedar on the local host. It needs no daemon, model or network, sends nothing
off the machine, and never reads the paths it names. Like `intent propose`, it
only prints an unapproved preview: it does not persist policy-management
records, bind a process or deploy enforcement.

```bash
# Deny all network use
agent-sec-cli intent init --deny-network

# Read the data tree, write only its out subtree, no network
agent-sec-cli intent init \
  --allow-read '/data/**' \
  --allow-write '/data/out/**' \
  --deny-network

# Read access restricted to one interpreter, with a secrets exception
agent-sec-cli intent init \
  --allow-read '/srv/app/**' \
  --deny-read '/srv/app/secrets/**' \
  --program /usr/bin/python3
```

Quote subtree paths in the shell so `*` is passed to the CLI instead of being
glob-expanded.

Rules can also come from a JSON spec file (`-` reads stdin), which is mutually
exclusive with the rule flags:

```bash
agent-sec-cli intent init --spec-file spec.json --format json
```

```json
{
  "allow_read": ["/srv/app/**"],
  "deny_read": ["/srv/app/secrets/**"],
  "programs": ["/usr/bin/python3"],
  "deny_network": true
}
```

The spec format is `asc.intent.spec.v1`; omitted fields default to empty or
false, and unknown fields are rejected. The equivalent flags are `--allow-read`,
`--allow-write`, `--allow-exec`, `--deny-read`, `--deny-write`, `--deny-exec`
(each repeatable), `--allow-network` / `--deny-network` (mutually exclusive) and
`--program` (repeatable).

### Rule semantics

- Paths must be canonical absolute literals; a trailing `/*` or `/**` grants or
  denies the directory itself and all descendants; the two forms are equivalent.
  For example, `/data/secrets/**` includes `/data/secrets` but not
  `/data/secrets-backup`. `/*` and `/**` include `/` and all absolute paths.
  Other wildcards, relative paths, `..` and backslashes are rejected.
- Deny rules render as `forbid`, which overrides `permit`. Cedar denies by
  default without a matching permit, so an allow-only spec already blocks
  everything it does not name; a deny-only spec adds explicit prohibitions on
  top of that default.
- `--program` restricts read/write rules to the listed requesting programs
  (exact paths). It does not affect `file.exec` rules: the schema records no
  requesting program for an invocation, whose target executable is already the
  rule's resource path. A spec with programs but no file rule is rejected.
- Network control is a whole-network switch (`network.use` on
  `SecCore::Network::"all"`); the schema has no endpoint granularity.
- Rendering is deterministic: rules follow a fixed action order and duplicates
  collapse, so identical specs produce byte-identical output. The result passes
  the same official Strict validation as the model path.

### Output and errors

`--format cedar` (default) prints the Cedar with a
`// LOCAL: generated by intent init; not approved, not effective.` header.
`--format json` prints `asc.intent.init.v1` with the normalized `spec`, the
validated `policy` text and `schema_sha256`. Paths and programs containing
control characters, Unicode bidi controls (including U+202E and U+2066–U+2069),
or Unicode line/paragraph separators are rejected before either format emits
output. Accepted input uses the same output on terminals and pipes, preserving
the Cedar template's line breaks. Rejected input is escaped in error messages.

An empty rule set is an error, not an empty deny-all policy. Contradictory
network switches, invalid paths or programs, more than 1024 rules and spec files
larger than 256 KiB are rejected before any output. Input and validation errors
exit `1` with a message naming the offending input; argument conflicts exit `2`;
success exits `0`. No run installs, records or requests an approval, and the
output must not be described as blocking actual operations.

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
agent-sec-cli --socket "$SOCKET" policy create --name "protect files" --language json --file policy.json
agent-sec-cli --socket "$SOCKET" policy get --policy-id "$POLICY_ID" --revision 1
agent-sec-cli --socket "$SOCKET" policy list --limit 100 --offset 0
agent-sec-cli --socket "$SOCKET" policy update --policy-id "$POLICY_ID" --name "protect files v2" --language json --file policy-v2.json
agent-sec-cli --socket "$SOCKET" policy delete --policy-id "$POLICY_ID" --revision 2
```

`--language` selects how `--file` is read: `cedar` (the default) reads raw Cedar
text, and `json` reads a native template or a JSON source envelope.

**Breaking change:** `policy create` and `policy update` now read `--file` as
raw Cedar text unless `--language json` is given. Scripts that pass JSON files
without it are rejected by the daemon with `invalid_argument` and no Policy
change; add `--language json` to keep their behavior.

Use the ID and revision returned by successful commands. These are reference
examples, not a sequential script. Create a Scope while the desired Policy revision
is current. Both create and update require a name and complete template; update is
not a partial patch. A changed name or template increments the revision; identical
content does not. Only the current complete record is retained. Policy get/delete
require its exact revision; an old revision returns `not_found`. File paths are
resolved relative to the CLI working directory.

Manual Scopes and their Bindings retain the snapshots selected at creation.
Delete the Scope to withdraw its assignments. A known deleted Policy ID retains its
revision counter; updating it recreates content at the next revision. An unknown ID
cannot be updated.

## Cedar Policy source

The same `policy create` and `policy update` commands read a Cedar file
directly. Save this example as `cedar-policy.cedar`:

```cedar
forbid(principal, action == SecCore::Action::"file.write", resource)
when {
    context.input.operation == SecCore::WriteOperation::"delete" &&
    resource.path == "/workspace/important"
};
```

```bash
agent-sec-cli --socket "$SOCKET" policy create --name "Cedar protection" --file cedar-policy.cedar
agent-sec-cli --socket "$SOCKET" policy get --policy-id "$POLICY_ID" --revision 1
agent-sec-cli --socket "$SOCKET" policy update --policy-id "$POLICY_ID" --name "Cedar protection v2" --schema-revision 1 --file cedar-policy-v2.cedar
```

The file must be UTF-8 and is stored verbatim. `--schema-id` defaults to
`seccore.system` and `--schema-revision` to the revision of the schema embedded
in the CLI (1); both apply only to `--language cedar`. The resolved reference is
saved with the Policy and selects its schema revision. `agent-sec-cli policy default` prints the built-in baseline as a
Cedar file that these commands accept; `policy default --json` prints it as a
JSON source envelope instead.

Use `--schema-revision 3` to target an agent family in Cedar, for example
`principal has agentType && principal.agentType == "qoder"`.
Use `context.agent.agentType` in Sandbox or Hook requests.
An optional `ownerUid` can narrow a process assignment. A Sandbox request supplies
the available agent category and omits the process owner. Manual `scope create` accepts
revisions 1 and 2. If you upgrade a Policy to revision 3, existing manual assignments
retain their pinned snapshots.

**Before upgrading**, use the old daemon to review current Policies and pinned
Scope and Binding snapshots. Export affected Policies and replace the Cedar
attribute `agent_type` with `agentType`, including in `has` conditions. In a
maintenance window, withdraw the affected old assignments and records while
the old daemon can still read them. Import the rewritten Policies with the
upgraded daemon, recreate any required manual Scopes and update request
clients before resuming affected workloads. The upgraded daemon rejects
`agent_type` on import. If stored copies still use it, startup stops and
reports the Policy IDs and revisions that require migration.

PAP accepts up to 4096 current v3 policies and 16 MiB of serialized v3 snapshots. A write that exceeds either limit reports an error. If an existing database is over the limit, delete or shrink v3 policies to resume automatic deployment.

The daemon's built-in `cosh` mapping matches the process name `cosh-core`.
To deploy to Qoder, configure its installed entry point in the root-owned
`/etc/agent-sec/agent-types.json` and restart the daemon. For example, if the
Qoder executable is installed at `/opt/qoder/bin/qoder`, use this mapping.

```json
{"qoder": {"executable": "/opt/qoder/bin/qoder"}}
```

Each entry accepts one exact `processName`, absolute `executable`, or absolute
`cmdlineArg`. Process names and arguments are discovery hints that another
process under the same UID can match. Use a dedicated non-root UID for
credential workloads that require isolation from other user processes. A
missing category appears in policy diagnostics and can be configured after
the Policy is saved. If the file is missing, the daemon uses its built-in
mappings. An invalid or unreadable file suspends automatic deployment. New
root processes receive no automatic file Binding and automatic credential
routes stay offline. Fix the file and restart. Manual Scopes and Sandbox
requests remain available.

When `policy get` has automatic assignment results, it returns `automaticDiagnostics`
with the source revision, status, reason and available `scopeId`. Use the Scope ID
to find the Bindings and inspect installation results for individual processes.
File rules with no exact `agentType` report an unassigned ActPlane result.
After a credential Policy update, use `gateway status` to confirm the serving
revision. If candidate admission fails, the old revision can still serve.
To withdraw a grant independently of a failed update, delete its source Policy
and verify withdrawal in Gateway.

A JSON source envelope names the schema inside the file and is imported with
`--language json`:

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
agent-sec-cli --socket "$SOCKET" policy create --name "Cedar protection" --language json --file cedar-policy.json
```

Get/list return the saved original text and exact schema reference. Invalid
syntax, schema or common-subset constructs return `invalid_argument` without
changing stored policy content or revisions. Unknown schema revisions are rejected
without fallback. Native templates remain supported in their existing shape.

Policies describe file and network requirements through `file.*` and `network.*`
actions. Network actions cover whole-network permissions and HTTP requests;
credential injection is one network capability. Mixed deployment currently supports
independent file-protection `forbid` rules and HTTP credential-injection `permit`
rules. Use schema revision 2 for a manual assignment or revision 3 for principal-based targeting. Credential administration requires root. Before downgrading to a version without mixed-policy support, delete
mixed Scopes, confirm cleanup and remove their mixed Policy records before restoring
older policies and assignments.

Policy creation success does not establish target PEP support. The current PEP
Translator supports a Cedar `forbid` subset for deletion, ordinary-write
restrictions and file read denial. With `file.read`, exact-file targets require
AgentSight's `file_read_protection`, and directory-tree targets require
`directory_tree_read_protection`. See [AgentSight's read-protection
boundaries](../agentsight/integrations.md#deny-reads-below-a-directory-tree).
Unsupported semantics reject the entire translation. Existing
manual Scope/Binding snapshots retain their saved Policy revision and schema when a
Policy is updated or deleted. Source can be reloaded into an in-memory PST;
neither AST nor PST is stored in SQLite. An older binary cannot read new Cedar
records, so preserve a pre-upgrade database backup when planning rollback.
The known SDK stack-overflow limit for deeply nested source remains unresolved;
use administrator-reviewed policies rather than untrusted generated input.

Sandbox runtime authorization uses the same Cedar Policy objects. The OS authorization PDP evaluates
Agent subjects, `process.spawn` and supported permit advice. Authorization is enabled
by default, without a separate permission-ceiling configuration. Decisions must be
accepted within 60 seconds; accepted approval waits and execution may finish later.
For v3, the PDP loads the Cedar statements or fragments selected for Sandbox.
It reuses them while the PAP policy-set generation is unchanged and evaluates
each request with its own attributes. Use `principal is SecCore::Subject` with
the optional `agentType` attribute to match requests. For v1/v2, the existing
principal-header rules determine which policies are eligible.

Updates and deletions take effect on subsequent requests. An empty set denies
authorization. Read or validation failures stop new authorizations. Existing
Scope/Binding snapshots and accepted decisions retain their bound versions.
See the [sandbox guide](sandbox.md#use-policy-authorization).

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
| Local `policy default` | Raw Cedar on stdout; `--json` selects a JSON source envelope; no daemon request | `0` |
| Daemon rejection | JSON `{requestId,error:{code,message}}` on stderr; stdout is empty | `1` |
| File, connection, response or output failure | Error explanation on stderr | `1` |
| Invalid command-line arguments | Usage error on stderr | `2` |

Policy source files are limited to 4 MiB. With `--language json`, they must contain valid
JSON without unknown or duplicate fields; with `--language cedar`, they must contain
UTF-8 Cedar text. The complete encoded request and response each have a separate 4 MiB limit, including
the line delimiter. Passing the file-size check does not guarantee the assembled request fits;
an oversized request is rejected before sending. The daemon additionally rejects a
serialized Policy or complete Scope larger than 1 MiB before storing it.

The timeout covers daemon communication, including waiting for daemon processing. It excludes
file reading, request encoding, response decoding and output. The CLI never retries automatically.
A timeout or invalid/missing response after sending does not prove the request was not executed;
inspect current state before submitting another change.

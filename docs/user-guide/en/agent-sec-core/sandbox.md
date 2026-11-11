# Run commands with explicit sandbox permissions

[中文版](../../zh/agent-sec-core/sandbox.md)

`agent-sec-cli sandbox run` confines a command and its children to caller-supplied
filesystem and network permissions. It runs locally without a daemon. The caller
chooses the permissions; the sandbox installs them before starting the command.

## Requirements

Install the Rust V2 CLI using the [component guide](index.md#installation).
This command requires Linux, unprivileged user namespaces, and non-setuid system
bubblewrap with `--bind-fd`, `--ro-bind-fd`, `--cap-add` and `--unshare-user`.
Distribution backports of these options are supported. The statically linked
`linux-sandbox` must be installed beside `agent-sec-cli`. A noexec rule also
requires working recursive `mount_setattr` support. Missing prerequisites stop
execution; there is no host-execution fallback.

## Run a command

The following example permits host reads, writes only to `/tmp/asc-demo`, and
blocks IP networking and direct execution from that directory. The root read
grant is deliberately broad to include runtime dependencies; replace it with
your application's explicit read paths for a restricted workload.

Save this `OsPermissionSet` JSON as `permissions.json` in the directory created
by the commands below:

```json
{
  "version": 1,
  "filesystem": {
    "default": "deny",
    "rules": [
      {"effect": "allow", "path": "/", "rights": ["read"]},
      {"effect": "allow", "path": "/tmp/asc-demo", "rights": ["read", "write"]}
    ]
  },
  "execution": {"deny_paths": ["/tmp/asc-demo"]},
  "network": {"mode": "off"}
}
```

```bash
mkdir -p /tmp/asc-demo
cd /tmp/asc-demo
# Save the JSON above as permissions.json.
agent-sec-cli sandbox run --permissions permissions.json -- /bin/sh -c 'printf "sandbox\n" > report.txt'
cat report.txt
```

`--permissions` is required and reads at most 4 MiB. Put the executable and its
arguments after `--`. The command uses the caller's working directory and
environment; environment names and values must be valid UTF-8.

## Cedar input

The same `--permissions` option also accepts raw Cedar using `seccore.system`
schema revision 1, or the existing `{"language":"cedar","content":{"schema":
{"schemaId":"seccore.system","revision":1},"text":"..."}}` source envelope.
Format detection uses the contents, not the filename. OS permission JSON retains
its existing behavior. Cedar input does not enable PDP calls or start a daemon.

For example, save the following as `permissions.cedar` after creating `/tmp/asc-demo`:

```cedar
permit(principal, action == SecCore::Action::"file.read", resource);
permit(principal, action == SecCore::Action::"file.write", resource)
when { resource.path == "/tmp/asc-demo" || resource.path like "/tmp/asc-demo/*" };
```

```bash
agent-sec-cli sandbox run --permissions permissions.cedar -- /bin/sh -c 'printf "sandbox\n" > /tmp/asc-demo/report.txt'
```

This example grants broad host reads and disables IP networking. A Cosh invocation
uses the same option followed by `-- cosh`; explicitly include its writable state
paths and `network.use` if its model provider needs networking. Dependencies are
never added automatically.

The conversion produces a coarse permission request, not equivalent enforcement
of every Cedar condition:

- File reads and writes use literal paths, directory-prefix `/*` patterns and
  Boolean combinations. Include both directory equality and its child pattern to
  authorize a complete directory tree. Exact file grants are checked against the
  local object; point-only or descendants-only directory grants are rejected.
- Any allowed write operation (`write`, `create`, `delete`, `rename`, `setattr`)
  maps to `write`, including reads and deletion. A delete-only forbid therefore
  does not remove a write grant when another mutation remains allowed.
- File `context.input.program` equality/inequality conditions are projected across
  matching programs; the resulting access applies to the entire process tree.
  Path exclusions and unconditional forbids remain effective.
- `network.use` maps to whole-network `full` or `off`. With only forbids, no
  positive permission is inferred.
- If invocation rules are present, `file.exec` and `process.spawn` check the
  initial command and its arguments. They do not restrict every descendant launch
  or generate noexec rules. Continue using OS permission JSON for explicit noexec.
  In the PDP flow, these input checks run before the authorization request. They
  are not checked again against a command transformed by an authorized `MODIFY`
  or `STEP_UP` decision; the PDP authorizes that final command, and `STEP_UP`
  additionally requires approval of the returned decision.

Subject-specific rules, advice, unsupported file conditions (such as a particular
chmod mode), arbitrary globs and unrepresentable regions are rejected. Input is
bounded to 4 MiB, 1,024 policies and a region budget that also accounts for condition
size. Distinct path/program limits are checked during collection, before selecting
representatives or evaluating regions. Cedar file-permission path/program literals
are limited to 4,095 UTF-8 bytes. Captured paths and ancestors have a separate 4 MiB total string
budget, checked before copying each distinct path. Intention
candidate envelopes are not permission files: select their Cedar text only after
handling unresolved requirements and candidate validity.

Cedar file read/write path literals must already exist. Preparation retains all paths, including
exclusions, resolves file and directory symlinks (also in intermediate components),
then evaluates and compacts permissions using canonical paths. An allow through an
alias cannot discard a forbid on its target. Readable aliases are retained for the
command; aliases with a writable ancestor are rejected because that ancestor could
retarget the link after installation. Uninstallable overlapping aliases are also
rejected by the backend. Explicit Cedar paths to files with multiple hardlinks are
rejected before compaction, since canonicalization cannot identify their other names.
Whole-subtree exclusions retain explicit deny rules even outside allowed roots;
the PDP preserves these request exclusions and the PEP rejects decisions that
discard them. The backend therefore still rejects hardlinked files inside excluded
directories before execution. A complement such as "everything except `/work`"
continues to use default deny outside its grants, without a root deny that would
also block `/work`.
The initial executable and working directory must match
the captured paths, and captured identities are checked again before execution.

For applications preparing PDP requests, parse the current source with
`asc_pep_runtime::PermissionInput::parse(text)` and pass it as the existing
`PermissionProvider` to `PepRuntime::authorize` or `run`. Both JSON and Cedar
produce `requested_permissions`; the daemon policy still determines the final
`effective_permissions`. The input is not uploaded to PAP as an authorization
policy. `AGENT_SEC_COSH_SANDBOX_PERMISSIONS` accepts the same formats and rereads
its file for each Hook preparation. A changed source affects the next request,
not an already prepared decision or running sandbox.
Both runtime entrypoints call `PermissionProvider::prepare` to bind paths before
execution or authorization. `PermissionInput::resolve` is only a lexical preview;
it does not produce a bound execution plan.

## Permission format

| Field | Behavior |
|---|---|
| `version` | Must be `1`; unknown fields are rejected. |
| `filesystem.default` | Must be `deny`. |
| `filesystem.rules[].effect` | `allow` or `deny`; deny wins regardless of rule order or a more specific allow. |
| `filesystem.rules[].rights` | A nonempty list of distinct `read` and/or `write` rights. Write includes read, creation, deletion and rename; denying read also prevents write. |
| `execution.deny_paths` | Mount-level noexec for named files or directory subtrees, including their visible submounts. |
| `network.mode` | `off` blocks IP networking; `full` uses host networking without destination, port or direction filters. |

Paths must be existing, normalized absolute paths, without wildcards, dot
components or a trailing slash except for `/`. Files select one object;
directories select their subtrees. Each of `filesystem.rules` and
`execution.deny_paths` accepts at most 1,024 entries. New files can be created
inside a writable directory. Include the executable, loader, libraries, working
directory and application data needed by the command; the runtime adds no grants.
`/proc` and `/dev` are reserved for sandbox infrastructure and cannot be explicit
rule targets.

Read-only does not mean non-executable. noexec prevents direct execution through
the named mount; it does not prevent an allowed interpreter reading a script,
execution through another allowed alias, or execution of a copy elsewhere.
Filesystem deny paths must be canonical. The backend rejects symlink deny paths,
overlapping explicitly mounted aliases and hardlinked files in explicitly denied ranges.
noexec paths must also be canonical. Host file permissions still apply.

Write grants restrict paths, not every name of the same file. A pre-existing
hardlink in a writable directory can still modify a file outside the write grants.
The backend leaves writable paths unscanned to preserve startup availability.
When `/` is not writable and the plan has writable paths, each actual strict helper
launch writes one warning to stderr and continues. This boundary notice also
appears when no hardlinks exist; it is not a detection result or a guarantee that
outside file contents are protected. Plan prechecks do not repeat the warning;
read-only plans and plans granting write on `/` do not emit it.

Use independent file copies when content isolation is required. Shared host trees
remain mutable by other host processes; the warning does not prevent concurrent
hardlink creation or directory-entry replacement. Explicit deny checks above
remain enforced and can still reject execution.

## Execution behavior

Both network modes retain process isolation, privilege dropping and base seccomp
restrictions. Socket stdio is rejected; caller-provided terminal, pipe and
regular-file stdio remain communication channels. Other inherited file
descriptors and connections to external Unix sockets are blocked. Local stream
and sequenced-packet Unix socket pairs remain available for child-process IPC.

The CLI returns the target's exit code, or `128 + signal` for a reported signal.
It leaves the caller's signal handling unchanged and provides no execution
timeout or active cancellation API. Installation must finish within 30 seconds
after the helper starts. Preparation or installation failure stops before the
target starts; failure never retries the command outside the sandbox.

Completion of the main command cleans up sandbox descendants. Parent exit
terminates the direct helper and the established sandbox process tree. During
bubblewrap initialization, parent termination can leave an installation child
that has not started the target; this cleanup race remains a known limitation.
See the [execution design](../../../../src/agent-sec-core/docs/design/LINUX_SANDBOX_PEP_DESIGN_zh.md)
for implementation and validation boundaries.

## Use policy authorization

Applications that need authorization use `asc-pep-runtime::PepRuntime` with
`DaemonAuthorizationClient`, then execute through the same backend. The runnable
[`authorized_sandbox` example](../../../../src/agent-sec-core/v2/crates/asc-pep-runtime/examples/authorized_sandbox.rs)
shows preparation, policy warnings before execution or ask, and receipts. The
public execution command remains `sandbox run`; authorization integration uses
the library and daemon RPCs, then the same local execution backend.

Store Cedar policies through the existing `policy create/update` commands. The
policy file is a raw Cedar file, as shown in the [Policy guide](policy-cli.md#cedar-policy-source).
Each successful update is available to the next authorization request.

Enable authorization with
`agent-sec-daemon --authorization-config /etc/agent-sec-core/authorization.json`.
This root-owned configuration selects PAP policy IDs and runtime limits; it does
not contain Cedar policy text or accept a policy snapshot file:

```json
{
  "baseline_policy": "<policy ID returned by PAP>",
  "environment_permissions": {
    "version": 1,
    "filesystem": {"default": "deny", "rules": [
      {"effect": "allow", "path": "/", "rights": ["read"]}
    ]},
    "execution": {"deny_paths": []},
    "network": {"mode": "off"}
  },
  "ttl_seconds": 60
}
```

An optional `task_policy` selects another PAP policy ID. Both sources must use
`schema: {schemaId: "seccore.system", revision: 1}`. Baseline and task are evaluated
independently. `environment_permissions` caps the permissions returned for each
`type: "os"` request. The sandbox executor enforces them on the launched process
tree. AgentSight uses Scope/Binding to select other host processes. Configuration and its parent
directories must be root-owned and not writable by group or other users; restart
the daemon to change this selection or its runtime limits.

Every evaluate reads the selected policies' current revisions from PAP together.
The compiled cache is reused only while those revisions remain current. Missing,
deleted or unusable policies stop authorization; the daemon does not fall back to
an earlier policy. Existing Scope/Binding assignments retain their fixed revisions.

The PEP must accept a decision within `ttl_seconds`. Once accepted for that one
invocation, approval waiting, installation and execution do not expire with that
deadline. Policy updates affect subsequent requests; accepted decisions remain
bound to their original command and permissions. Declined or failed invocations
cannot reuse the decision.

`authorization.evaluate` returns ALLOW, DENY, MODIFY, STEP_UP or DEFER; it never
runs the command. STEP_UP asks locally and executes after approval without a
second evaluate. PDP authorizes a transformed command and its permissions within
the same evaluation when the supplied facts are complete. PEP verifies that final
command locally and executes it; DEFER ends the invocation without execution. Narrower effective permissions also produce MODIFY.
Policy `warn` advice adds `warnings` without requiring approval or adding a sixth
decision. The example displays warnings before execution or ask and includes them
in its JSON result. The daemon also records them with the execution receipt.
`authorization.report` records execution results in the existing SQLite/JSONL
security event sinks after checking the decision reference, caller UID and result
consistency. Ack confirms all required writes, not command success; partial writes
can fail reporting. A failed report never reruns a command. Pending decisions are
process-local and lost on daemon restart. Without an authorization configuration
these RPCs return `unavailable`; direct `sandbox run` remains usable.

See the [implementation design](../../../../src/agent-sec-core/docs/design/LINUX_SANDBOX_PEP_DESIGN_zh.md)
for schemas, component responsibilities and validation coverage.

## Enable authorization in Cosh NG

Enable the daemon authorization configuration above and activate the ASC extension in Cosh.
Prepare a complete `OsPermissionSet` file for the target workspace, then set this in the
process environment before starting Cosh:

```bash
export AGENT_SEC_COSH_SANDBOX_PERMISSIONS=/absolute/path/permissions.json
cosh
```

Existing guard-matched shell commands now use PDP authorization. This does not broaden
command matching or cover native file tools. Without this variable the existing behavior
remains. Once configured, an invalid permission file, unavailable daemon or failed
authorization blocks execution, without falling back to the legacy sandbox or unsandboxed execution.
Requests use `agent_type: "cosh"` and `agent_id: "cosh:<session_id>"`; Cedar must allow
that subject and the `/bin/bash -c` invocation.

The hook prepares authorization through the private `sandbox hook prepare` entry point.
ALLOW/MODIFY replace the command with its authorized execution entry point; STEP_UP uses
Cosh's native ask, and warnings become notifications. The daemon retains the complete
request and decision; the command carries only a decision ID. The accept phase of
`authorization.claim` accepts before expiry. Its execute phase transfers the invocation
once to the same UID without reevaluation. Approval executes the fixed permissions;
rejection abandons the handoff and reports non-execution.

Sandbox approval shows the final command indented on its own line, followed only
by the resource operations requiring confirmation and their policy messages. The
working directory is not displayed. Read and write use short labels; directory
access includes descendants, and write includes read, create, modify and remove.
Files and directories are distinguished using local path facts; an unavailable
path type is labelled as a path. Excluded subregions remain visible. Automatically
allowed permissions and independent warnings are not expanded in this prompt.
Execution-only confirmation shows the command and reason without resource items.
Approval still accepts the complete fixed invocation once, not individual grants.

Each item uses the matching Cedar `escalate` advice's `message`, preserving its
language. A missing message receives a generic confirmation reason; the machine
`reason` code remains in the decision instead of becoming user-facing prose.
Requirements retained from before a command transformation are labelled as reasons
from the original request, not permissions requested by the replacement command.
Older decisions without item provenance show the command and aggregate reason.
The approval template currently uses Chinese labels; Cosh's `ui.language` does not
translate Hook messages. Full paths and arguments are preserved and control
characters escaped. Cosh controls wrapping and navigation, so long or numerous
items can still exceed its approval panel.

Non-approval sandbox notifications retain their final command and directory because
another Hook may ask while Cosh's main preview shows the internal handoff command.
Warnings remain in decisions, receipts and non-approval notifications. A policy
adjustment without approval still shows the changed permission categories.

A sandbox warning does not itself request approval. Other enabled hooks can still
require it: for example, code-scanner can ask about recursive deletion even when
sandbox-guard allows the command with a warning. Keep those approvals enabled.

The authorization integration targets Cosh NG, which fires PostToolUse after both successful and failed tools.
Configure the sandbox hook as the only hook that rewrites the shell command. Stock Cosh
does not enforce that ordering. If another pre-tool hook blocks the command, Cosh NG
skips execution without notifying the sandbox hook. The next sandbox request in the
same run releases the slot. If the run ends first, the slot remains until the daemon
restarts. A user waiting for approval has no time limit. If a tool times out after claiming its decision and no
executor receipt arrived, the daemon records an unknown outcome and releases its slot.
Daemon restart invalidates in-memory handoffs. Abrupt termination can leave incomplete
handoffs subject to the existing per-UID capacity limit. Receipt failure never reruns a
completed target. Other Gateway/ACP execution paths are outside this integration.

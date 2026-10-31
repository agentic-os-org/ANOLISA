# Run commands with explicit sandbox permissions

[中文版](../../zh/agent-sec-core/sandbox.md)

`sandbox run` confines a command and its children to caller-supplied filesystem
and network permissions. It runs locally without a daemon or policy approval.
The permissions file is an execution input, not proof of PDP authorization.

## Requirements

Use the Rust V2 build described in the [component guide](index.md). This command
requires Linux, unprivileged user namespaces, bubblewrap 0.10 or newer, and the
statically linked `linux-sandbox` installed beside `agent-sec-cli`. A noexec rule
also requires working recursive `mount_setattr` support. Missing capabilities
stop execution; the command does not fall back to running on the host.

## Run a command

The following example permits host reads, writes only to `/tmp/asc-demo`, and
blocks IP networking and direct execution from that directory. The root read
grant is deliberately broad so this example includes runtime dependencies;
replace it with your application's explicit read paths for a restricted workload.

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

Paths must already exist. Files select one object; directories select their
subtrees. Do not use wildcards or a trailing slash. New files can be created
inside a writable directory. Include the executable, loader, libraries, working
directory and application data needed by the command; the runtime adds no grants.

| Field | Behavior |
|---|---|
| `filesystem.default` | Must be `deny`. |
| `rules[].effect` | `allow` or `deny`; deny wins regardless of rule order. |
| `rules[].rights` | `read` or `write`; write includes read, creation, deletion and rename. |
| `execution.deny_paths` | Mount-level noexec for named files or directory subtrees. |
| `network.mode` | `off` blocks IP networking; `full` uses host networking without endpoint filters. |

Read-only does not mean non-executable. noexec prevents direct execution through
the named mount; it does not prevent an allowed interpreter reading a script or
execution of a copy elsewhere. The backend rejects ambiguous deny aliases and
hardlinked deny targets instead of silently broadening permissions. Host file
permissions still apply. Socket stdio is rejected; caller-provided terminal,
pipe and regular-file stdio remain explicit communication channels. Other
inherited file descriptors and external Unix sockets are blocked.

The command inherits its environment and returns the target's exit status.
There is no execution timeout or active cancellation API; the PEP leaves the
caller's signal handling unchanged. Installation must finish within 30 seconds
after the helper starts. Setup or installation failure stops before the target
starts. Completion of the main command cleans up sandbox children. Parent exit
terminates the direct helper and the established sandbox process tree; cleanup
during bubblewrap initialization has a remaining race described in the design.
If an external signal terminates the caller, a final receipt is not guaranteed.

## Use policy authorization

Applications that need authorization use `asc-pep-runtime::PepRuntime` with
`DaemonAuthorizationClient`, then execute through the same backend. The runnable
[`authorized_sandbox` example](../../../../src/agent-sec-core/v2/crates/asc-pep-runtime/examples/authorized_sandbox.rs)
shows preparation, policy warnings before execution or ask, and receipts. This is separate from `sandbox run`.

Store Cedar policies through the existing `policy create/update` commands. The
policy file uses the source envelope shown in the [Policy guide](policy-cli.md).
Each successful update is available to the next authorization request.

Enable authorization with
`agent-sec-daemon --authorization-config /etc/agent-sec-core/authorization.json`.
This root-owned configuration selects PAP policy IDs and runtime limits; it does
not contain Cedar policy text:

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
`schema: {schemaId: "seccore.system", revision: 2}`. Baseline and task are evaluated
independently within the environment ceiling. Configuration and its parent
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
command locally and executes it; only missing preparation facts require DEFER
and another evaluate.
Policy `warn` advice adds `warnings` without requiring approval or adding a sixth
decision. The example displays warnings before execution or ask and includes them
in its JSON result. The daemon also records them with the execution receipt.
`authorization.report` records execution results in the existing SQLite/JSONL
security event sinks; a failed report never reruns a command. Without a configured
authorization configuration these RPCs return `unavailable`; direct `sandbox run` remains usable.

See the [implementation design](../../../../src/agent-sec-core/docs/design/LINUX_SANDBOX_PEP_DESIGN_zh.md)
for schemas, component responsibilities and validation coverage.

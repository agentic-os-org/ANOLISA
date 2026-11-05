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
overlapping explicitly mounted aliases and hardlinked files in denied ranges.
noexec paths must also be canonical. Host file permissions still apply.

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

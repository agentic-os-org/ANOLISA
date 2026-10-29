# Qoder CLI

[中文版](../../../zh/aw/agents/qoder.md)

Run source examples from `src/aw`; build AW with the [integration tutorial](../integration.md) first.

## Build and start Qoder

AW is not yet available through `anolisa install` or an RPM. Developers can
build it on Linux with rustup and the repository's pinned toolchain. Install
Qoder CLI separately and verify its version. From `src/aw`:

```bash
cargo build --locked -p aw-cli --bin aw
qodercli --version
target/debug/aw validate --config crates/aw-service/examples/aw.qoder.yaml
target/debug/aw run --config crates/aw-service/examples/aw.qoder.yaml --agent qoder
```

The [Qoder example](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.qoder.yaml)
uses `argv: [qodercli]`. To select a particular installation, replace that entry
with the absolute path of its executable. `--agent qoder` selects the named
entry under `spec.agents`; the name is yours to choose, while `adapter: qoder`
selects the framework.

The CLI requires SemVer ≥1.1.64 with bare version output. Integration requires `--settings`, literal `args` command Hooks, `PreToolUse`/`PostToolUse`, callback identity and exit-code-2 denial.

For example, name the target `qodercli` and select your installed executable:

```yaml
agents:
  qodercli:
    adapter: qoder
    argv: [/absolute/path/to/qodercli]
```

Merge that block under `spec` in your complete configuration, then run
`aw run --config /absolute/aw.yaml --agent qodercli`. The target name is separate
from the native executable name and version.

This example consumes Hook input and returns `{}` before and after tools. It
adds no restriction and is not a security policy. Ask Qoder to run a harmless
read-only tool to exercise both callbacks. A reply produced without a tool call
does not exercise them. Arguments after `--` go to Qoder, for example:

```bash
target/debug/aw run --config crates/aw-service/examples/aw.qoder.yaml --agent qoder -- -p 'Read the current directory name with a tool.'
```

AW opens Qoder's native terminal interface, then returns to the original shell
when Qoder exits. It releases that session's binding and unfinished work. The
shared daemon remains available to later sessions using the same configuration.

```bash
target/debug/aw status --config crates/aw-service/examples/aw.qoder.yaml
target/debug/aw stop --config crates/aw-service/examples/aw.qoder.yaml
```

## Keep existing Hooks and scheduling

Qoder continues to load its normal user, project and local settings. AW creates
session-specific callback entries without editing those files. To include an
existing JSON file that you normally pass through Qoder's `--settings`, use:

```bash
target/debug/aw run --config ./aw.yaml --agent qoder --native-settings ./qoder.settings.json
```

AW preserves that file's other fields and Hook entries, adds its callbacks and
passes the merged settings to Qoder. Each AW step becomes a separate synchronous
native Hook. Qoder determines serial or parallel execution, including how the
new callbacks coexist with other matching Hooks.

Set `spec.agents.qoder.qoder.sequential: true` to mark the generated groups as
sequential. If any matching synchronous Qoder group requests sequential
execution, all matching synchronous Hooks run in sequence. Omitting this field
or setting it to false does not override an existing group's true value.
Steps in one native event share the AW budget; later callbacks do not reset it.

This adapter supports scripts whose input remains unchanged across AW steps.
Sequential input-rewrite chains are not supported: if a command returns
`updatedInput`, a later callback with changed input is rejected and follows that
step's `on_error`. Forwarding native bytes does not promise every native effect
combination or approval flow.

The launcher rejects settings and arguments that would disable or replace its
Hooks rather than overriding a user's disabled-hook choice. This includes raw
`--settings`, `--setting-sources`, `--headless-fast-hooks` and conflicting native
settings. Custom configuration roots, alternate Qoder configuration-directory
modes, resumed sessions, remote sessions and worktree launch are outside the current adapter's
scope. Pass short options separately rather than combining them, and launch from
the intended working directory. Existing native Hooks remain
responsible for their own behavior and audit records.

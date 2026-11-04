# AW quickstart

[中文版](../../zh/aw/index.md)

AW adds tool safety checks, output compression and custom rules to your existing Agent.
This guide starts Qoder through AW, verifies a tool call and stops the local service.
Your Agent continues to manage its interface, model account and permissions.

## Before you begin

- Use Linux. The prebuilt CI packages require Linux x86_64 and glibc 2.39 or later, such as Ubuntu 24.04.
- Install Qoder CLI and sign in using its own instructions. The verified baseline is 1.1.64; `qodercli` must be on `PATH`.
- Run the steps as your ordinary user. Other Agents and their prerequisites are covered in [configuration and launch](configuration.md).

## 1. Install AW

Download the `aw-core` archive and matching `SHA256SUMS` from the intended successful
AW Integration run, then verify and extract it using the [installation guide](installation.md#get-a-package).
In the extracted `aw-core` directory, install the prebuilt CLI:

```bash
export AW_PREFIX="$HOME/.local"
make install PREFIX="$AW_PREFIX"
export PATH="$AW_PREFIX/bin:$PATH"
aw --version
```

`aw --version` should print the installed version. AW currently uses standalone
packages; `anolisa install aw` and RPM installation are not available yet.
If AW is already installed, set `AW_PREFIX` to its installation prefix and continue.

## 2. Create configuration

Keep configuration and runtime data in a private directory outside the installed payloads.
The configuration below starts Qoder with an empty policy; no Provider or backend is required.

```bash
export AW_HOME="$HOME/aw"
export AW_QODER="$(command -v qodercli)"
install -d -m 700 "$AW_HOME" "$AW_HOME/workspace"
qodercli --version
aw config init --prefix "$AW_PREFIX" \
  --config "$AW_HOME/aw.yaml" --state-dir "$AW_HOME/state" \
  --qoder "$AW_QODER"
aw config validate --config "$AW_HOME/aw.yaml"
```

`AW_QODER` must be an absolute executable path. `config init` creates `aw.yaml`
and refuses to overwrite an existing file. Validation should print `configuration valid`.

<a id="run-a-session"></a>

## 3. Launch and verify a tool call

```bash
cd "$AW_HOME/workspace"
aw run --config "$AW_HOME/aw.yaml" --agent qoder
```

Expect an `AW instance ...; service ...` message followed by Qoder's interface.
Ask Qoder to read the current directory with a tool, and follow its native permission prompts.
Confirm that the tool runs and its result appears. `qoder` is the target name generated in `aw.yaml`.

This first run verifies the launch path. An empty policy performs no security
checks or output compression. Continue with [Add capabilities](#add-capabilities) to enable those effects.

## 4. Stop when finished

Exit Qoder. The AW service remains available for later sessions, so inspect it and stop it explicitly:

```bash
aw service status --config "$AW_HOME/aw.yaml"
aw service stop --config "$AW_HOME/aw.yaml"
```

Exit all Agents sharing the configuration before stopping its service. Stopping retains
configuration and audit data. For startup errors, upgrades and cleanup, see [operations](operations.md).

<a id="choose-capabilities"></a>

## Add capabilities

Install the corresponding Provider and backend components, then configure the policy
and verify its effect with an actual tool call:

| Goal | Guide |
| --- | --- |
| Reduce tool-result context | [Use Tokenless](tokenless.md) |
| Check commands before execution | [Use sec-core](security.md) |
| Associate AgentSight records with a run | [Connect AgentSight](observability.md) |

Native permission checks remain in effect. AW's current Hook checks do not provide an OS sandbox.

<a id="start-here"></a>

## Next steps and reference

| Task | Guide |
| --- | --- |
| Choose another Agent, add rules or edit configuration | [Configuration and launch](configuration.md) |
| Look up subcommands, parameters, defaults and examples | [CLI reference](cli-reference.md) |
| Check status, stop, upgrade or troubleshoot | [Operations](operations.md) |
| Compare Hook, MCP and policy-update support | [Capability support matrix](capabilities.md) |
| Write Providers or integrate an SDK application | [Developer guide](../../../developer-guide/en/aw/index.md) |

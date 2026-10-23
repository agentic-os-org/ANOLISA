# AW user guide

[中文版](../../zh/aw/index.md)

AW adds tool safety checks, output compression and custom rules to your existing Agent.
Choose the Agent and capabilities in one `aw.yaml`, then launch it with `aw run`.
The framework continues to manage its interface, model account and permissions.

## Start here

| Task | Guide |
| --- | --- |
| Install AW and start your first Qoder session | [Installation and first run](installation.md) |
| Compile on your own Linux machine | [Build AW from source](../../../developer-guide/en/aw/building.md) |
| Choose an Agent, add rules or edit configuration | [Configuration and Agent launch](configuration.md) |
| Reduce the context used by tool results | [Use Tokenless](tokenless.md) |
| Check commands before execution | [Use sec-core](security.md) |
| Associate AgentSight records with a run | [Connect AgentSight](observability.md) |
| Check status, stop, upgrade or troubleshoot | [Operations](operations.md) |

AW is currently a Linux Preview distributed as standalone packages. `anolisa install aw`
and RPM installation are not available. Install and sign in to your selected Agent first;
AW does not install frameworks or configure model accounts.

## Run a session

After installation and configuration, launch from your working directory.

```bash
aw validate --config "$HOME/aw/aw.yaml"
aw run --config "$HOME/aw/aw.yaml" --agent qoder
```

`qoder` names a configured target. For other Agents, see [Configuration](configuration.md).

After the Agent exits, the AW service remains available for later sessions. Stop it when finished.

```bash
aw status --config "$HOME/aw/aw.yaml"
aw stop --config "$HOME/aw/aw.yaml"
```

## Choose capabilities

Core can launch an Agent independently. For safety checks or compression, install the
corresponding Provider or all-in-one, then enable it in configuration.

AW can observe or block calls before tools execute and compress supported text results afterwards.
Native permission checks remain in effect. Current Hook checks do not provide an OS sandbox.
For custom components and SDK applications, see the [developer guide](../../../developer-guide/en/aw/README.md).

# AW user guide

[中文版](../../../zh/user-entrypoint/aw/README.md)

AW adds tool safety checks, output compression and custom rules to your existing Agent.
Choose the Agent and capabilities in one `aw.yaml`, then launch it with `aw run`.
The framework continues to manage its interface, model account and permissions.

## Start here

| Task | Guide |
| --- | --- |
| Install AW and start your first Qoder session | [Installation and first run](installation.md) |
| Compile on your own Linux machine | [Build AW from source](../../../../developer-guide/en/aw/building.md) |
| Choose an Agent, add rules or edit configuration | [Configuration and Agent launch](configuration.md) |
| Reduce the context used by tool results | [Use Tokenless](tokenless.md) |
| Check commands before execution | [Use sec-core](security.md) |
| Associate AgentSight records with a run | [Connect AgentSight](observability.md) |
| Check status, stop, upgrade or troubleshoot | [Operations](operations.md) |

AW is currently a Linux Preview distributed as standalone packages. `anolisa install aw`
and RPM installation are not available. Install and sign in to your selected Agent first;
AW does not install frameworks or configure model accounts.

## Run a session

After installation and configuration, launch from the directory where the Agent should work.

```bash
aw validate --config "$HOME/aw/aw.yaml"
aw run --config "$HOME/aw/aw.yaml" --agent qoder
```

The first command checks the configuration; the second opens Qoder's native interface.
`qoder` names a target in the configuration. For other Agents, follow the
[configuration guide](configuration.md).

After the Agent exits, the AW service remains available for later sessions. Stop it when finished.

```bash
aw status --config "$HOME/aw/aw.yaml"
aw stop --config "$HOME/aw/aw.yaml"
```

## Choose capabilities

Core alone can launch an Agent. To enable safety checks or compression, install the corresponding
Provider and select it in the configuration. The all-in-one package installs the components together;
configuration still determines which capabilities run.

AW can observe or block calls before tools execute and compress supported text results afterwards.
Native permission checks remain in effect. Current Hook checks do not provide an OS sandbox.
For custom components and SDK applications, see the [developer guide](../../../../developer-guide/en/aw/README.md).

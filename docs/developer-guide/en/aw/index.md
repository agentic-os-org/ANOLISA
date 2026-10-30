# AW developer guide

[中文版](../../zh/aw/index.md)

For developers integrating AW into applications, deploying workers or implementing Providers. Start with the [user guide](../../../user-guide/en/aw/index.md) for installation and daily operation.

## Integrate applications and rules

- [Integration tutorial](integration.md) · Test allow, block and failure handling with a local script.
- [Agent / SDK integration](native-agents.md) · Select versions, factories and launch modes.
- [Custom Hooks](providers/hooks.md) · Boolean scripts and native commands.
- [Provider development](providers/index.md) · Protocol, sec-core, Tokenless and read-only views.

## Deploy and validate

- [Resident SDKs](deployment/resident.md) · Task factories, MCP and client cleanup.
- [Services and containers](deployment/lifecycle.md) · Persistence, shutdown, upgrades and capacity.
- [Policy updates](deployment/policy-updates.md) · Prewarm, drain, activate and recover.
- [External process attachment](deployment/process-attach.md) · Attach before the first request and remove.
- [Native behavior validation](validation.md) · Tool outcomes and audit evidence.
- [Local service demo](local-service.md) · Socket and audit checks without a model.

## References

[Configuration fields](configuration.md) · [Configuration editing](reference/configuration-editor.md) · [CLI automation](reference/cli.md) · [Packages](reference/packages.md) · [AgentSight](agentsight.md)

Follow [contributing](../../../../src/aw/CONTRIBUTING.md) to build, test and submit AW implementation changes.
Architecture decisions and protocol invariants live in the [design index](../../../../src/aw/docs/design/en/README.md).

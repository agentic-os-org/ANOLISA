# AW developer guide

[中文版](../../zh/aw/index.md)

For developers integrating AW into applications, deploying workers or implementing Providers. Start with the [user guide](../../../user-guide/en/aw/index.md) for installation and daily operation.

## Integrate applications and rules

- [Integration tutorial](integration.md) · Test allow, block and failure handling with a local script.
- [Agent / SDK integration](native-agents.md) · Select versions, factories and launch modes.
- [Runtime SDK packages](deployment/native-sdk.md) · Dual-language core/adapter packages, `LanguageSource` zero-adapter instrumentation, Descriptors and cooperative PID attach.
- [Python SDK](deployment/python-sdk.md) · `anolisa-aw-sdk`, `aw_sdk.sources`, QwenPaw and Claude worker integration.
- [Provider development](providers/index.md) · Minimal 3-method `aw-provider/v1alpha1` skeleton, `aw.yaml` wiring, sec-core, Tokenless and read-only views.
- [Custom Hooks](providers/hooks.md) · Boolean scripts and native commands.

## Deploy and validate

- [Independent instances](deployment/independent-instances.md) · Prepare native startup, confirm Ready, update and release application-owned instances.
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

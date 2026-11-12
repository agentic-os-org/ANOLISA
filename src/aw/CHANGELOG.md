# Changelog

[中文版](CHANGELOG_zh.md)

## [0.1.1] - 2026-11-12

### Added

- **AW**: Updated to v0.1.1, manage prepared native instances across eight
  Adapters and apply policy updates after their work drains.
- **AW**: Updated to v0.1.1, export bounded public audit and runtime facts so
  applications can inspect instance state and completed work.
- **AW**: Updated to v0.1.1, let supported before-tool Providers replace checked
  tool input before native authorization and execution.
- **AW**: Updated to v0.1.1, let the sec-core Provider wait for its backend
  socket within the check budget during parallel startup
  ([#9881](https://github.com/agentic-os-org/ANOLISA/pull/9881)).

### Changed

- **AW**: Updated to v0.1.1, shorten ordinary QwenPaw startup with private
  Registry registration and recognize generated pip/uv console scripts
  ([#9959](https://github.com/agentic-os-org/ANOLISA/pull/9959)).
- **AW**: Updated to v0.1.1, prepare native startup and idle policy updates for
  Qoder CLI, OpenClaw, Hermes, Pi, Claude SDK and AgentScope integrations.
- **AW**: Updated to v0.1.1, wake shared Service connections when readable to
  reduce control-call latency across Adapters
  ([#9837](https://github.com/agentic-os-org/ANOLISA/pull/9837)).

### Fixed

- **AW**: Updated to v0.1.1, preserve native veto and expired-event failure
  actions, keeping denied or timed-out work from continuing unchecked.
- **AW**: Updated to v0.1.1, allow new-prefix Provider installs alongside
  retained 0.1.0 records in the shared database, preserving the rollback copy
  ([#9763](https://github.com/agentic-os-org/ANOLISA/pull/9763)).

### Support and deployment

Linux supports the eight Adapters in the [Hook matrix](../../docs/user-guide/en/aw/capabilities.md#hook-support-matrix).
Seven support checked `tool.before` input replacement; OpenClaw does not.
`tool.after` replacement remains limited to authorized text slots, and native
permission checks retain precedence. Managed MCP and idle policy update boundaries
vary by Adapter and launch mode; see the [support table](../../docs/user-guide/en/aw/capabilities.md#mcp-and-policy-updates).
Local Skill checks use the supported sec-core Provider paths described in the
[Skill reference](../../docs/user-guide/en/aw/capabilities.md#skill-checks).
Deploy the selected Provider and backend separately, prepare the native startup
route, and enable policies in AW configuration. Preserve the previous 0.1.0
packages and configuration for rollback.

## [0.1.0] - 2026-11-07

### Added

- **AW**: First release v0.1.0, apply one policy configuration across native
  Agent integrations, with shared audit, cancellation and instance cleanup
  ([#9222](https://github.com/agentic-os-org/ANOLISA/pull/9222)).
- **AW**: Connect user submission and tool Hooks to structured Providers, scripts
  and native commands. Submission and before-tool checks can block execution;
  structured after-tool Providers can replace authorized result text.
- **AW**: Provide sec-core and Tokenless bridges as separate packages. sec-core
  checks prompt injection, input/output PII, code and supported local Skills;
  Tokenless compresses tool results and suggests command retries.
- **AW**: Run multiple configured Agents against one local service, stop selected
  instances, and update supported resident policies when their tasks have drained.
- **AW**: Preserve native package ownership through staged Makefile installation
  and provide a local bundle installer for independently packaged Providers.

### Support matrix

Linux is supported. The following matrix describes ordinary launch modes;
restricted reloadable modes have additional admission requirements.

| Adapter | Before-tool observe / block | After-tool observe / text replacement | Submission observe / block |
| --- | --- | --- | --- |
| AgentScope | Supported | Supported | Supported |
| Claude SDK | Supported | Supported | Supported |
| Hermes | Supported | Supported | Supported |
| OpenClaw | Supported | Supported | Unavailable |
| Pi | Supported | Supported | Supported |
| Qoder CLI | Supported | Supported | Supported |
| Qoder SDK | Supported | Supported | Supported |
| QwenPaw | Supported | Supported | Supported |

Submission checks cover the integrated text ingress of each Adapter. Result
replacement updates integrated text slots while preserving native metadata and
failure state. Qoder CLI Post checks cover successful tool calls. Skill checks
cover Qoder SDK, QwenPaw, AgentScope and Hermes local sources, plus Pi skill
submissions. Qoder SDK, QwenPaw and AgentScope support idle policy updates in their
ordinary resident/factory modes.

See the [capability reference](../../docs/user-guide/en/aw/capabilities.md) for
MCP, native configuration, private credentials and mode-specific limits, and the
[native test matrix](tests/native-matrix.json) for reproducible version baselines.

### Deployment

- Configure frameworks, application factories, models and backend services,
  then install Providers and enable policies through configuration.
- Integrate Providers independently through the process protocol.
- Upgrade preview deployments together with their Provider paths and policy
  configuration; retain the previous artifacts and configuration for rollback.

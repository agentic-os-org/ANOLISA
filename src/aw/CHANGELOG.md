# Changelog

[中文版](CHANGELOG_zh.md)

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

Submission checks cover the admitted text ingress of each Adapter. Result
replacement preserves native metadata and failure state and requires a supported
text slot. Qoder CLI has no failed-tool Post callback integration. Skill checks
cover Qoder SDK, QwenPaw, AgentScope and Hermes local sources, plus Pi skill
submissions. Qoder SDK, QwenPaw and AgentScope support idle policy updates in their
ordinary resident/factory modes.

See the [capability reference](../../docs/user-guide/en/aw/capabilities.md) for
MCP, native configuration, private credentials and mode-specific limits, and the
[native test matrix](tests/native-matrix.json) for reproducible version baselines.

### Deployment

- Frameworks, application factories, model configuration and backend services
  are supplied by the deployment. Provider installation is followed by explicit
  configuration and activation.
- Security result processing runs after tool execution; it controls delivery and
  cannot undo tool side effects. OS isolation remains a deployment concern.
- Provider process integration is available independently of Rust. The bundled
  Rust Providers currently build against repository-local AW libraries.
- Upgrade preview deployments together with their Provider paths and policy
  configuration; retain the previous artifacts and configuration for rollback.

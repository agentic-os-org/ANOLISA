# Changelog

[中文版](CHANGELOG_zh.md)

## [0.2.0] - 2026-11-20

### Added

- **AW**: Updated to v0.2.0, ship Python and TypeScript runtime SDKs with
  optional Claude, QwenPaw, Pi and Qoder adapters, so applications can install the
  integration they use and keep their native startup and configuration
  ([#10211](https://github.com/agentic-os-org/ANOLISA/pull/10211),
  [#10325](https://github.com/agentic-os-org/ANOLISA/pull/10325)).
- **AW**: Updated to v0.2.0, compile target-specific startup hints offline with
  `aw config descriptor`. SDK applications can use one policy configuration for
  multiple targets and validate a matching descriptor before installing callbacks
  ([#10244](https://github.com/agentic-os-org/ANOLISA/pull/10244)).
- **AW**: Updated to v0.2.0, connect application-owned Claude and Pi workers before
  protected work and support background attachment for the pinned Hermes 0.23.1
  Gateway. Resident integrations keep work bound to the active policy until
  callbacks and cleanup drain
  ([#10290](https://github.com/agentic-os-org/ANOLISA/pull/10290),
  [#10304](https://github.com/agentic-os-org/ANOLISA/pull/10304)).
- **AW**: Updated to v0.2.0, add native Codex and DeepSeek Harness profiles and
  admit final result checks on the verified Qoder SDK, Claude SDK, OpenClaw and
  DeepSeek Harness profiles. Exact versions, transports and covered result types
  are listed in the [capability reference](../../docs/user-guide/en/aw/capabilities.md#hook-support-matrix).

### Changed

- **AW**: Updated to v0.2.0, unify live connection, policy updates and service
  control under the commands below. Update scripts that use retired command
  spellings before switching binaries
  ([#10279](https://github.com/agentic-os-org/ANOLISA/pull/10279)).
- **AW**: Updated to v0.2.0, deliver the six SDK artifacts from one revision with
  an `aw-sdk-candidate/v2` receipt. The four adapter packages require the matching
  0.2.0 core SDK. Existing SDK import paths forward to the installed adapter
  ([#10325](https://github.com/agentic-os-org/ANOLISA/pull/10325)).
- **AW**: Updated to v0.2.0, support decision, constrained mutation and lifecycle
  events in the Provider contract. Native execution requires an admitted
  integration for the particular Hook and effect
  ([#10321](https://github.com/agentic-os-org/ANOLISA/pull/10321)).

### Fixed

- **AW**: Updated to v0.2.0, restore checked text replacement for supported
  QwenPaw tool results before stream and model delivery. Required check failures
  withhold the original result
  ([#10201](https://github.com/agentic-os-org/ANOLISA/pull/10201)).
- **AW**: Updated to v0.2.0, correlate Hermes tool output with the originating
  call so the active policy checks the matching result
  ([#10304](https://github.com/agentic-os-org/ANOLISA/pull/10304)).

### Support and deployment

Linux supports [AW-managed launch, prepared native startup, and application-owned SDK integration](../../docs/developer-guide/en/aw/deployment/lifecycle.md).
Consult the [Hook matrix](../../docs/user-guide/en/aw/capabilities.md#hook-support-matrix)
for observation, blocking, checked input replacement, and text result replacement
by Agent and mode. The framework-free `sdk/application` profile supports input and
before-tool observation and blocking, plus after-tool observation. See the
[Skill checks](../../docs/user-guide/en/aw/capabilities.md#skill-checks) and
[MCP and policy updates](../../docs/user-guide/en/aw/capabilities.md#mcp-and-policy-updates)
for supported sources, managed MCP paths and the idle boundary for each mode.

| Previous entrypoint | 0.2.0 entrypoint |
| --- | --- |
| `aw agent connect` or `aw install --config FILE --agent TARGET` | `aw connect --config FILE --agent TARGET` |
| `aw instance attach` | `aw connect` with native `--agent` and `--pid`, or SDK selection |
| `aw instance attach --remove` or `aw instance detach` | `aw disconnect` with the native target and PID, or `--sdk-instance` |
| `aw instance update` | `aw update` |
| `aw serve`, `aw status`, `aw stop` | `aw service serve`, `aw service status`, `aw service stop` |
| `aw validate` | `aw config validate` |

Keep application runtimes and credentials in the native environment. Install the
selected Provider bridges with the matching AW version and provision their
backends separately. Stop affected instances before upgrading, validate the
configuration with the new binary, regenerate any deployment descriptors, and
restart the service and applications through the selected entrypoints. Keep
the previous binary, Provider and SDK packages, configuration and launcher
scripts together for rollback.

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

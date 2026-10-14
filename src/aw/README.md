# AW

[中文版](README_zh.md)

AW provides a shared configuration and local service for Agent policies. On
Linux, it starts Qoder CLI >=1.1.64, OpenClaw, QwenPaw, Hermes and explicit Claude SDK workers through supported native
entrypoints, runs external Providers and retains execution metadata after Agent
sessions end. Frameworks keep their native interfaces, model configuration,
scheduling and permissions. It also manages public Pi SDK workers. The current release is a Preview.

## Start here

| Task | Guide |
| --- | --- |
| Install prebuilt core and optional sec-core packages | [Preview installation](../../docs/user-guide/en/user-entrypoint/aw-preview.md) |
| Build from source and connect your component | [Integration walkthrough](../../docs/developer-guide/en/aw/integration.md) |
| Write and validate configuration | [Configuration reference](../../docs/developer-guide/en/aw/configuration.md) |
| Launch four frameworks with one policy, verify and clean up | [User guide](../../docs/user-guide/en/user-entrypoint/aw.md#use-one-configuration-for-four-frameworks) |
| Compress model-visible results across six frameworks, including Pi and Claude SDK | [Tokenless Provider](../../docs/user-guide/en/user-entrypoint/aw-tokenless.md) |
| Use an existing sec-core backend | [sec-core Provider](../../docs/user-guide/en/user-entrypoint/aw-sec-core.md) |
| Deploy in a container and retain audit data | [Runtime and persistence](../../docs/user-guide/en/user-entrypoint/aw.md#containers-and-persistent-data) |

AW is not yet distributed through `anolisa install` or RPM. Preview package
installation uses `aw-package`; it does not install an Agent or its model
credentials. Core works without sec-core or any Provider. The source walkthrough
includes a [shared YAML](crates/aw-service/examples/aw.shared.yaml) and a small
[boolean policy](crates/aw-service/examples/check-tool.py) that need no backend.

## Supported native entrypoints

| Framework | Native prerequisite | Entrypoint |
| --- | --- | --- |
| Qoder CLI | Native command Hook contract; detected version recorded | Interactive or print |
| OpenClaw | 2026.9.6 | A new Gateway |
| QwenPaw / AgentScope | 2.2.2b4 / 2.0.8 | App/API |
| Hermes | Official revision `952c941e` | Local chat after explicit plugin installation |
| Pi Coding Agent / Core | 0.84.2 / 0.84.2 | Public SDK factory in an AW-owned foreground Node worker |
| Claude Agent SDK | 0.2.88 / bundled CLI 2.1.161 | Foreground Python worker using its public AW client factory |

Runtime supports tool-before `observe`/`block` and tool-after `observe` or single-text
`replace_result`, plus
framework-specific native commands. Completion observers and user Hooks can
precede one final replacement step (OpenClaw observes natively after compression); the combined sec-core/Tokenless template
retains scanning/observation and adds Tokenless PreTool retry suggestions before
execution plus final PostTool compression. Suggested commands require a fresh call
and pass sec-core/native permissions again; RTK is an external PreTool dependency.
RTK ownership survives quoted arguments, symlinked state paths and the execution
prefix forms listed in the user guide. After-event coverage varies by framework.
If OpenClaw lacks the replacement middleware API, startup reports degraded
compression while retaining before enforcement and completion observation.
The configuration vocabulary includes more events/effects than the runtime
supports; static validation alone does not certify native adoption. Cross-Agent
approval, input rewriting and OS enforcement are outside this Preview.
See the user guide for each Adapter's profile requirements and coexistence.
Pi uses an application factory module with `adapter: pi` and shares PreTool retry
reasons and PostTool single-text replacement. Its per-invocation UUID pairs both
hooks while preserving the SDK session and original tool ID; see the
[SDK launch guide](../../docs/user-guide/en/user-entrypoint/aw.md#start-a-pi-sdk-worker).

## Basic use

From the repository root on Linux with rustup and Qoder CLI available:

```bash
cd src/aw
cargo build --locked -p aw-service --bin aw
target/debug/aw validate --config crates/aw-service/examples/aw.qoder.yaml
target/debug/aw run --config crates/aw-service/examples/aw.qoder.yaml --agent qoder
```

This neutral example connects Hooks without security rules. After exiting Qoder:

```bash
target/debug/aw status --config crates/aw-service/examples/aw.qoder.yaml
target/debug/aw stop --config crates/aw-service/examples/aw.qoder.yaml
```

Initialize Qoder/OpenClaw YAML with `aw-package configure`; omit `--provider`
for an empty policy, or select `--provider command` / `--provider sec-core` / `--provider tokenless` /
`--provider sec-core+tokenless` (with `--socket`).
For four targets, copy the shared template. Simple scripts omit `protocol` and
`operation`, return JSON true/false and declare matched effects in each YAML
step. AW validates admission and constructs results without script handshake
methods. Follow the [initialization and editing flow](../../docs/user-guide/en/user-entrypoint/aw-preview.md#edit-existing-provider-and-hook-configuration)
before adding rules. `aw-package config` views,
validates and edits Providers and Hook steps in existing YAML/JSON offline.
Neither edits a running service; exit Agents and stop the original service
before changing its configuration. `aw install` is the separate, explicit
Hermes native-plugin installation command; it does not install software.

## Development references

`aw inspect --config ABS_FILE --json` returns a bounded, read-only Panel snapshot
without starting AW or executing tool Hooks. Explicit optional view registration
enables Tokenless statistics; unknown admission/readiness stays unknown. See the
[Tokenless guide](../../docs/user-guide/en/user-entrypoint/aw-tokenless.md#read-only-panel-queries)
and the [presentation contract](docs/design/provider-views.md).

Component integrations normally use the Provider protocol; they do not need an
AW crate dependency. Adapter and service-client developers can use the detailed
contracts below. Configuration, Provider messages and local service messages
are independently versioned.

- [Development environment, crate responsibilities and tests](CONTRIBUTING.md)
- [Provider protocol](docs/design/provider-protocol.md) and [Host](docs/design/provider-host.md)
- [Local service and client](docs/design/local-service.md)
- [Bounded command execution](docs/design/bounded-execution.md)
- [Core execution and durable journal](docs/design/core-execution.md)

- [AgentSight lifecycle and phase-one contracts](docs/design/agentsight-integration.md)

Claude SDK workers explicitly compose `aw_claude.create_client(options)`; see the
[factory and launcher guide](../../docs/user-guide/en/user-entrypoint/aw.md#start-a-claude-sdk-worker)
and [Adapter contract](docs/design/claude-sdk-adapter.md).

Claude SDK additionally supports Tokenless Bash retry suggestions and stdout compression; MCP results remain observation-only.

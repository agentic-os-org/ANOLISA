# sec-core Provider

[中文版](README_zh.md)

Bridge public sec-core code, PII and Skill checks into AW. This directory owns
the bridge; the corresponding component supplies the backend.

Select code tools with each Hook step's `match.tools`, using standard names such
as `bash` or exact `native:<adapter>:<name>` selectors. Provider `config.tools`
maps normalized `event.tool.name` values to inputs.
A selected unmapped tool returns an error by default;
`unmapped_tool: observe` explicitly enables observation without scanning.
Required security steps use `mode: block` and `on_error: block` to reject risks
and failed checks.

The shipped Qoder example selects native `Bash` in both phases. Unlisted tools
remain native and are recorded as skipped. SDK mappings and extraction rules
are listed in the [configuration guide](../../docs/developer-guide/en/aw/providers/sec-core.md#configure-a-provider).

Ordinary resident Qoder SDK and QwenPaw launches also check submitted user text
with `input.submit`, `scan_prompt` and `scan_user_input`. Configure prompt and
PII blocking with `on_error: block` to stop injection risks, sensitive user text and failed checks
before a model request. See the [submission configuration](../../docs/developer-guide/en/aw/providers/sec-core.md#user-submission-checks).

Qoder SDK and QwenPaw can also check their native `Skill` tool before loading
instructions. Enable `config.skill: true` and select `check_skill` on
`tool.before`; see [Skill checks](../../docs/developer-guide/en/aw/providers/sec-core.md#validate-skills-before-loading)
for directory authorization, trust states and supported local sources.

Maintained by @kongche-jbw under [CODEOWNERS](../../.github/CODEOWNERS).
Bridge code uses [Apache-2.0](../../LICENSE); backend source is
[agent-sec-core](../../src/agent-sec-core/README.md).

Use [the tool PII example](examples/aw.tool-pii.yaml) for Qoder SDK or QwenPaw.
`scan_tool_input` blocks sensitive arguments; `scan_tool_output` replaces a risky
text slot with a fixed safety message. Required output checks use
`on_error: withhold_result`; scan failures remain distinct from risk.

- [Configuration and operation](../../docs/developer-guide/en/aw/providers/sec-core.md)
- [Configuration example](examples/aw.yaml)
- [Contribution and distribution](../README.md)

Build and test from the repository root:

```bash
make -C providers/agent-sec-core build
make -C providers/agent-sec-core install PREFIX=/usr/local DESTDIR="$PWD/target/aw-root"
cargo test --locked --manifest-path providers/agent-sec-core/Cargo.toml
```

The Cargo package and binary remain `aw-provider-sec-core`. For independent archive
distribution, create selected packages with the
[build guide](../../contribute/en/aw/building.md).

Installation copies only the prebuilt bridge, examples, private schemas where present,
and documents. It does not install or start the backend. Omit `DESTDIR` for a direct
file installation with the caller's permissions; `AWPRJ` optionally namespaces private
paths. Runtime configuration uses the resulting absolute `transport.argv`.

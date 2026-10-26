# sec-core Provider

[中文版](README_zh.md)

Bridge the public sec-core CLI into AW. This directory owns the bridge; the
corresponding component supplies the backend.

Select code tools with each Hook step's `match.tools`, using standard names such
as `bash` or exact `native:<adapter>:<name>` selectors. Provider `config.tools`
maps normalized `event.tool.name` values to inputs.
A selected unmapped tool returns an error by default;
`unmapped_tool: observe` explicitly enables observation without scanning.
Required security steps use `mode: block` and `on_error: block` to reject risks
and failed checks.

Maintained by @kongche-jbw under [CODEOWNERS](../../.github/CODEOWNERS).
Bridge code uses [Apache-2.0](../../LICENSE); backend source is
[agent-sec-core](../../src/agent-sec-core/README.md).

- [Configuration and operation](../../docs/developer-guide/en/aw/providers/sec-core.md)
- [Configuration example](examples/aw.yaml)
- [Contribution and distribution](../README.md)

Build and test from the repository root:

```bash
cargo build --locked --manifest-path providers/agent-sec-core/Cargo.toml
cargo test --locked --manifest-path providers/agent-sec-core/Cargo.toml
```

The Cargo package and binary remain `aw-provider-sec-core`. For AW Preview
distribution, create matching packages with the
[build guide](../../contribute/en/aw/building.md).

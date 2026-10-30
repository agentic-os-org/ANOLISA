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

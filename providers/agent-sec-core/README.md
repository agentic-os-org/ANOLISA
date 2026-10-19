# sec-core Provider

[中文版](README_zh.md)

Bridge the public sec-core CLI into AW. This directory owns the bridge; the
corresponding component supplies the backend.

Maintained by @kongche-jbw under [CODEOWNERS](../../.github/CODEOWNERS).
Bridge code uses [Apache-2.0](../../LICENSE); backend source is
[agent-sec-core](../../src/agent-sec-core/README.md).

- [Configuration and operation](../../docs/developer-guide/en/aw/sec-core.md)
- [Configuration example](examples/aw.yaml)
- [Contribution and distribution](../README.md)

Build and test from the repository root:

```bash
cargo build --locked --manifest-path providers/agent-sec-core/Cargo.toml
cargo test --locked --manifest-path providers/agent-sec-core/Cargo.toml
```

The Cargo package and binary remain `aw-provider-sec-core`. For AW Preview
distribution, create matching packages with the
[build guide](../../docs/developer-guide/en/aw/building.md).

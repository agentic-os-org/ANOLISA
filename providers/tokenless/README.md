# Tokenless Provider

[中文版](README_zh.md)

Bridge the public Tokenless CLI into AW. This directory owns the bridge; the
corresponding component supplies the backend.

Maintained by @kongche-jbw under [CODEOWNERS](../../.github/CODEOWNERS).
Bridge code uses [Apache-2.0](../../LICENSE); backend source is
[tokenless](../../src/tokenless/README.md).

- [Configuration and operation](../../docs/developer-guide/en/aw/tokenless.md)
- [Configuration example](examples/aw.yaml)
- [Contribution and distribution](../README.md)

Build and test from the repository root:

```bash
cargo build --locked --manifest-path providers/tokenless/Cargo.toml
cargo test --locked --manifest-path providers/tokenless/Cargo.toml
```

The Cargo package and binary remain `aw-provider-tokenless`. For AW Preview
distribution, create matching packages with the
[build guide](../../docs/developer-guide/en/aw/building.md).

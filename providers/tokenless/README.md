# Tokenless Provider

[中文版](README_zh.md)

Bridge the public Tokenless CLI into AW. This directory owns the bridge; the
corresponding component supplies the backend.

Maintained by @kongche-jbw under [CODEOWNERS](../../.github/CODEOWNERS).
Bridge code uses [Apache-2.0](../../LICENSE); backend source is
[tokenless](../../src/tokenless/README.md).

- [Configuration and operation](../../docs/developer-guide/en/aw/providers/tokenless.md)
- [Configuration example](examples/aw.yaml)
- [Contribution and distribution](../README.md)

Build and test from the repository root:

```bash
make -C providers/tokenless build
make -C providers/tokenless install PREFIX=/usr/local DESTDIR="$PWD/target/aw-root"
cargo test --locked --manifest-path providers/tokenless/Cargo.toml
```

The Cargo package and binary remain `aw-provider-tokenless`. For independent archive
distribution, create selected packages with the
[build guide](../../contribute/en/aw/building.md).

Installation copies only the prebuilt bridge, examples, private schemas where present,
and documents. It does not install or start the backend. Omit `DESTDIR` for a direct
file installation with the caller's permissions; `AWPRJ` optionally namespaces private
paths. Runtime configuration uses the resulting absolute `transport.argv`.

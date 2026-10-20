# AW Rust SDK

[中文版](README_zh.md)

`aw-sdk` gives Rust applications typed access to sec-core code scanning and
Tokenless operations through one dependency. Each component owns its client or
Runtime; the SDK does not start system services or register Agent Hooks.

See the [SDK guide](../../../../docs/developer-guide/en/aw/sdk.md) for configuration,
examples and the component feature matrix. The SDK is an independent workspace;
it is not a tenth AW framework crate.

From this directory:

```bash
cargo test --locked
cargo doc --no-deps --locked
```

The repository gate, `python3 src/aw/scripts/check.py`, also tests each component
selection and rejects imports of AW Service or concrete Provider implementations.

Public source archives include the complete local library closure; use
`python3 src/aw/scripts/export_sdk.py --output src/aw/target/aw-sdk-source.tar.gz`
from the repository root (Python 3.11+). The gate builds an external consumer
without needing OS implementation packages.

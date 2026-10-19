# Provider development and distribution

[中文版](../../zh/aw/providers.md)

Providers connect capabilities such as safety checks and compression to AW through
a process protocol. They can be developed, distributed, and upgraded independently,
or contributed to the [Provider directory](../../../../src/aw/providers/README.md).

## Directories and responsibilities

```text
src/aw/
├── crates/                  # AW framework
│   └── aw-provider/         # Protocol and capability admission library
└── providers/               # Implementations and collaboration
    ├── sec-core/            # Security Provider
    └── tokenless/           # Compression Provider
```

`aw-provider` is the protocol library shared by the Host and implementations.
Each directory under `providers/` owns a concrete integration, including its
program, schemas, examples, and tests. Existing binaries retain the names
`aw-provider-sec-core` and `aw-provider-tokenless`, preserving packages and commands.

## Integrate independently

1. Implement `describe`, `validate_config`, and `invoke` using JSON over stdin/stdout.
   See the [Provider protocol](../../../../src/aw/docs/design/provider-protocol.md)
   for fields, errors, and capability declarations.
2. Distribute the executable and dependencies through your own release process.
   Set `transport.argv` to its installed absolute path. Document backend credentials,
   sockets, and data directories in the Provider's installation instructions.
3. Declare the Provider and Hook steps in configuration. Verify allow, block,
   timeout, error, and cleanup behavior. Start with the [integration tutorial](integration.md)
   or [minimal Rust example](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-provider/examples/policy.rs).

Providers return candidate effects; the Adapter adopts them in the native framework.
An implementation does not need to link AW or use Rust. The native framework
continues to manage the user's Agent and model configuration.

## Contribute an implementation

Submit under `providers/<provider>/` with a maintainer, source and license,
bilingual instructions, and offline tests. Rust packages join the workspace and
[layout checks](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/scripts/check_layout.py). Wire new tests into
the AW gate; see the [contribution contract](../../../../src/aw/providers/README.md).

Build and test the first-party Providers separately from `src/aw`:

```bash
cargo build --locked -p aw-provider-sec-core -p aw-provider-tokenless
cargo test --locked -p aw-provider-sec-core -p aw-provider-tokenless
```

`aw-build` and `aw-package` currently package and install built-in components only;
they do not automatically install arbitrary community Providers. Independent
installation works with the same protocol. Bundling requires package manifest
and installation acceptance support.

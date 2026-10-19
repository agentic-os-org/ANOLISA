# Provider development and distribution

[中文版](../../zh/aw/providers.md)

Providers connect capabilities such as safety checks and compression to AW through
a process protocol. They can be developed, distributed, and upgraded independently,
or contributed to the [Provider directory](../../../../providers/README.md).

## Directories and responsibilities

```text
ANOLISA/
├── src/aw/crates/           # AW framework
│   └── aw-provider/         # Shared protocol and admission
└── providers/               # Independent implementation workspaces
    ├── agent-sec-core/
    └── tokenless/
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
bilingual instructions, and offline tests. Each Rust package owns its workspace and lockfile and follows the
[layout checks](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/scripts/check_layout.py). Wire new tests into
the AW gate; see the [contribution contract](../../../../providers/README.md).

Build and test the first-party Providers from the repository root:

```bash
cargo build --locked --manifest-path providers/agent-sec-core/Cargo.toml
cargo test --locked --manifest-path providers/agent-sec-core/Cargo.toml
cargo build --locked --manifest-path providers/tokenless/Cargo.toml
cargo test --locked --manifest-path providers/tokenless/Cargo.toml
```

The bundled workspaces currently use repository-local AW SDK paths; Tokenless
composition tests also depend on the sec-core bridge. Independent SDK publication
and artifact-based composition are deferred. Existing Preview packages retain
their matching-version and matching-source installation rules.

`aw-build` and `aw-package` currently package and install built-in components only;
they do not automatically install arbitrary community Providers. Independent
installation works with the same protocol. Bundling requires package manifest
and installation acceptance support.

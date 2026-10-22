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

## OS distribution design

The component owner maintains the implementation, tests, dependencies and release.
The OS distributor selects versions, verifies origin and composition compatibility,
and delivers installation packages and upgrade paths. Sharing a repository does
not require a shared release cycle.

First-party Providers should build, test and release outside the repository using
pinned public AW libraries, with backend dependencies managed by their owners.
Third parties can implement the public process protocol and connect through YAML,
without linking Rust libraries or checking out ANOLISA. The distribution layer
consumes component artifacts and validates bundled dependencies together.

See [architecture boundaries](../../../../src/aw/docs/design/en/architecture-boundaries.md)
for ownership and migration scope.

## Integrate independently

1. Implement `describe`, `validate_config`, and `invoke` using JSON over stdin/stdout.
   See the [Provider protocol](../../../../src/aw/docs/design/en/providers/provider-protocol.md)
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

The bundled workspaces currently use repository-local AW library paths; Tokenless
composition tests also depend on the sec-core bridge. These implementations cannot
yet build outside the repository. Pinned SDKs, independent releases and artifact
composition tests are deferred. Preview delivers core and Providers as one build
set, with installation checking matching versions and source commits.

`aw-build` and `aw install` currently package and install built-in components only;
they do not automatically install arbitrary community Providers. Independent
installation works with the same protocol. Bundling requires package manifest
and installation acceptance support.

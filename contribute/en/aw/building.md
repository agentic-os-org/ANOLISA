# Build AW from source

[中文版](../../zh/aw/building.md)

Build AW and its Provider bridges on Linux x86_64 or aarch64. For prebuilt
archives, see [Install AW](../../../docs/user-guide/en/aw/installation.md).

## Build environment

Install [rustup](https://www.rust-lang.org/tools/install), the pinned Rust toolchain
from each workspace's `rust-toolchain.toml`, Python 3.11+, and Node.js for the AW
checks. On Ubuntu / Debian, prepare the compiler dependencies:

```bash
sudo apt-get update
sudo apt-get install -y git build-essential pkg-config libssl-dev python3
```

Uncached Cargo dependencies need network access. Build for the destination's
architecture and compatible glibc. Backend components have their own build
requirements and are not built by the AW or Provider targets.

## Build and install components

From the repository root, build only the components you need:

```bash
make -C src/aw build
make -C providers/agent-sec-core build
make -C providers/tokenless build
```

Each target builds its own release binary using its locked Cargo workspace.
`CARGO_TARGET_DIR` selects the output directory; the default is that component's
`target/`. Installation consumes those prebuilt files without rebuilding:

```bash
make -C src/aw install PREFIX=/usr/local DESTDIR="$PWD/target/aw-root"
make -C providers/agent-sec-core install PREFIX=/usr/local DESTDIR="$PWD/target/aw-root"
make -C providers/tokenless install PREFIX=/usr/local DESTDIR="$PWD/target/aw-root"
```

`DESTDIR` adds a temporary root; omit it to install into `PREFIX` with the caller's
permissions. No target invokes `sudo`, creates an AW package record, registers
Hooks or starts services. `BINARY` may select an already built executable.

| Component | Files below `PREFIX` |
| --- | --- |
| AW | `bin/aw`; `share/doc/aw/` |
| sec-core Provider | `libexec/aw/providers/sec-core/aw-provider-sec-core`; `share/aw/providers/sec-core/{schemas,examples}/`; `share/doc/aw-provider-sec-core/` |
| Tokenless Provider | `libexec/aw/providers/tokenless/aw-provider-tokenless`; `share/aw/providers/tokenless/examples/`; `share/doc/aw-provider-tokenless/` |

`AWPRJ` defaults to empty. A value such as `AWPRJ=anolisa` inserts that namespace
before `aw/` in private program and resource paths and before `doc/` in document
paths. It does not change `bin/aw`. Override `BINDIR`, `LIBEXECDIR`, `DATADIR` or
`DOCDIR` as appropriate. Runtime configuration must name the resulting Provider
executable explicitly; installation paths are not discovered automatically.

## Build independent archives

Use a clean source checkout and an empty absolute output directory:

```bash
make -C distribution aw-packages VERSION=0.1.0 COMPONENTS='core sec-core tokenless' \
  OUTPUT="$PWD/target/aw-packages"
cd target/aw-packages
sha256sum -c SHA256SUMS
```

The explicit `COMPONENTS` list selects independent archives:

| Entry | Archive | Contents |
| --- | --- | --- |
| `core` | `aw-core-0.1.0-linux-<arch>.tar.gz` | `bin/aw`, ordinary installation Makefile and source/license documents |
| `sec-core` | `aw-provider-sec-core-0.1.0-linux-<arch>.tar.gz` | Provider `package.json`, bridge payload, schema, examples and configuration templates |
| `tokenless` | `aw-provider-tokenless-0.1.0-linux-<arch>.tar.gz` | Provider `package.json`, bridge payload, examples and configuration templates |

Select one or more entries without `all` or a combined archive. `VERSION` labels
artifacts; it does not select a source tag. `distribution/packaging/aw/` owns
archive assembly and concrete templates. Backend executables are delivered by
their own core packages. Native acceptance builds its selected backends through
the separate `aw-integration-backends` target; those files are not AW archives.

## Verify the installation

Run `python3 src/aw/scripts/check.py` on Linux for the normal offline contract
checks. Then use [installation](../../../docs/user-guide/en/aw/installation.md)
to configure an installed Agent and launch a session. Verify the actual effects
of [security checks](../../../docs/user-guide/en/aw/security.md) and
[Tokenless](../../../docs/user-guide/en/aw/tokenless.md) after installing their
backend core components. Provider file installation alone does not enable them.

Other components use the [repository build guide](../../../docs/user-guide/en/getting-started/building.md).

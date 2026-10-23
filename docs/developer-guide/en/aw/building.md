# Build AW from source

[中文版](../../zh/aw/building.md)

Build and package AW on Linux x86_64 or aarch64. For prebuilt packages, see
[Install AW](../../../user-guide/en/aw/installation.md).

## Build environment

Install [rustup](https://www.rust-lang.org/tools/install). On Ubuntu / Debian,
install the system dependencies below.

```bash
sudo apt-get update
sudo apt-get install -y git build-essential pkg-config libssl-dev
```

The build downloads Rust, Cargo dependencies, and optional RTK; fully offline
builds are not yet supported. The target architecture and glibc must be compatible
with the build environment. Build on the corresponding system for older targets.

## Build packages

Use a clean source checkout and an empty output directory.

```bash
git clone https://github.com/agentic-os-org/ANOLISA.git anolisa
cd anolisa/src/aw
cargo run --locked --release -p aw-package --bin aw-build -- \
  --version 0.1.0-preview.1 --component core \
  --output "$PWD/target/aw-preview-packages"
cd target/aw-preview-packages
sha256sum -c SHA256SUMS
```

Every checksum should report `OK`. Output contains
`aw-core-0.1.0-preview.1-linux-<arch>.tar.gz` and `SHA256SUMS`.
`--version` is a package identifier, independent of the source tag.

| `--component` | Output |
| --- | --- |
| `core` | AW launcher and configuration tools |
| `all` | Core, both Provider extensions, and all-in-one |
| `sec-core` / `tokenless` | The selected extension; requires core from the same build |

Choose `all` for security checks and compression, using a new empty output
directory. The package supplies RTK and TOON encoding. Prepare the Agent, model
credentials, and AgentSight separately.

## Deliver and verify

Deliver the selected archives and their checksum entries. Follow
[Install and run](../../../user-guide/en/aw/installation.md#install)
to install, configure, and launch. For all-in-one, set `AW_BUNDLE_KIND=aw-all-in-one`.

Verify that the Agent can call tools, `aw status` reports status, and `aw stop`
stops the service. After enabling optional capabilities, verify their effects
with [Tokenless](../../../user-guide/en/aw/tokenless.md) or
[Security checks](../../../user-guide/en/aw/security.md).

For other Agentic OS components, see the [repository build guide](../../../user-guide/en/getting-started/building.md).

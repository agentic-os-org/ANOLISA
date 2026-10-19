# Build AW from source

[中文版](../../zh/aw/building.md)

Build AW on your own Linux machine and produce packages to copy into a customer
environment. Build core for AW alone, or all-in-one for security checks and output
compression. For prebuilt packages, see [Install AW](../../../user-guide/en/user-entrypoint/aw/installation.md).

## Prepare the build environment

Native builds support Linux x86_64 and aarch64. Install Git, rustup, a C compiler,
pkg-config, and OpenSSL development headers. On Ubuntu / Debian, install the system
dependencies below, then install rustup using the
[official Rust instructions](https://www.rust-lang.org/tools/install).

```bash
sudo apt-get update
sudo apt-get install -y git build-essential pkg-config libssl-dev
```

The build needs network access to download source, the Rust toolchain, and Cargo
dependencies. Packages containing Tokenless also fetch a pinned RTK version.
These instructions cover local builds; dependency mirrors and bundles for fully
offline builds are not provided. The target machine needs a glibc version compatible
with the build environment. Build on the corresponding system when distributing to
older systems.

## Build the packages

The commands below build core. Keep the source checkout clean and committed, and
use an empty output directory. Inside `src/aw`, rustup selects the toolchain from
the repository's `rust-toolchain.toml`.

```bash
git clone https://github.com/agentic-os-org/ANOLISA.git anolisa
cd anolisa/src/aw
cargo run --locked --release -p aw-package --bin aw-build -- \
  --version 0.1.0-preview.1 --component core \
  --output "$PWD/target/aw-preview-packages"
cd target/aw-preview-packages
sha256sum -c SHA256SUMS
```

Every checksum should report `OK`. The directory contains
`aw-core-0.1.0-preview.1-linux-<arch>.tar.gz` and `SHA256SUMS`, where `<arch>` is
`x86_64` or `aarch64`. `--version` is the Preview identifier assigned to this build,
not a source tag.

For security checks and Tokenless, replace `--component core` with
`--component all` and choose a new empty output directory. This produces core,
both Provider extensions, and `aw-all-in-one`. The Tokenless package includes RTK
and TOON encoding, so customers do not install either separately. Configuration
determines which optional capabilities are enabled in all-in-one.

The `sec-core` and `tokenless` options produce only the respective extension.
Installing an extension requires core from the same source commit and package
version. Deliver artifacts from the same build together.

## Install, run, and verify

Deliver the selected archives and `SHA256SUMS`. If delivering only some archives,
keep only their entries in the checksum file to avoid errors for missing packages.

Continue from [Extract and install](../../../user-guide/en/user-entrypoint/aw/installation.md#install)
to install, generate a configuration, and start Qoder. When using all-in-one,
set `AW_BUNDLE_KIND` to `aw-all-in-one` in the installation example.

Verify that the Agent starts, `aw status` returns the running state, and `aw stop`
stops the local service. The initial core configuration has no policies. To verify
optional capabilities, continue with [Tokenless](../../../user-guide/en/user-entrypoint/aw/tokenless.md)
or [Security checks](../../../user-guide/en/user-entrypoint/aw/security.md).

For other Agentic OS components, use the [repository build guide](../../../BUILDING.md).
AW packages do not include an Agent, model credentials, or AgentSight.

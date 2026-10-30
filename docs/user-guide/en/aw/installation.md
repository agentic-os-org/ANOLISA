# Installation and first run

[中文版](../../zh/aw/installation.md)

Install AW on Linux to launch an existing Agent with optional Provider policies.
`anolisa install aw` and a published AW RPM channel are not yet available. Agent applications, model credentials and backend components are
installed separately.

For Qoder, use a signed-in CLI with native command Hooks; 1.1.64 is the verified
baseline. For other Agents, see [Configuration and Agent launch](configuration.md).

## Get a package

Use artifacts from a [successful AW Integration run](https://github.com/agentic-os-org/ANOLISA/actions/workflows/aw-integration.yml) for the intended
source commit. Check the matching `SHA256SUMS`; checksums detect corruption and
are not publisher signatures. See [source builds](../../../../contribute/en/aw/building.md)
for the developer delivery entrypoint.

CI archives target Linux x86_64 with glibc 2.39 or later, such as Ubuntu 24.04.
For Linux aarch64 or a different build environment, follow the source build guide.

| Archive | Contents |
| --- | --- |
| `aw-core` | AW command-line tool |
| `aw-provider-sec-core` | Tool safety checks Provider |
| `aw-provider-tokenless` | Tool output compression and command rewriting Provider |

Provider archives contain bridge programs only. Install Tokenless CLI/RTK and
sec-core CLI/daemon from their backend owner's release or native core package.

## Install

From the downloaded artifact directory, install the prebuilt AW CLI as your user:

```bash
export AW_VERSION=0.1.0
export AW_BUNDLE_KIND=aw-core
export AW_BUNDLE="$AW_BUNDLE_KIND-$AW_VERSION-linux-$(uname -m)"
sha256sum -c SHA256SUMS
tar -xzf "$AW_BUNDLE.tar.gz"
export AW_BUNDLE_DIR="$PWD/$AW_BUNDLE"
export AW_PREFIX="$HOME/.local"
make -C "$AW_BUNDLE_DIR" install PREFIX="$AW_PREFIX"
export PATH="$AW_PREFIX/bin:$PATH"
aw --version
```

Each checksum should report `OK`; `aw --version` should print the installed version.

Change `AW_PREFIX` to choose an installation directory. For a system installation,
an administrator can use
`sudo make -C "$AW_BUNDLE_DIR" install PREFIX=/usr/local`.

To add a local Provider extension, install it into a separate extension prefix:

```bash
export AW_PROVIDER_BUNDLE="aw-provider-tokenless-$AW_VERSION-linux-$(uname -m)"
tar -xzf "$AW_PROVIDER_BUNDLE.tar.gz"
export AW_EXTENSION_PREFIX="$HOME/.local/lib/aw/extensions"
install -d -m 700 "$HOME/.local/lib/aw"
aw install --prefix "$AW_EXTENSION_PREFIX" --bundle "$PWD/$AW_PROVIDER_BUNDLE"
aw list --installed --prefix "$AW_EXTENSION_PREFIX"
```

Use the independent sec-core archive in the same way when security checks are needed.

These extension commands use user scope. Root-managed extension writes require
`sudo` and explicit `--scope system`. Keep each extension prefix's scope consistent;
`sudo` alone does not select scope. Use `aw install` for local Provider bundles.

A Provider installed by RPM or another deployment owner can instead be referenced
by its absolute `transport.argv`. See [Provider bundles](../../../developer-guide/en/aw/reference/packages.md) for optional
raw extension management.

## Create configuration

Prepare the Agent using its own installation guide. This example requires a
signed-in Qoder CLI 1.1.64 or later. Generate an empty policy as your ordinary
user, keeping configuration and data outside the installation prefix:

```bash
export AW_HOME="$HOME/aw"
export AW_QODER="$(command -v qodercli)"
install -d -m 700 "$AW_HOME" "$AW_HOME/workspace"
qodercli --version
aw config init --prefix "$AW_PREFIX" \
  --config "$AW_HOME/aw.yaml" --state-dir "$AW_HOME/state" \
  --qoder "$AW_QODER"
aw validate --config "$AW_HOME/aw.yaml"
```

`AW_QODER` must select the actual executable by absolute path. `config init`
refuses to overwrite an existing configuration.

To enable a Provider, use its extension prefix and explicit template/backend
paths from [Tokenless](tokenless.md) or [security checks](security.md).
Use actual absolute paths in YAML; AW does not expand shell variables there.
Other Agent entrypoints and native settings are described in
[configuration](configuration.md).

## Launch and verify

```bash
cd "$AW_HOME/workspace"
aw run --config "$AW_HOME/aw.yaml" --agent qoder
```

Confirm that the native Agent starts and can call a tool. Exit the Agent, then
run `aw status --config "$AW_HOME/aw.yaml"` and
`aw stop --config "$AW_HOME/aw.yaml"`. An empty policy does not perform security
checks or compression. Enable the selected Provider and verify its actual tool
effects before using it for a workload.

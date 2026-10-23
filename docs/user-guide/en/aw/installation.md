# Installation and first run

[中文版](../../zh/aw/installation.md)

Requires Linux and a signed-in Qoder CLI 1.1.64 or later. For other Agents, see
[Configuration and Agent launch](configuration.md).

## Get a package

AW uses Preview packages; ANOLISA CLI and RPM installation are not available yet.
Download the `aw-preview-linux-x86_64-…` artifact from a
[successful AW Integration run](https://github.com/agentic-os-org/ANOLISA/actions/workflows/aw-preview.yml).
Extract the ZIP to obtain the packages and `SHA256SUMS`. CI packages target Linux x86_64
with glibc 2.39 or later, such as Ubuntu 24.04.

To compile yourself or use Linux aarch64, follow [Build from source](../../../developer-guide/en/aw/building.md)
and then continue here. Building requires network access to download dependencies.

| Package | Contents |
| --- | --- |
| `aw-core` | AW launch and configuration tools |
| `aw-provider-tokenless` | Tokenless and its required RTK; install core first |
| `aw-provider-sec-core` | Security Provider and sec-core programs; install core first |
| `aw-all-in-one` | All three components above |

Split packages must come from the same build. Install all-in-one directly into a new prefix;
do not also install the split packages there.

## Install

Run from the extracted artifact directory. Match the version to the downloaded filenames.
Use core for a first trial, or set `AW_BUNDLE_KIND` to `aw-all-in-one` for all capabilities.

```bash
sha256sum -c SHA256SUMS
export AW_VERSION=0.1.0-preview.1
export AW_BUNDLE_KIND=aw-core
export AW_BUNDLE="$AW_BUNDLE_KIND-$AW_VERSION-linux-$(uname -m)"
export AW_PREFIX="/opt/aw-preview/$AW_VERSION"
tar -xzf "$AW_BUNDLE.tar.gz"
sudo install -d -m 755 /opt/aw-preview
sudo "./$AW_BUNDLE/aw" install --bundle "$PWD/$AW_BUNDLE" --prefix "$AW_PREFIX" --scope system
export PATH="$AW_PREFIX/bin:$PATH"
aw --help
```

Every checksum should report `OK`; a successful install reports the installed packages.
The installer refuses to overwrite existing components. [Use a new prefix when upgrading](operations.md).
Set `AW_PREFIX` and `PATH` in subsequent terminals too, or add them to your shell configuration.

For a personal installation that does not host a system backend, use an absolute `AW_PREFIX`
you own, omit `sudo` and select `--scope user` (the default). Other users must not be able to write to its
parent directory. A root backend needs a root-managed installation. Scope is explicit:
running through `sudo` does not automatically select system scope or another user's database.

The prefix and [package database](operations.md) need
filesystems that support anonymous temporary files, hard links, atomic renames
and durable directory synchronization. If an error names `O_TMPFILE`, use a
filesystem that supports it for the reported directory.

Inspect the installation without starting a service or Provider:

```bash
aw list --installed --prefix "$AW_PREFIX"
aw doctor --prefix "$AW_PREFIX"
```

The list reads installation records; `doctor` checks the recorded files. New
installs store package facts in the selected database and a database locator under
`$AW_PREFIX/.aw-packages`. Queries with an explicit prefix follow that locator,
so ordinary-user configuration can read a system install. Old Preview installs
still use their component records there. These queries do not migrate records
or create a database. See
[package database and file checks](operations.md).

## Create configuration

If AW is already installed, the same local installer is available through:

```bash
aw install --prefix "$AW_PREFIX" --bundle "/absolute/path/to/unpacked-bundle"
```

For the system prefix above, run the command with `sudo` and `--scope system`.
This accepts unpacked Preview bundles and independent packages described by
`package.json`, not component names, download URLs or RPM/DEB packages. Preview
Provider packages still require core from the same build; an independent ordinary
tool need not include AW core or declare a Provider. Exact dependencies must
already exist in the same prefix or be delivered in the same Preview bundle.
Use `--dry-run` before execution; see [package plans and recovery](operations.md).
It does not start services, register Agent Hooks or enable Providers.
The standalone `aw-package` command has been removed. Use the `aw` at each
bundle's root for first installation, with an explicit absolute `--bundle` path,
and the installed `aw` for later operations.

Create configuration as your ordinary user, keeping data outside the installation prefix.

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

`AW_QODER` must be the absolute path to the real executable. `config init` creates a complete file
and refuses to overwrite it. Enable [sec-core](security.md) or [Tokenless](tokenless.md) separately.

## Launch and verify

```bash
cd "$AW_HOME/workspace"
aw run --config "$AW_HOME/aw.yaml" --agent qoder
```

Expect AW instance information followed by the Qoder interface. Ask Qoder to read the current
directory with a tool, following its native permission prompts. After the tool returns, exit Qoder
and run these commands in the same terminal.

```bash
aw status --config "$AW_HOME/aw.yaml"
aw stop --config "$AW_HOME/aw.yaml"
```

After enabling a Provider, follow its guide to verify the effect.

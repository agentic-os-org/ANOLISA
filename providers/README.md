# AW Providers

[中文版](README_zh.md)

This is the shared contribution space for concrete AW Providers. Each directory
owns one integration and its tests, examples, and release instructions. Framework
and protocol libraries remain in `../src/aw/crates/`.

| Provider | Capability | Source |
| --- | --- | --- |
| [sec-core](agent-sec-core/README.md) | Command security checks | First-party sec-core CLI/backend |
| [Tokenless](tokenless/README.md) | Tool output compression and command suggestions | First-party Tokenless CLI and packaged RTK |

## Contribute or distribute

Use `providers/<provider>/` when contributing an implementation here. Providers
may also live in independent repositories and be installed separately. They
connect through `aw-provider/v1alpha1` over stdio; AW configuration selects an
executable with `transport.argv`. Runtime loading does not depend on a Cargo
package name or a source-tree location.

For a contributed Provider, include:

- A maintainer/contact, source and license, supported backend versions, and paired READMEs.
- Implementation, offline contract tests, and a runnable configuration example.
- Build/install instructions, backend requirements, data locations, and cleanup steps.

Each Rust implementation owns an independent Cargo workspace and lockfile.
`providers/` has no shared workspace; framework crates never depend on concrete
Providers. Other languages use their own toolchain and the same process protocol.
Wire contract tests into the AW gate. Keep native framework adaptation in `aw-adapter` and backend
behavior in the Provider.

Component owners maintain implementations, dependencies and releases. The OS
distributor selects versions, validates their composition and delivers installation
and upgrades. First-party Providers should build, test and release outside the
repository using pinned public AW libraries.

The bundled Rust Providers currently use repository-local AW libraries; Tokenless
composition tests also use the sec-core bridge, so builds outside the repository
are not yet supported. Standalone SDK publication and artifact-based composition
are future work. Preview delivers a single build set, with installation checking
matching versions and source commits for core and Providers.

The existing package builder supports core, sec-core, and Tokenless. A new folder
is not automatically bundled or installed; independent Providers supply their
own installer. Bundling a new Provider also requires package manifest, build,
installation, and lifecycle test support.

See [Provider development and distribution](../docs/developer-guide/en/aw/providers/index.md)
for the protocol, sample implementation, and validation entrypoints.

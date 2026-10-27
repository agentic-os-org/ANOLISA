# AGENTS.md — agent-memory

Repository-wide contribution, platform, code, documentation and commit rules are
inherited from [the root AGENTS.md](../../AGENTS.md). This file adds the component's
build and delivery boundaries.

## Entry points and payload ownership

Use the component root [Makefile](Makefile); `packaging/*.mk` files implement its
targets. `make build` builds the native binary and OpenClaw bundle. `make install`
copies prebuilt files using `PREFIX`, `AWPRJ` and optional `DESTDIR`, without
building, registering frameworks or starting services. The explicit
`install-legacy` entrypoint retains the historical user/system profile.

Without an explicit `INSTALL_PROFILE`, effective UID 0 selects system installation
with `PREFIX=/usr/local`; other UIDs select user installation with
`PREFIX=$HOME/.local`. User resources, documentation and the MCP descriptor use
`${XDG_DATA_HOME:-$HOME/.local/share}` and the user unit under its `systemd/user/`.
The user plugin bundle defaults to `DATADIR/adapters`; system installations use
`LIBEXECDIR/adapters`. An explicit `ADAPTERDIR` overrides either default.
Explicit path variables take precedence. Keep `PREFIX` independent of the XDG
data root; `DESTDIR` only prepends a temporary root. RPM installs retain `/usr`; package staging must explicitly select `INSTALL_PROFILE=system`.
Do not infer profiles from `PREFIX`, migrate runtime paths, or treat
the installed `default.toml` sample as the active user configuration.

[packaging/rpm/agent-memory.spec.in](packaging/rpm/agent-memory.spec.in) defines the
physical RPM payloads and scriptlets:

| Package | Responsibility |
|---------|----------------|
| `agent-memory` | Empty compatibility package providing the component capability and requiring matching core/plugins at exact `%{version}-%{release}`. |
| `agent-memory-core` | Native binary, configuration, MCP descriptor, optional user unit, tmpfiles rules, license and documentation; provides the separate core capability. |
| `agent-memory-plugins` | OpenClaw bundle and registration scripts; requires matching core. |

Keep the conflicts with older monolithic packages to prevent overlapping payload
owners. Conflicts do not transfer ownership or replace an installed package.
Keep runtime scriptlets with their payload owner and framework cleanup guarded
for final erase. Do not add an empty skills package.

## Service sources

The [user unit template](packaging/systemd/anolisa-memory@.service.in) is the
single source for RPM and Make installation. Both render `{bindir}` using the
final binary directory; the RPM uses `%{_bindir}` and source installs use `BINDIR`.

[packaging/render-install.py](packaging/render-install.py) renders both the
service token and the MCP JSON command from the final `BINDIR`. Keep `DESTDIR`
out of their runtime commands. The static MCP descriptor retains `/usr/bin`
for the native RPM; source installation renders a copy without modifying it.

Raw archives keep the `share/anolisa/agent-memory/anolisa-memory@.service` member
name but carry the template bytes. The raw contract must declare
`render = "anolisa-paths-v1"` for this mapping so ANOLISA resolves `{bindir}` at
installation time for both user and system layouts. This requires ANOLISA
0.3.15 or later. Keep the unit identity, session/config paths and opt-in activation
policy unchanged when adjusting delivery paths.

## Raw builds and integration metadata

Release/preview packaging uses contracts, inputs and scripts from the same source
commit. The release action pins Node 22.23.2; use `make build-openclaw-plugin-ci`
to check versions before `npm ci --legacy-peer-deps` and emit the esbuild
dependency manifest. Preview validation does not publish a release.

[packaging/raw/package.py](packaging/raw/package.py) accepts explicit binary and
plugin output directories, target, epoch and TOML build metadata. It neither
builds nor executes the supplied binary. Metadata binds `version`, `target_os`,
`target_arch`, `binaries.agent-memory` (SHA256) and `plugin.sha256` to the inputs.
The raw contract comes from [its component template](packaging/anolisa/raw/component.toml.in)
and the Cargo version. Both raw targets must retain the same payload inventory,
permissions and reproducibility checks.

ANOLISA owns the RPM integration registry under
[distribution/anolisa/manifests/components/](../../distribution/anolisa/manifests/components/).
Component RPM builds must not generate or install those contracts. The explicit
`make generate-component-contract` legacy helper is not a dependency of
`make dist` or ordinary plugin compilation. Synchronize raw layout changes with
the distribution's agent-memory contract without lowering its stronger minimum
version requirement for RPM adapter features.

For packaging changes, use `make test-packaging` with fixtures; it checks file
installation, raw archives and RPM ownership without native compilation. Follow
the root platform restrictions before running native builds or Rust checks.

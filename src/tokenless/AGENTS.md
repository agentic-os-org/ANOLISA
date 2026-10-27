# AGENTS.md — tokenless

Repository-wide contribution, platform, code, documentation and commit rules are
inherited from [the root AGENTS.md](../../AGENTS.md). This file adds Tokenless
delivery boundaries; it does not replace those rules.

## Build and installation entrypoints

Use the component root [Makefile](Makefile) for public targets; its included
`packaging/*.mk` files hold channel-specific rules. File installation and layout
validation live in [packaging/install.sh](packaging/install.sh). Do not introduce
a second public Makefile.
`make build` produces the native CLI, RTK and framework bundles. `make install`
copies existing outputs using `PREFIX`, `AWPRJ` and optional `DESTDIR`, without
building, registering frameworks or activating services. `make install-core`
copies only CLI/RTK and core resources; `make install-plugins` copies only
framework resources, without requiring the core build outputs. Neither group
installs or activates the other. Keep the explicit
`install-legacy` entrypoint for the historical user/system installation profile.
`make setup` builds, installs and registers adapters using the public layout;
framework registration and `make uninstall` use that same resolved layout.

Without an explicit `INSTALL_PROFILE`, effective UID 0 selects the system
profile (`PREFIX=/usr/local`); other UIDs select the user profile
(`PREFIX=$HOME/.local`) and `XDG_DATA_HOME` resources (or the owned home
`.local/share` fallback). Explicit profiles override this default. Keep private
executables under `PREFIX/libexec`, since XDG defines no libexec root. Explicit
role paths override these defaults. Packaging must select `INSTALL_PROFILE=system` with `PREFIX=/usr`
and `DESTDIR`; it must ignore user XDG roots. Preserve existing runtime configuration
and state directories; file installation never migrates them.

## RPM package ownership

[packaging/rpm/tokenless.spec.in](packaging/rpm/tokenless.spec.in) is the authority
for physical RPM products, dependencies, file ownership and scriptlets.

| Package | Responsibility |
|---------|----------------|
| `tokenless` | Empty compatibility package selecting matching core and plugins; retains `Provides: anolisa-component(tokenless)`. |
| `tokenless-core` | Public `tokenless` and `rtk` commands, private RTK executable, license and runtime documentation. |
| `tokenless-plugins` | All framework integrations, common hooks/resources, lifecycle scripts and the cosh extension. |

The compatibility package requires both roles at exact `%{version}-%{release}`.
The plugins package also requires matching core and its declared Python, jq and
Bash runtime dependencies. There is no Tokenless skills RPM. Preserve the
conflicts with older monolithic `tokenless` packages so ownership transfers in
the same transaction. Keep scriptlets with the package owning their resources;
framework removal must remain guarded for final erase.

When changing payloads, update the shared file inventories and the corresponding
RPM `%install`/`%files` groups together. Do not place framework payloads in core
or create duplicate file ownership across the RPM roles.

## ANOLISA metadata boundary

ANOLISA owns the installed RPM integration registry. Its Tokenless contract is
[distribution/anolisa/manifests/components/tokenless/component.toml](../../distribution/anolisa/manifests/components/tokenless/component.toml).
Neither the compatibility RPM nor the core/plugins RPMs install
`/usr/share/anolisa/components/tokenless/component.toml`. The compatibility
package's Provides capability identifies the component; it does not transfer
ownership of ANOLISA's registry to Tokenless.

The component's [raw contract template](packaging/anolisa/component.toml.in)
remains an input for raw/explicit distribution entrypoints. The explicit
`make generate-component-contract` target writes
`target/packaging/anolisa/component.toml`; raw archives embed it as
`.anolisa/component.toml`. Ordinary framework builds stamp their own manifests
independently and must not generate ANOLISA metadata. RPM builds must not depend
on generating or installing that raw contract.

For packaging-only changes, run `make test-packaging` with prebuilt/mock fixtures.
Use `make test-install-docs` when changing installation documentation. Follow the
root platform restrictions before running native builds or Rust checks.

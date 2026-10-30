# AW package assembly

[中文版](README_zh.md)

Build independent AW core and Provider archives with ordinary Make targets and
scripts. Component owners deliver the backend programs separately; AW manages
Provider configuration and execution.

Use a clean Linux checkout, Rust 1.97.1 and Python 3.9 or newer. Select the
artifacts explicitly and use an absolute, empty output directory:

```bash
make -C distribution aw-packages VERSION=0.1.0 \
  COMPONENTS='core sec-core tokenless' OUTPUT="$PWD/target/aw-packages"
cd target/aw-packages
sha256sum -c SHA256SUMS
```

`COMPONENTS` accepts `core`, `sec-core` and `tokenless`, separated by spaces.
Each selection produces its own archive; `VERSION` labels those artifacts and
does not select a source revision. Assembly verifies the checkout stays on one
unchanged Git commit and records that source identity.

```text
aw/
├── build.sh, build.py             # Build the selected AW/Provider workspaces
├── package.py                    # Assemble independent archives and SHA256SUMS
├── common.py                     # Bounded commands, cancellation and copying
├── core.Makefile                 # Archive install entry; reuses AW install.mk
├── build-integration.sh          # Explicit backend build entry for acceptance
├── integration.py                # Build sec-core, Tokenless and pinned RTK
├── templates/                    # Policy data with explicit program paths
└── tests/                        # Build selection and archive/install fixtures
```

Core delivers `bin/aw`, an ordinary installation Makefile, and license/source
documentation. Install it with `make PREFIX=... AWPRJ=... DESTDIR=... install`.
Provider archives contain `package.json`, their bridge and private resources in
`payload/`, and policy templates. Optional local Provider installation uses an
already installed AW CLI. Provider archives contain no backend executables.

Templates take `PROVIDERS_DIR`, the actual root containing the `sec-core` and
`tokenless` Provider directories. Supply backend paths through `SEC_CORE_CLI`,
`TOKENLESS_CLI`, `RTK_BINARY`, `RTK_SHA256` and `SOCKET` as applicable. This also
supports an `AWPRJ` namespace or separate core and Provider prefixes.

Native integration acceptance prepares backend binaries through a separate
entrypoint; ordinary package assembly does not invoke it:

```bash
make -C distribution aw-integration-backends BACKENDS='sec-core tokenless'
```

See the [source guide](../../../contribute/en/aw/building.md) and
[installation guide](../../../docs/user-guide/en/aw/installation.md).

# AW package assembly

[中文版](README_zh.md)

Build independent AW core and Provider archives with ordinary Make targets and
scripts. Component owners deliver the backend programs separately; AW manages
Provider configuration and execution.

Use a clean Linux checkout, Rust 1.97.1 and Python 3.9 or newer. Select the
artifacts explicitly and use an absolute, empty output directory:

```bash
make -C distribution aw-packages VERSION=0.1.1 \
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
├── integration.py                # Build sec-core and Tokenless
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
`TOKENLESS_CLI` and `SOCKET` as applicable. This also supports an `AWPRJ`
namespace or separate core and Provider prefixes.

Native integration acceptance prepares backend binaries through a separate
entrypoint; ordinary package assembly does not invoke it:

```bash
make -C distribution aw-integration-backends BACKENDS='sec-core tokenless'
```

See the [source guide](../../../contribute/en/aw/building.md) and
[installation guide](../../../docs/user-guide/en/aw/installation.md).

## Matched SDK candidates

Build npm and wheel artifacts from one full Git commit. The builder uses a Git
archive, so local edits cannot change its inputs. Node 22+ and Python 3.11+ with
pip, setuptools and wheel already installed are required; SDK builds are offline.
The receipt records node, npm, Python, pip, setuptools and wheel versions. Use
those interpreter and build-tool versions when verifying it; verification compares
package filenames and payloads, and does not enforce tool-version equality.
The output directory must be empty. Artifacts and the receipt are staged before
publication; failure or cancellation rolls back files published by this build,
leaving the directory ready for retry.

```bash
python3 distribution/packaging/aw/sdk_candidate.py build \
  --repo "$PWD" --candidate <full-commit-sha> --output "$PWD/target/sdk-candidate"
python3 distribution/packaging/aw/sdk_candidate.py verify \
  --repo "$PWD" --candidate <full-commit-sha> \
  --receipt "$PWD/target/sdk-candidate/sdk-candidate.json"
```

Verification rebuilds from the candidate and checks artifact filenames and every shipped SDK file,
including native bridge assets. Archive timestamps are ignored. A copied commit
field and updated checksum cannot make an old payload pass this comparison.
Add three `--rpm /absolute/path/package.rpm` arguments to inspect the AW and both
Provider RPMs; this requires rpm, rpm2cpio, cpio and Bash. Their embedded
`source.json` must declare the candidate in `source_commit` (archive schema) or
`commit` (native RPM schema); if both appear, they must agree. All three RPMs
must share a version/release/architecture. RPM source
records are declarations: retain the controlled offline RPM build log, source and
vendor checksums separately. This check does not establish binary reproducibility,
installation behavior, native adoption or business performance. Run those gates
on the actual candidate before delivery; keep previous packages as labeled history.

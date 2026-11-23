# Developing and releasing the Omarchy integration

[简体中文](CONTRIBUTING_zh.md)

Read the root `AGENTS.md`, `CONTRIBUTING.md`, and the component onboarding and
commit standards under `specs/`. Keep integration source, tests and documentation
in `distribution/omarchy`; canonical user manuals live in `docs/user-guide/{en,zh}/omarchy/`.
Changes to reusable release infrastructure must preserve
the existing components' targets and verification rules.

## Local checks

On Linux, use Python 3.11.6 for CI parity. Install development dependencies in a
virtual environment; PySide6 is not a laptop runtime dependency.

```bash
python3.11 -m venv .venv-omarchy
.venv-omarchy/bin/python -m pip install -r distribution/omarchy/tests/requirements.txt
.venv-omarchy/bin/ruff check distribution/omarchy
.venv-omarchy/bin/ruff format --check distribution/omarchy
.venv-omarchy/bin/python -m pytest distribution/omarchy/tests -p no:cacheprovider
.venv-omarchy/bin/python distribution/omarchy/tests/run_qml.py
python3 .github/scripts/validate-repository-metadata.py
```

The PR CI runs Python and offscreen QML tests for this component and changes to
its build action or shared release workflow. Offscreen QML checks do not prove
native bar anchoring, outside-click dismissal or terminal focus on Omarchy.

## Build a local candidate

Use Linux x86_64, Rust 1.97.1, Python 3.11+, a C linker, pkg-config, OpenSSL,
libudev and libseccomp development packages. Ubuntu 24.04 is the CI build host.
Run from the full repository root with access to crates.io and official GitHub Releases.

```bash
rustup toolchain install 1.97.1 --profile minimal
python3 distribution/omarchy/release.py \
  --version 0.1.3 --output /tmp/omarchy-candidate \
  --cache /tmp/omarchy-build-cache --allow-dirty
```

`--allow-dirty` is for unpublished local review only. The manifest records the
base commit, dirty status and a hash of the selected source files. Normal CI
requires a clean checkout. Formal builds also require the exact tag, version
and HEAD to agree. Existing output directories are never overwritten.

`release.json` pins official cosh-ng, sec-core and Tokenless archives and SBOMs
by SHA-256. AW and both Providers are built from this checkout with their own
Cargo.lock files. Qoder is not bundled. The sec-core payload intentionally covers
native Bash code scans; Python-specific scanners and OS sandboxing are outside
this integration's scope.

The builder checks input hashes, native versions, complete payload hashes and
the shared CycloneDX graph rules before exposing the output directory. Failure
leaves no publishable output. Build inputs may use a verified local cache.

Four binary artifacts are produced:

```text
anolisa-omarchy-0.1.3-linux-x86_64.tar.gz
anolisa-omarchy-0.1.3-linux-x86_64.tar.gz.sha256
anolisa-omarchy-0.1.3-linux-x86_64.tar.gz.cdx.json
anolisa-omarchy-0.1.3-linux-x86_64.tar.gz.cdx.json.sha256
```

```bash
python3 .github/actions/prebuilt-rust-common/verify-artifacts.py \
  --directory /tmp/omarchy-candidate --component omarchy --version 0.1.3 \
  --os linux --arch x86_64 --layout flat
python3 distribution/omarchy/verify_release.py \
  /tmp/omarchy-candidate/anolisa-omarchy-0.1.3-linux-x86_64.tar.gz --version 0.1.3
```

The SBOM distinguishes upstream build-input inventories from exact shipped-file
hashes. An input inventory can contain dependencies used by adapters omitted from
this payload. Review `bundle.json` for the exact installed files.

## Preview, acceptance and publication

1. Submit the source and CI changes for review. Merging the PR does not publish a Release.
2. A maintainer runs **Release Preview** with component `omarchy` and version `0.1.3`.
   Preview artifacts are retained for 14 days. They are not public Release assets.
3. Download the four artifacts and run the [Omarchy acceptance checks](USER_GUIDE.md#acceptance).
   Preserve the source SHA, checksums, OS details, install transcript and results.
4. Agree on ownership and version. Finalize the changelog and create the approved
   `omarchy/v0.1.3` tag on the reviewed commit. The release workflow rebuilds that tag;
   it does not promote the Preview files.
5. The existing `release` environment and `RELEASE_TOKEN` control publication.
   The new build job uses `ubuntu-24.04`; maintainers must permit that runner.
   Successful verification publishes source, binary and SBOM assets under the official tag.
6. Test the official download install path. Then update the separate `omarchy-pkgs`
   recipe to use that official immutable asset and its verified digest.

Confirm the proposed CODEOWNERS fallback with the maintainers before submission.
Before the first release, obtain a recorded maintainer decision on the onboarding
requirement for `.anolisa/component.toml`: either implement and test its lifecycle,
or approve an explicit exception for this standalone integration. Until that
decision is recorded, lifecycle registration remains an open release gate.
The integration currently uses its own
installer and is not registered with the ANOLISA CLI's discovery or lifecycle
manager. A placeholder contract would not make `anolisa install` work; supporting
that separate entry point requires an actual, tested lifecycle design.
No personal-fork Release URL is used by the new installer. Preview source archives
contain committed integration files and the root license; rebuild binaries from
the full monorepo so AW and Provider lockfiles are available.

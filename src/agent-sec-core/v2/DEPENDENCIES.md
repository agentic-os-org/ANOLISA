# V2 dependency boundaries

`Cargo.lock` pins the resolved dependency graph; it does not establish that the
sources are available offline or that the dependencies have been security-audited.
The workspace currently has registry and local path dependencies, with no ActPlane
Git dependency. The Adapter emits the supported DSL subset; compiler acceptance
belongs to the actual AgentSight/ActPlane deployment.

## HTTP/TLS and unsafe inventory

| Dependency path | Purpose and review boundary |
|---|---|
| `asc-daemon / asc-cli -> asc-observability[runtime] -> opentelemetry_sdk / tracing-opentelemetry` | Local context and stderr diagnostics only. No OTLP exporter, HTTP client or TLS provider in this path. |
| `asc-agentsight-client -> ureq -> rustls / rustls-webpki -> ring` | AgentSight API HTTP/TLS. ureq 2.12.1 explicitly selects the ring provider for its default TLS config. This Client is wired into the daemon through Policy Runtime. |
| `rustls -> ring` | The AgentSight client crypto boundary contains unsafe/native code. Workspace-local unsafe prohibition does not establish a zero-unsafe dependency tree. |
| `ureq / clients -> url -> idna / ICU` | URL and domain-name handling. Keep input bounds and evaluate advisories for the resolved graph. |
| `tokio -> libc / mio / socket2` | Existing OS/socket boundary, also outside workspace-local `unsafe_code = "forbid"`. |
| `Client -> uuid (v5) -> sha1_smol` | Deterministic target identity, not an authentication or signature algorithm. The v5 feature is requested only by the Client. Workspace builds can still unify features. |
| `asc-capability-code-scan -> fancy-regex` | Backtracking regex engine for the code-scan rule set, needed because the rules use look-around that `regex` does not support. The crate itself contains no `unsafe`; its `regex-automata` dependency does. |
| `asc-capability-code-scan -> yaml-rust2` | Parses the embedded code-scan rule documents. Pure Rust with no `unsafe` in the crate itself; pulls `encoding_rs`, `simdutf8`, `hashlink` and `arraydeque`, which do contain `unsafe`. |
| `asc-sqlite-kernel -> rusqlite -> libsqlite3-sys` | Event persistence. The `bundled` feature compiles the amalgamated SQLite C sources, so the resolved SQLite version is pinned by the crate rather than by the host. The FFI layer contains unsafe/native code outside the workspace-local `unsafe_code = "forbid"` boundary, same class as `tokio -> libc`. Requires a C toolchain at build time. `agent-sec-core.spec.v2.in` already lists gcc/clang among the required build tools, but as a comment (lines 39-46) stating they are supplied by the CI image rather than declared as `BuildRequires`; this change does not alter that arrangement. Builds outside CI must install a C compiler. |
| `asc-sqlite-kernel -> rustix` | Only the `fs` and `process` feature subsets, for `flock()` advisory locking and `getuid()`. Chosen over raw `libc` because it wraps the syscalls safely and therefore keeps `unsafe_code = "forbid"` intact; `File::lock()` is unavailable at MSRV 1.88. |
| `asc-daemon -> asc-event-sink -> asc-persistence-sqlite` | The daemon now persists code-scan audit events through explicit JSONL and SQLite paths. This brings the bundled SQLite C dependency into the daemon normal graph; each configured write path remains independently fail-open, matching v1 bookkeeping. |

YAML parsing is back in this workspace, so the earlier statement that no YAML
parser remains no longer holds. What still holds is the narrower property that
mattered: the removed `actplane-ifc-compiler -> serde_yaml -> unsafe-libyaml`
chain is not reintroduced. `yaml-rust2` is a Rust parser rather than a
transliterated C one, so it avoids that specific unsafe surface — it does not
make the code-scan dependency subtree unsafe-free, as the table above records.
No HTTP/TLS library or crypto-provider switch is part of this change. In
particular, replacing ring with aws-lc-rs would add an FFI-based crypto
implementation, not prove that unsafe exposure decreased.

The code-scan capability is wired into `asc-daemon` through the handler crate,
so `fancy-regex` and `yaml-rust2` are now in the daemon's normal dependency
graph. This is a deliberate consequence of scanning inside the daemon rather
than in the CLI. Verify the resolved subgraph rather than assuming it:

```sh
cargo tree -p asc-capability-code-scan --edges normal --locked
cargo tree -p asc-daemon --edges normal,build --locked
```

The daemon's normal/build graph includes Policy Runtime, the AgentSight Adapter
and Client, and the Client's ureq/rustls/ring dependencies. Runtime itself depends
only on generic policy ports and std threads. Reconciliation adds local path
crates to the daemon graph without adding registry packages to Cargo.lock. Verify
the executable dependency boundary separately from workspace tests:

Cargo unifies `asc-observability/runtime` across selected workspace members. The
feature exposes process initialization helpers but no longer enables exporter
or HTTP dependencies. Only product main installs the process-singleton runtime;
depending on the crate does not automatically initialize OTel globals.

Verify both scopes from `v2/` after dependency or composition changes:

```sh
cargo tree -p asc-daemon -p asc-cli --edges normal,build,features --locked --offline
cargo tree --workspace --edges normal,build,features --locked --offline
```

The removed `actplane-ifc-compiler -> serde_yaml -> unsafe-libyaml` chain remains
absent from this workspace lockfile.

## Native build and packaging

Local tracing adds no C/CMake crypto toolchain. The daemon includes ring through
its AgentSight Client and bundled SQLite through event persistence. The V2 RPM
spec (`agent-sec-core.spec.v2.in`) builds this workspace independently of V1.
A developer workspace build does not establish RPM/systemd acceptance.

## Release checks

- Keep `Cargo.lock` reviewed; inspect new dependencies, enabled features, licenses,
  source origins and build scripts. Retain the existing ban on local unsafe code.
- Recheck the product and workspace feature graphs above, including crypto
  provider selection and native build requirements. A dependency upgrade must
  not silently invalidate this inventory or the release environment's prerequisites.
- Use `cargo audit` for known advisories, or `cargo deny check` for advisories plus
  source/license/dependency policies. Record tool version, advisory database
  revision, findings and time-bounded exceptions. A successful build is not an
  advisory scan, and a clean scan does not prove absence of unknown defects.
- For critical dependencies, record trusted audit evidence and review upgrade
  diffs. `cargo vet` can manage these records; it does not perform the audit itself.
- Before offline packaging, obtain all dependency sources (including required
  target-specific packages). `cargo vendor --locked` supports both registry and
  Git sources; install its emitted source replacement configuration, then validate
  the intended build with `--frozen` in an isolated environment. A warm local cache
  passing `--offline` does not establish that an offline source bundle is complete.

This file registers the dependency boundary and follow-up release checks. No new
advisory scan, third-party source audit, vendor bundle or CI audit gate is claimed
by the tracing or reconciliation tests. These remain explicit release-engineering work.

References: [RustSec tooling](https://rustsec.org/),
[cargo-deny](https://embarkstudios.github.io/cargo-deny/),
[cargo-vet](https://mozilla.github.io/cargo-vet/),
[Cargo vendor](https://doc.rust-lang.org/cargo/commands/cargo-vendor.html).

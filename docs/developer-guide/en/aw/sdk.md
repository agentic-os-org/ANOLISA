# Use the Rust SDK

[中文版](../../zh/aw/sdk.md)

Use `aw-sdk` when your application owns its Agent loop and needs typed security
or compression calls. To run an existing Agent with managed Hooks, use
[`aw run`](../../../user-guide/en/user-entrypoint/aw/README.md).

The package lives in `src/aw/sdk/rust/`, outside the framework workspace.
In a local Cargo project, point the dependency at that directory:

```toml
[dependencies]
aw-sdk = { path = "/absolute/path/to/ANOLISA/src/aw/sdk/rust" }
```

Both components are enabled by default. An application or Provider needing only
one can select it without linking the other component's implementation:

```toml
[dependencies]
aw-sdk = { path = "/absolute/path/to/ANOLISA/src/aw/sdk/rust", default-features = false, features = ["tokenless"] }
```

| Feature | Operations | Runtime requirements |
| --- | --- | --- |
| `sec-core` | `scan_code` | Reachable sec-core daemon; client verifies server UID 0 |
| `tokenless` | `before_model`, `pre_tool`, `post_tool`, `retrieve` | Explicit state directory; PreTool requires an explicit RTK executable |

## Export public sources

A component owner can export the committed SDK and public Provider libraries
without including AW Service, Provider implementations or backend executables.
From the repository root, using Python 3.11 or newer:

```bash
python3 src/aw/scripts/export_sdk.py \
  --output src/aw/target/aw-sdk-source.tar.gz
mkdir -p /tmp/aw-sdk-sources
tar -xzf src/aw/target/aw-sdk-source.tar.gz -C /tmp/aw-sdk-sources
cd /tmp/aw-sdk-sources/aw-sdk-0.1.0/src/aw/sdk/rust
cargo test --locked
```

Successful AW CI runs retain the verified source archive and `result.json`
under `aw-sdk-source-<tested SHA>` for 14 days. These are candidate build
artifacts, not a registry release.

The exporter reads committed `HEAD`; use `--revision` to select another commit.
It refuses to overwrite an existing archive and prints its SHA-256. `source.json`
records component versions, original Git blobs and packaged file digests.
Workspace manifests retain dependency inheritance but include only public members.
Schemas, shared taxonomy data, test fixtures and licenses travel with the source.
Registry dependencies remain pinned by Cargo.lock and require registry access or
an already populated Cargo cache.

An external application points its `aw-sdk` dependency at the extracted SDK
directory. Its own Cargo.lock records its dependency resolution. The archive does
not install RTK or the sec-core daemon. End-user binary installation is a separate
[distribution responsibility](../../../../src/aw/docs/design/provider-distribution.md).

## Configure and call

Pass absolute paths to `Client::new(Config)`. Components omitted from configuration
return `Error::NotConfigured`; construction does not install or start them.
Tokenless creates its configured state directory and databases, which remain after
the client is dropped. `stash_error()` and `stats_error()` expose unavailable
storage without claiming recovery or statistics succeeded.

The [application example](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/sdk/rust/examples/application.rs)
scans code through the daemon and processes an API result through Tokenless.
With a running sec-core daemon, execute from the repository root:

```bash
cd src/aw/sdk/rust
cargo run --locked --example application -- /run/agent-sec-core/daemon.sock "$HOME/.local/state/aw-sdk"
```

For RTK, set `tokenless::Config::rtk_path` to the installed executable.
The SDK never searches the user's PATH. TOON runs through the linked library.

`CallContext` carries application, session and tool-call attribution. Session and
tool-call IDs become sec-core compatibility labels; an optional W3C carrier stays
separate. These fields do not establish identity or grant authorization.

## Adopt results explicitly

- `scan_code` separates a typed scanner result from a daemon rejection and a local
  transport error. `ok` means the scan completed; inspect `verdict` before deciding
  whether to proceed. Timeout does not prove server cancellation or permit replay.
- Tokenless returns transformations; the application chooses how to adopt them.
  Declare only capabilities that the application can apply. Failed tool output and
  RTK-owned output retain the component's preservation rules.
- `retrieve` requires markers from trusted, currently visible conversation state.
  Never accept a model's assertion of visibility. Declaring a recovery tool promises
  the application has registered a working, authorized implementation.

Calls are synchronous and do not retry. They do not pass through AW Service,
generate Host/Core receipts or prove native effect adoption. AW Providers retain
their own admitted effects: PreTool suggestions require a new call through security
and native authorization.

## Validate changes

From the repository root:

```bash
python3 src/aw/scripts/check.py
```

The gate compiles both features and each separately, inventories required SDK
tests, compares Tokenless results against its real Runtime, checks authorized
recovery, exports a reproducible source archive, builds an external consumer, and verifies that component-only builds exclude the other component.
The local RTK fixture checks invocation and result mapping; it does not replace
the real RTK and root-daemon release acceptance.

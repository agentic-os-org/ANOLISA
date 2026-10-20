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
recovery, and verifies that component-only builds exclude the other component.
The local RTK fixture checks invocation and result mapping; it does not replace
the real RTK and root-daemon release acceptance.

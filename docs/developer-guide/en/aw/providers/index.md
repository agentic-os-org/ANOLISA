# Provider development and distribution

[中文版](../../../zh/aw/providers/index.md)

Providers connect capabilities such as safety checks and compression to AW through
a process protocol. They can be developed, distributed, and upgraded independently,
or contributed to the [Provider directory](../../../../../providers/README.md).

## Directories and responsibilities

```text
ANOLISA/
├── src/aw/crates/           # AW framework
│   └── aw-provider/         # Shared protocol and admission
└── providers/               # Independent implementation workspaces
    ├── agent-sec-core/
    └── tokenless/
```

`aw-provider` is the protocol library shared by the Host and implementations.
Each directory under `providers/` owns a concrete integration, including its
program, schemas, examples, and tests. Existing binaries retain the names
`aw-provider-sec-core` and `aw-provider-tokenless`, preserving packages and commands.

## OS distribution design

The component owner maintains the implementation, tests, dependencies and release.
The OS distributor selects versions, verifies origin and composition compatibility,
and delivers installation packages and upgrade paths. Sharing a repository does
not require a shared release cycle.

First-party Providers should build, test and release outside the repository using
pinned public AW libraries, with backend dependencies managed by their owners.
Third parties can implement the public process protocol and connect through YAML,
without linking Rust libraries or checking out ANOLISA. The distribution layer
consumes component artifacts and validates bundled dependencies together.

See [architecture boundaries](../../../../../src/aw/docs/design/en/architecture-boundaries.md)
for ownership and migration scope.

## Integrate independently

You can build, test and deploy a Provider in any language outside ANOLISA
without linking Rust crates. Every method runs as a one-shot process that reads
one UTF-8 JSON request from stdin and writes one UTF-8 JSON response to stdout
(exit `0` for both allow and policy `block`; nonzero exit signals execution
failure).

### 1. Minimal 3-method Provider skeleton

Save the following executable script as `/absolute/path/to/my-provider.py`
(`chmod 755`). It implements the three `aw-provider/v1alpha1` methods
(`describe`, `validate_config` and `invoke`):

```python
#!/usr/bin/env python3
import json
import sys


def valid_config(cfg):
    return (
        isinstance(cfg, dict)
        and isinstance(cfg.get("blocked_tools"), list)
        and all(isinstance(name, str) for name in cfg["blocked_tools"])
    )


def main():
    req = json.load(sys.stdin)
    reply = {
        "api_version": "aw-provider/v1alpha1",
        "request_id": req["request_id"],
        "status": "ok",
    }
    method = req["method"]

    if method == "describe":
        reply["operations"] = [
            {
                "name": "check",
                "events": ["tool.before", "tool.after"],
                "effects": ["observe", "block"],
            }
        ]
    elif method == "validate_config":
        if not valid_config(req.get("config")):
            reply.update(status="error", error_code="invalid_config")
    elif method == "invoke":
        cfg = req.get("config")
        if req.get("operation") != "check" or not valid_config(cfg):
            reply.update(status="error", error_code="invalid_operation_or_config")
        else:
            tool = req["event"]["tool"]["native_name"]
            blocked = req["event"]["name"] == "tool.before" and tool in cfg["blocked_tools"]
            reply["input_digest"] = req["input_digest"]
            reply["effects"] = (
                [{"type": "block", "reason_code": "tool_policy"}]
                if blocked and "block" in req["allowed_effects"]
                else []
            )
    else:
        reply.update(status="error", error_code="unsupported_method")

    json.dump(reply, sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
```

### 2. Smoke-test outside AW

Pipe `describe` and `validate_config` requests directly to stdin and confirm
exit status `0` with valid JSON on stdout:

```bash
printf '%s\n' '{"api_version":"aw-provider/v1alpha1","method":"describe","request_id":"req-1"}' \
  | /usr/bin/python3 /absolute/path/to/my-provider.py
printf '%s\n' '{"api_version":"aw-provider/v1alpha1","method":"validate_config","request_id":"req-2","config":{"blocked_tools":["Bash"]}}' \
  | /usr/bin/python3 /absolute/path/to/my-provider.py
```

### 3. Wire into `aw.yaml` and verify

Declare the Provider with `protocol: aw-provider/v1alpha1` and `transport.argv`,
then attach its operation to an event step:

```yaml
apiVersion: aw/v1alpha1
kind: AWConfiguration
metadata:
  name: custom-provider-demo
spec:
  daemon:
    startup: on_demand
    endpoint: auto
    state_dir: /absolute/path/to/aw-state
  execution:
    guarantee: native_hook
    default_event_budget_ms: 5000
  audit:
    enabled: true
    payload: metadata_only
  agents:
    qoder:
      adapter: qoder
      argv: [/absolute/path/to/qodercli]
  providers:
    my-policy:
      protocol: aw-provider/v1alpha1
      transport:
        type: stdio
        location: agent
        argv: [/usr/bin/python3, /absolute/path/to/my-provider.py]
      timeout_ms: 1000
      max_output_bytes: 65536
      config:
        blocked_tools: ["Bash"]
  events:
    tool.before:
      enabled: true
      required: true
      steps:
        - id: check-blocked-tools
          provider: my-policy
          operation: check
          effects: [observe, block]
          on_error: block
```

Validate the configuration, then test allow, block, timeout and failure
handling against a real Agent (or locally with the [integration tutorial](../integration.md)):

```bash
aw config validate --config /absolute/path/to/aw.yaml
aw run --config /absolute/path/to/aw.yaml --agent qoder
```

### 4. Deploy independently

Ship the executable and its dependencies through your own release process,
pointing `transport.argv` to its installed absolute path (no AW package record
is required), or optionally package it as a local bundle for `aw install --bundle`
([local Provider package format](../reference/packages.md)). Document any
backend credentials, sockets and data directories alongside your Provider.

### Common Provider patterns

| Pattern | Events & effects | Reference |
| --- | --- | --- |
| Safety / policy / semantic checking | `input.submit` or `tool.before` with `observe`, `block` | [sec-core](sec-core.md) · [policy.rs](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-provider/examples/policy.rs) |
| Command rewrite & output compression | `tool.before` with `replace_input`; `tool.after` with `replace_result` | [Tokenless](tokenless.md) |
| Checkpointing / audit observation | `tool.before` and `tool.after` with `observe` (external state capture or telemetry) | [Provider protocol](protocol.md) |

Providers return candidate effects; the Adapter adopts them in the native framework.
An implementation does not need to link AW or use Rust. The native framework
continues to manage the user's Agent and model configuration.
See the [Provider protocol](protocol.md) for full request/response schemas,
budgets, error codes and capability admission rules.

## Contribute an implementation

Submit under `providers/<provider>/` with a maintainer, source and license,
bilingual instructions, and offline tests. Each Rust package owns its workspace and lockfile and follows the
[layout checks](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/scripts/check_layout.py). Wire new tests into
the AW gate; see the [contribution contract](../../../../../providers/README.md).

Build and test the first-party Providers from the repository root:

```bash
cargo build --locked --manifest-path providers/agent-sec-core/Cargo.toml
cargo test --locked --manifest-path providers/agent-sec-core/Cargo.toml
cargo build --locked --manifest-path providers/tokenless/Cargo.toml
cargo test --locked --manifest-path providers/tokenless/Cargo.toml
```

The bundled workspaces currently use repository-local AW library paths; Tokenless
composition tests also depend on the sec-core bridge. These implementations cannot
yet build outside the repository. Pinned SDKs, independent releases and artifact
composition tests are deferred. Integration builds test framework, Provider and
backend versions together; local extensions retain independent IDs and versions.

`make -C distribution aw-packages VERSION=0.2.2 COMPONENTS='core sec-core tokenless' OUTPUT="$PWD/target/aw-packages"` invokes the scripts in
`distribution/packaging/aw/` to build independent core and Provider archives.
Local Provider extension
installation accepts verified independent `package.json` bundles in the
[local Provider package format](../reference/packages.md), but does not
download community Providers or resolve product names. `aw-package` keeps its
Provider file transactions private; generic software and native packages retain
their actual installation manager. An externally installed Provider connects through its declared
`transport.argv`; protocol use does not require an AW-owned installation record.

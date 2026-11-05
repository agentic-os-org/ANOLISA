# Gateway application composition root

## Decision and ownership

`cosh-gateway-app` is the binary-only owner of the installed `cosh-gateway`
entrypoint. The cosh-ng Gateway application owner maintains CLI presentation,
serve-time wiring, and concrete checkpoint/OS adapter selection. The Gateway
library owner retains Task policy, persistence, scheduling, and the local
protocol. This split moves the former `cosh-gateway/src/bin` composition code;
it does not introduce a second policy implementation.

## Callers and dependencies

Current callers are the `cosh agent` wrapper, the packaged systemd unit,
`cosh-shell` Task commands, and direct CLI automation. The Linux Web adapter
is an internal presentation module. No future Rust library caller is assumed;
additional presentation clients should use the Gateway protocol and contracts.

Internal dependency edges are:

```text
cosh-gateway-app -> cosh-gateway -> cosh-gateway-contracts
                -> cosh-gateway-contracts
                -> cosh-platform -> cosh-types
                -> cosh-types
```

The application connects Gateway policy to concrete platform side effects.
`cosh-gateway` must not acquire an internal dependency on `cosh-platform`,
`cosh-types`, or the application. OS execution remains outside the contracts
leaf. Shell integration invokes the installed command rather than depending
on application internals.

## Public surface and tests

There is no library target and no exported Rust API. The executable exposes
`doctor`, `run`, `serve`, `task`, and `admin`; Linux also exposes the experimental
`web` command. CLI flags, JSONL presentation, and exit codes are maintained in
the application. Wire DTOs remain in Gateway/contracts rather than being
republished by the binary.

Private parsing, admission, token, and adapter component checks stay next to
their owners. Installed-command and subprocess checks live in
`crates/cosh-gateway-app/tests/`. Gateway policy/storage regressions stay in
`cosh-gateway`; platform side effects stay with their adapters. Real providers,
packaged systemd execution, and manual Terminal acceptance remain separate
installation-level checks, not implied by a passing unit suite.

The Web command is Linux-only because credential validation resolves an opened
file descriptor through procfs. It queries daemon capabilities before listening,
binds the workspace through the existing canonical path/device/inode digest,
and rejects delegated or unbrokered Runtime authority, including unavailable
entries. The built-in Core/Codex catalog currently fails that admission check;
there is no admitted production Web configuration in this release. Moving a
same-user token outside the workspace is insufficient isolation. The former
caller-supplied capability-profile label has been removed.

## Alternative and revisit conditions

Keeping the binary inside `cosh-gateway` would make its package depend on the
platform execution layer, weakening the library's contracts-only internal
boundary. Feature-gating those dependencies would retain the same architectural
edge and add configuration variants. A binary-only composition root keeps the
existing wiring explicit without adding an interface or a new shared library.

Revisit when another executable needs the same concrete adapter wiring, when a
non-process consumer needs an application API, or before adding new internal
dependency edges. Extract only the demonstrated shared responsibility then.
Enabling Web requires a separately validated restricted Runtime and attestation
contract, including the lifetime of that boundary; changing a CLI declaration
or hiding an unavailable Runtime is not sufficient.

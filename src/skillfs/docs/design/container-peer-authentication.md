# SkillFS Container Peer Authentication

[中文版](container-peer-authentication_zh.md)

Development plan for [issue #2439](https://github.com/alibaba/anolisa/issues/2439).
This document defines planned behavior; it must not be read as an available
deployment contract until the implementation and acceptance items are complete.

## Current development status

The development branch implements only the SkillFS-owned surfaces: shared
authentication primitives, the control server, the notify client, fail-closed
configuration gates, documentation, regression tests, and an independent
standard-library Python probe. Fixed handshake and protected-frame vectors,
plus a real FUSE probe loop, pin the proposed cross-language byte contract.

No agent-sec-core source or tests are changed by this branch. Peer-side
implementation and ownership remain with the sec-core maintainers after they
confirm the contract. The real sec-core separate-container fixture, the
security-integrated Pod profile, and ACK evidence remain open. This issue must
stay open until those cross-component and deployment acceptance items pass.

## Outcome

Allow SkillFS and agent-sec-core to authenticate their private Unix-socket
traffic while running in separate PID and mount namespaces. Preserve the
current executable-identity authentication as the unchanged host default.

The first container profile retains the existing `shared_path` resolver
transport. SkillFS and agent-sec-core mount the same physical source at the
same absolute path, while the workload sees only the propagated FUSE view.

## Security boundary

The trusted domain contains the SkillFS and agent-sec-core containers. A
Kubernetes Secret volume and the physical source are mounted only into this
domain. A private runtime volume carries their Unix sockets.

An untrusted workload may know the socket path and may intentionally be given
read-only access to the runtime volume in a negative test. It must still fail
authentication when it lacks the Secret, including when it uses the same UID,
GID, process name, or executable basename as the trusted peer. Write access to
the socket directory remains inside the trusted domain: any writer can unlink
or replace an endpoint and cause a detectable denial of service.

Node root, a compromised trusted container, and a peer that can read the
Secret are outside this boundary.

## Profiles

### Host executable profile

This remains the default. SkillFS verifies `SO_PEERCRED`, `/proc/<pid>/exe`
path and file identity, configured UID/GID, and process start time. Existing
CLI, configuration, wire format, and failure behavior stay unchanged.

### Container HMAC profile

This profile is explicit and mutually exclusive with executable identity.
SkillFS and agent-sec-core load the same secret from an absolute, nonblocking,
no-follow, bounded regular file with restrictive permissions. Nonblocking open
allows FIFOs and other non-regular candidates to fail metadata validation
instead of stalling startup. UID/GID remain optional additional constraints;
they are not treated as container identity.

Before each authenticated notify connection, SkillFS requires the socket's
immediate parent to be a non-symlink directory owned by its effective UID with
no group or other permissions; `0700` is the recommended default, while more
restrictive usable owner permissions such as `0300` remain valid. The endpoint
must be an owner-matched Unix socket with no group or other permissions; the
agent-sec-core listener therefore binds it as `0600`. Because agent-sec-core
creates both paths, the initial profile requires SkillFS and agent-sec-core to
run with the same effective UID. Supporting different UIDs requires a future,
explicit endpoint ownership policy. These metadata checks provide a first
availability and deployment boundary. The HMAC exchange remains the peer
identity and business-frame integrity boundary, including against a same-UID
process.

Each connection completes a bounded challenge-response exchange before an
existing business request is read or dispatched:

1. The client sends a bounded `auth.init` frame.
2. The server sends a fresh cryptographically random nonce.
3. The client returns a domain-separated HMAC-SHA256 proof.
4. The server compares the proof in constant time and returns its own
   domain-separated proof.
5. The client verifies the server proof before sending business data.
6. Each sender writes the existing raw NDJSON business frame followed by an
   `auth.frame` tag. The receiver verifies the tag before decoding or
   dispatching the business frame.

Control and notify traffic use distinct domains. A connection is single-use,
so reconnect and process restart always require a fresh nonce. Authentication
errors close the connection without falling back to executable identity or
plain protocol handling. Binding each business frame to the fresh nonce and
sender direction prevents a socket proxy from relaying the handshake and then
altering a request, response, or acknowledgement.

The handshake uses a total deadline, not a timeout that restarts after each
byte. This bounds shutdown latency and prevents a peer from holding the
single-request control loop indefinitely with a slow partial frame. Socket
ownership and optional UID/GID constraints remain the first availability
boundary; the shared secret authenticates the peer after connection.

The authentication frames are NDJSON and use the following fixed envelope:

```json
{"authVersion":"1","type":"auth.init"}
{"authVersion":"1","type":"auth.challenge","nonce":"<base64>"}
{"authVersion":"1","type":"auth.proof","proof":"<base64>"}
{"authVersion":"1","type":"auth.ok","proof":"<base64>"}
<existing raw business JSON>
{"authVersion":"1","type":"auth.frame","proof":"<base64>"}
```

The nonce is 32 random bytes encoded with padded standard Base64. Each proof
is `HMAC-SHA256(secret, domain || NUL || raw_nonce)`, also encoded with padded
standard Base64. The domains are:

- `anolisa.skillfs.control.client.v1`
- `anolisa.skillfs.control.server.v1`
- `anolisa.skillfs.notify.client.v1`
- `anolisa.skillfs.notify.server.v1`

For a business payload, the tag input is:

```text
domain || NUL || "frame" || NUL || raw_nonce ||
u64_be(payload_length) || raw_business_json
```

The tag is HMAC-SHA256 under the shared Secret and uses the same client or
server domain as its sender. The payload length excludes the NDJSON newline.
The sender transmits the raw business JSON and newline first, then the
`auth.frame` line. The receiver retains the raw bytes, verifies the tag in
constant time, and only then parses or dispatches them. This avoids
cross-language JSON canonicalization while keeping the inner control schema v1
and notify schema v2 unchanged.

Secret material and reusable proofs must never appear in logs, responses,
audit events, protocol events, or checked-in deployment assets.

## Implementation phases

### Phase 1: shared authentication primitive

- Add strict secret-file loading and validation in SkillFS and agent-sec-core.
- Define compatible challenge, proof, and protected business frames; byte
  limits; timeouts; domain strings; and fixed cross-language test vectors.
- Add constant-time proof verification and failure redaction.

### Phase 2: control resolver

- Add a mutually exclusive HMAC peer mode to the SkillFS control server.
- Add explicit socket and secret paths to the agent-sec-core resolver client.
- Authenticate before `ping`, `status`, resolver, or activation method
  dispatch while leaving the business schema unchanged.
- Retain the existing Flat, Hermes, fd-anchored resolution, and error mapping.

### Phase 3: notify direction

- Authenticate SkillFS as a client of the agent-sec-core daemon.
- Require authentication only for `skill_ledger.skillfs_notify_change` when
  the hardened mode is configured; unrelated daemon APIs keep their existing
  compatibility behavior.
- Authenticate the daemon response so a fake listener cannot acknowledge a
  notification.
- Fail startup when container HMAC control and notify are enabled together but
  the notify key is omitted, avoiding a partially authenticated profile.

Notification retry and durable reconcile are separate work. Authentication
failure retains the current rule that normal FUSE I/O continues and the active
mapping does not change.

### Phase 4: deployment and local acceptance

- Add a separate security-integrated Pod profile rather than changing the
  standalone Sidecar example.
- Use distinct source, propagated FUSE, runtime socket, and Secret volumes.
- Do not enable `shareProcessNamespace`.
- Add positive and negative local container tests with separate namespaces.
- Verify restart, readiness, resolver, notify, activation, denied workload,
  and clean-unmount behavior.

## Local acceptance

Run from `src/skillfs`:

```sh
cargo +1.86.0 fmt --all -- --check
cargo +1.86.0 clippy --workspace --all-targets -- -D warnings
cargo +1.86.0 test --workspace
cargo +1.86.0 doc --workspace --no-deps
scripts/test.sh
```

Run the independent probe fixed-vector check and unilateral real-FUSE plan:

```sh
python3 scripts/container-peer-auth-probe.py self-test
```

The sec-core formatter, lint, type, pytest, and bilateral container fixtures
belong to its follow-up implementation. Those fixtures must fail rather than
skip authentication or namespace cases.

Required negative cases include missing, empty, short, oversized, symlinked,
FIFO, over-permissive, and incorrectly owned secret files; wrong, malformed,
stale, or replayed proofs; a relayed handshake followed by a modified business
frame; authentication timeouts; UID/GID mismatch; a plain request against HMAC
mode; insecure notify socket type, owner, mode, or parent directory; and a
same-UID untrusted peer with socket access but no Secret.

## ACK follow-up

After local completion, run a focused one-off validation on ACK and record:

- Kubernetes version, runtime, node architecture, manifest revision, and image
  digests;
- Secret, source, runtime, and propagated-volume visibility from every
  container;
- separate PID and mount namespaces without shared process namespace;
- resolver, notify, activation, readiness, and both sidecar restart paths;
- denial of an untrusted container that can reach the runtime socket; and
- Pod termination and residual-mount cleanup.

ACK results are release evidence, not recurring CI evidence. Do not describe
the security-integrated profile as released until this validation and the
remaining release gates in issue #2012 are complete.

## Deferred work

- `SCM_RIGHTS` or directory-fd resolver transport.
- Removing the physical source from agent-sec-core.
- Shared PID namespace as a supported security dependency.
- Durable notify queues or reconnect reconciliation.
- Multi-source registration, source hot refresh, CSI, and rootless FUSE.

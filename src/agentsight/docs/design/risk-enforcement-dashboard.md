# Risk-Enforcement Dashboard Design

[中文版](risk-enforcement-dashboard_zh.md)

## Status

Approved for implementation. This design adds a standalone **Risk Enforcement**
navigation entry to AgentSight. It uses AgentSight's enforcement coordinator and
the ActPlane-backed `agentsight-enforcer`; it does not depend on AgentSecCore.

The first release is deliberately limited to blocking a selected Agent process
tree from opening one existing sensitive file. Taint propagation, arbitrary
ActPlane DSL, `unless`/`since` temporal rules, network enforcement, approval
flows, and incident ownership are outside this release.

## User Outcome

An operator can complete one enforcement loop from the Dashboard:

1. verify that the privileged enforcer is ready;
2. bind a file-open blocking policy to a live Agent PID;
3. see ActPlane acknowledge the binding as `enforced`;
4. inspect normalized violations produced by denied file opens;
5. detach the binding and restore access.

The page remains readable when the enforcer is unavailable. Historical binding
state and violations remain visible, while mutating actions are disabled with
an actionable readiness message.

## Navigation and Layout

Add `/enforcement` as a top-level route named `风险拦截`, adjacent to but
independent from `/security`. The existing `/security` route continues to
represent AgentSecCore-backed security observability.

The page uses the existing Dashboard visual system and a single-screen
operations-console layout:

- summary cards for enforcer readiness, backend name, active bindings, and
  blocked violation count;
- an active-binding table with state, Agent, PID, path, policy revision, and a
  guarded detach action;
- a create-file-policy form containing only product-level fields;
- a newest-first violation table with time, Agent, PID, operation, target,
  effect, result, and binding identity.

The create form accepts `agent_id`, optional `session_id`, `root_pid`, and an
absolute `path`. It never exposes binding UUID generation, Linux process start
time, policy revision, or ActPlane DSL to the user.

## API Boundary

The page reads the existing endpoints concurrently:

```text
GET /api/enforcement/health
GET /api/enforcement/bindings
GET /api/enforcement/violations?limit=100
DELETE /api/enforcement/bindings/{binding_id}
```

Add one product-level endpoint for the form:

```http
POST /api/enforcement/file-bindings
Content-Type: application/json

{
  "agent_id": "qoder",
  "session_id": "optional-session",
  "root_pid": 45231,
  "path": "/root/.ssh/id_rsa"
}
```

The server converts this request into the existing versioned `ApplyPolicy`
protocol. The raw `POST /api/enforcement/bindings` endpoint remains available
for trusted integration clients but is not called by the Dashboard.

## Server-Side Policy Construction

The server performs the following steps before calling the coordinator:

1. trim and validate the Agent identity;
2. require an absolute, existing regular file and canonicalize its path;
3. read `/proc/<pid>/stat`, reject PID 0/1 and the AgentSight services, and
   capture field 22 as the process start time;
4. generate a binding UUID;
5. derive a stable policy identifier from the binding and use product template
   revision `agentsight-file-open-v1`;
6. escape the canonical path as an ActPlane quoted string and build exactly one
   rule with `block open file` and no taint or temporal clause;
7. call `EnforcementCoordinator::apply` and return only after the privileged
   backend acknowledges the binding.

The policy template is owned by the server so the frontend and ActPlane adapter
cannot drift independently. Input containing a NUL byte, a non-file target, an
unreadable process identity, or an unsafe quoted-string sequence is rejected
before persistence or attachment.

## Page Data Flow

On entry and manual refresh, the page loads health, bindings, and violations in
parallel. One failed request does not erase successful data from the other
panels. A successful create or detach operation refreshes all three resources.
The submit button is disabled while a request is in flight; detach requires a
confirmation dialog and is disabled unless the enforcer reports ready.

Bindings in `pending`, `failed`, `degraded`, or `detaching` remain visible with
their backend message. Detached bindings are retained as audit state but are
not counted as active. Violations are displayed as immutable facts and are not
editable or dismissible in this release.

## Error Semantics

The REST boundary preserves actionable categories:

| Status | Meaning | Page behavior |
|---|---|---|
| `400` | Invalid PID, path, or request field | Keep the form and show the field-level reason |
| `404` | Process, file, or binding no longer exists | Refresh state after showing the reason |
| `409` | Conflicting binding state | Keep current state and ask the operator to refresh or detach |
| `422` | ActPlane compilation or attachment rejection | Show the backend reason without retrying automatically |
| `503` | Enforcer or BPF-LSM unavailable | Mark readiness degraded and disable mutations |

Authentication continues to use the existing Dashboard session cookie. The
browser never communicates with the privileged UDS directly.

## Testing and Acceptance

Implementation follows red-green-refactor.

Backend tests cover request validation, `/proc` identity extraction, canonical
path handling, ActPlane string escaping, policy-template output, conversion to
`ApplyPolicy`, and HTTP status mapping. Frontend tests cover the navigation
entry, independent unavailable state, successful refresh, create success and
failure, guarded detach, and normalized violation rendering.

Required checks are Dashboard unit tests, TypeScript type checking, embedded
frontend build, AgentSight Rust formatting, Clippy, and tests. Final acceptance
runs on a real Linux host with BPF LSM enabled:

1. deploy the branch and start both AgentSight and `agentsight-enforcer`;
2. bind a live test process to an existing sensitive file;
3. verify the page reports `enforced`;
4. verify opening the file returns `EPERM`;
5. verify the violation appears in the page with the correct binding and path;
6. detach from the page and verify the process can open the file again.

Deployment backs up the installed binaries and unit files, preserves the
existing AgentSight databases, and verifies that the Dashboard still answers on
port 7396 after restart.

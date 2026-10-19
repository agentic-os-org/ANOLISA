# Secret Gateway

[中文版](../../zh/agent-sec-core/secret-gateway.md)

The secret gateway keeps real API keys out of the Agent process. Agents start independently;
root registers their UID and PID, and each Agent loads a same-shaped fake through its own
configuration. When the Agent sends that fake to an allowed endpoint, the gateway swaps in the
real key on the wire. The fake key is useless
anywhere else. A prompt-injected Agent that prints its environment, writes it to a file or sends
it to another host leaks only the fake.

This release protects one credential: a GitHub token (`github` preset). It covers the GitHub REST
API and HTTPS Git. User-defined rules, other providers, content inspection and audit export are
not available.

## How it works

- **Agent domain**: a subset of one UID's processes in a named systemd scope
  (`agentsec.slice/agent-sec-agent-<name>.scope`). Root registers an existing Agent's UID and PID;
  the daemon verifies process identity and moves it and a snapshot of existing descendants into
  the scope. Children created inside the domain inherit its membership.
- **Routing**: the daemon installs the nftables table `inet agent_sec_gateway`. It redirects
  outbound TCP ports 80 and 443 from each Agent domain to the gateway, and drops UDP port 443 so
  clients fall back from HTTP/3 to TCP. Loopback traffic and processes outside the domains are not
  affected.
- **Gateway**: `agent-sec-gateway.service` runs the pinned mitmproxy standalone build as the
  unprivileged `agent-sec-gw` user, in transparent mode. Each Agent domain uses its own port in
  `15001`–`15008`, so at most 8 domains can be configured.
- **Keys**: the root daemon stores the real key under `/var/lib/agent-sec/credentials` and
  publishes only the entries the gateway needs into `/run/agent-sec-gateway`. Real keys never
  enter the Agent environment, command line or logs.

The `github` preset injects the real token only for these requests:

| Host | Path | Methods |
|------|------|---------|
| `api.github.com` | any | GET, HEAD, POST, PUT, PATCH, DELETE |
| `github.com` | `…/info/refs` | GET |
| `github.com` | `…/git-upload-pack` | POST |
| `github.com` | `…/git-receive-pack` | POST |

The Agent presents the fake in the `Authorization` header, as a token or as the HTTP Basic
password. Requests without a fake in that header are forwarded unchanged. The gateway rejects a
request with a JSON body `{"error": "<code>", "source": "agent-sec-gateway"}`, status `403` for
the codes below, `421` for `sni_host_mismatch` and `500` for an internal `gateway_error`. Common
codes:

| Code | Trigger |
|------|---------|
| `credential_not_allowed` | the fake goes to a host, path or method outside the table above |
| `plaintext_credential` | the fake goes over plain HTTP |
| `real_credential_exposed` | the headers, path or body contain the real token |
| `subject_mismatch` | the fake belongs to another Agent domain |
| `retired_credential` | the fake was withdrawn by `key remove` |
| `gateway_not_ready` | the gateway has not loaded a configuration for this domain |

On allowed REST and Git ref/push responses, the gateway replaces any echoed real token with the
fake. Git fetch pack data streams through unchanged.

## Requirements

- Linux with systemd, cgroup v2 and nftables (`nft`); x86_64 or aarch64.
- The root `agent-sec-core.service` daemon from the Rust V2 core. The gateway is not part of the
  V1 Python CLI. Released `anolisa install sec-core` and RPM packages do not yet include it.
- Programs in the Agent domain must trust the gateway CA as described under
  [Agent credential and CA configuration](#agent-credential-and-ca-configuration). Programs
  that pin certificates or ignore those variables need their own supported CA configuration;
  otherwise they cannot reach HTTPS hosts through the gateway.

## Installation

Build and install the V2 core as described in the [SkillSec V2 guide](skillsec-v2.md#installation-boundary),
then stage the gateway files from the repository root:

```sh
sudo make -C src/agent-sec-core install-gateway
sudo /usr/share/anolisa/sec-core/gateway/install-mitmproxy.sh
sudo systemctl daemon-reload
sudo systemctl restart agent-sec-core.service
```

`install-gateway` installs the gateway addon, `agent-sec-gateway.service` and the daemon drop-in
`agent-sec-core.service.d/gateway.conf`, which adds `CAP_NET_ADMIN` for nftables.
`install-mitmproxy.sh` downloads the pinned mitmproxy 12.2.3 archive from
`downloads.mitmproxy.org`, verifies its SHA-256, installs it under `/opt/agent-sec-gateway` and
creates the `agent-sec-gw` system user. On hosts without Internet access, download the archive
named in `mitmproxy.provenance` elsewhere and pass it with `--archive PATH`.

## Registering an Agent and configuring a key

Agents run through their own command, service or container entry point; the gateway does not
launch them. Administration requires kernel-authenticated UID 0. Non-root policy administrators
cannot register domains, manage keys or enable/disable the gateway.

Obtain the existing Agent's PID and actual UID first. This example assumes PID `4242` and UID
`1000`; replace them with actual values. These commands change cgroups, credentials and service state:

```sh
sudo agent-sec-cli gateway register --agent dev --uid 1000 --pid 4242
sudo sh -c 'umask 077 && cat > /root/github.token'   # paste the token, then press Ctrl-D
sudo agent-sec-cli gateway key add github --agent dev --secret-file /root/github.token
sudo agent-sec-cli gateway enable
```

- `--agent` is a globally unique lowercase DNS label for a subset of one UID's processes.
- `register` verifies process identity and moves the target and a snapshot of existing descendants
  into the domain's cgroup. It does not start or restart the Agent. The UID cannot be root,
  `agent-sec-gw` or a member of the gateway group, which can read real keys.
- Registration works while the gateway is disabled and before keys are configured. Keys require
  a registered domain. Removing its last key retains registration; `unregister` deletes the domain
  and revokes all its keys without stopping Agent processes.
- Registering another PID under the same UID is allowed. Changing a domain's UID requires
  unregistering first. After a process restart, root must register the new PID.
- The real-token file must belong to root and have no group or other permissions. The daemon reads
  it directly; the token never travels over the CLI socket.
- Each domain gets its own fake. All domains using the same preset share one real token; adding a
  different token rotates the real value for every domain while keeping their fakes.

Key changes publish immediately while enabled. If revocation cannot be confirmed, the daemon
stops the gateway rather than leaving the old key usable. Configuration survives daemon restarts;
an enabled gateway recovers in the background. PIDs are not persisted or used to reattach processes
on recovery.

Read-only and administration commands:

```sh
agent-sec-cli gateway status             # read-only: only your UID's domains; root sees all
agent-sec-cli gateway credentials --agent dev  # read-only: your domain's fakes and CA paths
sudo agent-sec-cli gateway key list       # read-only, root only: abbreviated fakes
sudo agent-sec-cli gateway key remove github --agent dev
sudo agent-sec-cli gateway unregister --agent dev
sudo agent-sec-cli gateway disable        # stops the gateway, removes routing and runtime files
```

## Agent credential and CA configuration

`credentials` returns JSON `{agent, scope, environment}` containing only the domain's fakes and
CA paths. It never moves a process. Ordinary users can read only their UID's domains; root can
read all. Requests cannot select another caller UID.

Load the returned `environment` through the Agent's own credential or environment configuration:

- `GITHUB_TOKEN`, `GH_TOKEN`: the domain's fake token;
- `SSL_CERT_FILE`, `REQUESTS_CA_BUNDLE`, `CURL_CA_BUNDLE`, `GIT_SSL_CAINFO`, `NODE_EXTRA_CA_CERTS`,
  `DENO_CERT`: `/run/agent-sec-gateway/ca-bundle.pem`, the system bundle plus the gateway CA,
  available after a successful enable.

Registration cannot change a running process's environment. Use the Agent's supported reload
mechanism to load the fake and CA. Programs that load settings only at startup must restart through
their own mechanism and have their new PID registered. The gateway does not write user profiles,
install a system CA, replace Git helpers or remove proxy variables. Configure HTTPS Git's own
credential helper to return the fake as its password; avoid helpers supplying real tokens. Check
and remove `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` and lowercase variants that bypass transparent routing.

Loading fake/CA settings does not classify a process. Only new connections from registered domain
processes and their descendants pass through the gateway.

## Limits

- Registration captures a snapshot of existing descendants; it cannot guarantee coverage of children
  forked concurrently by processes not yet moved. Register while the Agent is idle, before starting
  work. Earlier traffic and established connections are not retroactively protected. A failed move
  may leave some processes already migrated; an error is not complete admission. Inspect and retry.
- Agent and gateway startup order is independent. Complete injection requires registration, key
  configuration, enable and the Agent's own fake/CA configuration before opening new connections.
  An unrouted fake can leave the host; this is not a blocked request.
- Only TCP ports 80 and 443 are routed. A fake sent to another port leaves the host unchanged and
  is useless to the receiver.
- The gateway listens on IPv4 loopback only, so IPv6 connections to ports 80 and 443 from an
  Agent domain fail. Clients that try IPv4 next keep working.
- Agent domains that run as the same user are not isolated from each other: that user can read
  the fakes of all of them.
- Every removed fake stays recorded so that it keeps being rejected; the store holds at most 4096
  fakes in total, removed ones included.
- SSH Git remotes, GitHub Enterprise, `uploads.github.com` and raw content hosts receive the fake
  only.
- The gateway inspects traffic only for the fake and real token values. It does not filter other
  content and does not record traffic; rejections are logged without token values.

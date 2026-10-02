# AgentSight Deployment

[中文版](../../../zh/agent-observability/agentsight/deployment.md)

AgentSight runs as two workers: `agentsight trace` (eBPF capture, needs root) and
`agentsight serve` (API + Dashboard). Every deployment form below is a different way to start those
two.

## Requirements

| Requirement | Value |
|---|---|
| OS | Linux x86_64 |
| Kernel | >= 5.8 with BTF (`/sys/kernel/btf/vmlinux` must exist) |
| Privileges | root, or `CAP_BPF` + `CAP_PERFMON` |
| Build toolchain (source only) | Rust >= 1.80, clang/llvm >= 15, libbpf >= 0.8, Node.js for the Dashboard |

clang 14 and older optimise away a length clamp the eBPF verifier requires, so `sslsniff` and
`tcpsniff` fail to load. Use clang 15+ when building from source.

## Packaged install with systemd (recommended)

```bash
sudo anolisa install agentsight        # or: sudo yum install agentsight
sudo systemctl enable --now agentsight.service
```

Two units are installed:

| Unit | Role |
|---|---|
| `agentsight.service` | Runs `/usr/local/bin/agentsight-start`, which supervises `agentsight trace` and `agentsight serve --host 0.0.0.0` |
| `agentsight-enforcer.service` | Optional ActPlane enforcement daemon; pulled in by the main unit and required for the Risk Enforcement page |

What the packaged unit gives you:

- `Restart=always` with `RestartSec=10` and no start rate limit, so a repeated crash or OOM kill can
  never leave the host permanently unobserved. The trade-off is visible: a service that cannot start
  at all keeps retrying every 10 s and says so in the journal, rather than going quiet;
- `OOMPolicy=continue`, so one OOM-killed worker does not tear down the whole unit — the supervisor
  stops its sibling and exits, and `Restart=always` brings both back. Requires systemd 243+; older
  releases (Anolis 8 / systemd 239) log an unknown-key warning, ignore the directive and fall back to
  stopping the unit, which `Restart=always` still recovers from;
- `CPUQuota=30%` and `MemoryMax=350M`, sized for the default `runtime_limits`;
- `UMask=0077`, so data under `/var/log/sysak/.agentsight` is root-only;
- `systemctl reload` sends `SIGHUP`, and the supervisor restarts both workers so they re-read
  `config.json` — no full restart needed.

```bash
systemctl status agentsight.service
journalctl -u agentsight.service -n 50 --no-pager
sudo systemctl reload agentsight.service     # after editing config.json
```

Because the unit binds the Dashboard to `0.0.0.0`, restrict TCP 7396 in your firewall or cloud
security group before exposing the host.

## Foreground run for troubleshooting

Only one tracer should run at a time, so stop the service first:

```bash
sudo systemctl stop agentsight.service

# terminal 1
sudo agentsight trace -v

# terminal 2
sudo agentsight serve
```

Restore normal operation with `sudo systemctl start agentsight.service`.

## Source build

```bash
cd src/agentsight

# Anolis / Alibaba Cloud Linux / CentOS / RHEL
sudo yum install -y openssl-devel elfutils-libelf-devel perl-IPC-Cmd libbpf-devel clang llvm bpftool

make build-all          # Dashboard frontend + agentsight + agentsight-enforcer
sudo ./target/release/agentsight trace &
sudo ./target/release/agentsight serve --host 0.0.0.0
```

`make build` alone skips the enforcer and `serve` then logs `AgentSight enforcement unavailable` at
every start.

## Containers and sidecars

eBPF probes need capabilities the default container profile does not grant:

```bash
docker run --cap-add CAP_BPF --cap-add CAP_PERFMON \
  -v /sys/kernel/btf:/sys/kernel/btf:ro \
  -p 7396:7396 <image>
```

The ANOLISA container entrypoint (`docker/docker-entrypoint.sh`) already follows this rule: it
checks for `cap_bpf`/`cap_sys_admin`, starts `agentsight-start` when they are present, and otherwise
prints the exact `docker run` flags to add instead of failing silently.

For a Kubernetes sidecar, the same three things matter:

1. capabilities — `securityContext.capabilities.add: ["BPF", "PERFMON"]`, or `privileged: true` on
   platforms that do not support fine-grained eBPF capabilities;
2. host visibility — the sidecar must share the Agent container's PID namespace
   (`shareProcessNamespace: true`) so it can see Agent processes and attach uprobes;
3. BTF — mount `/sys/kernel/btf` read-only from the host.

**Persistence (mandatory)**: mount a volume at `/var/log/sysak/.agentsight`, or every container restart wipes
all captured data — the writable layer is recreated from the image on restart (`kubectl exec ... pkill`, OOM,
image updates, node drains all trigger it). Prefer `hostPath` for host-scoped observation data; use a PVC only
when the data truly needs to follow the pod across nodes (note the data then lives and dies with the pod);
`emptyDir` survives container restarts but not pod recreation.

```yaml
# sidecar reference shape (excerpt)
volumes:
  - name: agentsight-data
    hostPath:
      path: /var/log/sysak/.agentsight
      type: DirectoryOrCreate
containers:
  - name: agentsight
    # ...capabilities / shareProcessNamespace as above
    volumeMounts:
      - name: agentsight-data
        mountPath: /var/log/sysak/.agentsight
```

> Permission note: the databases contain full prompts and responses. `DirectoryOrCreate` creates the host
> directory root-owned with mode 0755, and the sidecar does not inherit the packaged service's `UMask=0077`.
> Pre-create the directory with `chmod 700` (or tighten it from an initContainer) before production use so
> other host users cannot read conversation content.

Keep the Dashboard port inside the cluster and reach it through a Service or port-forward rather
than exposing 7396 publicly.

## Kubernetes DaemonSet (node-wide)

A DaemonSet gives you one AgentSight pod per node observing every Agent process on that node —
the sidecar form above watches one pod, this form watches the whole machine. The manifest lives at
`src/agentsight/packaging/k8s/daemonset.yaml`; build the image with
`src/agentsight/packaging/docker/Dockerfile`, push it to your registry, and set the `image:` field
accordingly.

How it works: `hostPID: true` places the pod in the host PID namespace and the probes attach
host-wide (uprobes register by inode, so Agent binaries inside other pods are covered), while PID
attribution stays correct because AgentSight reports PIDs in the observer namespace.

```bash
kubectl apply -f src/agentsight/packaging/k8s/daemonset.yaml
kubectl -n agentsight rollout status ds/agentsight
kubectl -n agentsight port-forward ds/agentsight 7396:7396   # then open http://localhost:7396
```

The manifest requests `BPF` + `PERFMON` + `SYS_PTRACE` capabilities (the last one is needed to
read `/proc/<pid>/maps` of another uid's process once it is non-dumpable; without it such Agents
are silently skipped), mounts host `/sys/kernel/btf` read-only, and persists data to hostPath
`/var/log/sysak`; resource limits mirror the packaged systemd unit (300m CPU / 350Mi memory). It
talks to no Kubernetes API, so no RBAC is needed. The pod runs only the trace and serve workers
(`agentsight-start`), so the Risk Enforcement page has no data in this form. To override
`config.json`, create the `agentsight-config` ConfigMap from the shipped default and uncomment the
marked blocks in the manifest.

A hostPID-free form (bind-mounting the host procfs instead) is planned once the procfs-root work
lands; it is not available yet — keep `hostPID: true` until then.

## macOS

macOS builds contain no eBPF. Two commands exist:

| Command | Behaviour on macOS |
|---|---|
| `agentsight trace` | Trajectory collector only: scans local Agent JSONL session files, converts them to ATIF v1.7, stores them in `trajectories.db` |
| `agentsight serve` | Dashboard and trajectory viewer over that database |

```bash
cd src/agentsight && make build-mac
./target/release/agentsight trace     # terminal 1
./target/release/agentsight serve     # terminal 2
```

`--db` and `--config` are Linux-only, and Token/audit/interruption commands are unavailable.

## Upgrade

```bash
sudo systemctl stop agentsight.service
sudo yum update agentsight            # or: sudo anolisa install agentsight
sudo systemctl start agentsight.service
agentsight --version
```

RPM keeps your `/etc/agentsight/config.json` (`%config(noreplace)`). If the new release bumps
`schema_version`, AgentSight copies your file to `config.json.bak.<unix-seconds>` on the next start
and writes a merged one: current defaults with your top-level customisations overlaid, so your Agent
rules survive the upgrade. Databases carry over; no migration step is needed.

## Uninstall

```bash
sudo systemctl disable --now agentsight.service
sudo yum remove agentsight             # or: sudo anolisa uninstall agentsight

# optional: drop collected data
sudo rm -rf /var/log/sysak/.agentsight
```

## Hardening checklist

| Item | Recommendation |
|---|---|
| Dashboard exposure | Keep `--host 127.0.0.1` where possible; otherwise firewall TCP 7396 |
| Authentication | Leave `server.auth.enabled` at `true`; the token file is `0600` and root-owned |
| Data directory | Leave the private umask in place; captured prompts and responses live there |
| Log export | Leave `runtime.sls_logtail_path` empty unless an external collector needs the events |
| Resource limits | Keep `CPUQuota`/`MemoryMax` from the packaged unit when running custom units |

## Related pages

- [Quick start](QUICKSTART.md) — first run
- [Configuration](configuration.md) — what to change after installing
- [Troubleshooting](troubleshooting.md) — probes that fail to load, missing data

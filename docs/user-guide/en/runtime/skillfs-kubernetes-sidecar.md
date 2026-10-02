# Run SkillFS as a Kubernetes Sidecar

[中文版](../../zh/runtime/skillfs-kubernetes-sidecar.md)

Run SkillFS beside a Kubernetes workload so the workload reads the SkillFS
view without mounting the physical skill source. The SkillFS container owns the
FUSE mount; the workload stays non-privileged and receives the propagated view.

The repository does not currently publish a dedicated SkillFS sidecar image.
Build the image from the source revision you plan to deploy, verify it locally,
and push it to a registry that the cluster can pull from.

## Read-only installed skills with Ledger

The optional `30-ledger-pod.yaml` profile copies an installed, flat skill bundle
into `/state/source`, runs the real Ledger scanner and activation processor,
then starts Ledger, SkillFS and Cosh in that order. It keeps
`--security --activation-mode file`; read-only package permissions never grant
activation. Both `.skill-meta/activation.json` and its version snapshots remain
inside each copied skill. File activation also reads the existing xattr protocol.

Use an ANOLISA RPM image containing `os-skills`, `agent-sec-core` and `cosh-ng`
from the revision being validated, and the corresponding dedicated SkillFS
image. The example uses the RPM Python layout and `/usr/share/anolisa/skills`.
For a raw install, change the init command's package argument and Python runtime
to that image's installed paths. Do not point SkillFS directly at the package.
Run these commands from `src/skillfs`, after creating the example namespace:

```bash
export NS=skillfs-container-example
export IMAGE=registry.example.com/anolisa/skillfs-sidecar:validated
export ANOLISA_IMAGE=registry.example.com/anolisa/anolisa:validated
kubectl -n "$NS" create configmap skillfs-ledger-init \
  --from-file=ledger-init.py=container/ledger-init.py --dry-run=client -o yaml |
  kubectl -n "$NS" apply -f -
sed -e "s|skillfs-sidecar:dev|$IMAGE|g" \
    -e "s|anolisa:dev|$ANOLISA_IMAGE|g" deploy/kubernetes/30-ledger-pod.yaml |
  kubectl -n "$NS" apply -f -
kubectl -n "$NS" logs skillfs-ledger-example -c ledger-init
kubectl -n "$NS" wait --for=condition=Ready pod/skillfs-ledger-example --timeout=300s
```

The init container has a read-only root filesystem. The initializer normalizes
copied permissions, refuses links, special files and package-supplied Ledger
metadata, and publishes a complete source tree before scanning. It uses a
dedicated Ledger configuration with explicit `managedSkillDirs`; daemon startup
alone does not turn default discovery roots into managed skills. Scanner or
activation errors stop initialization; policy outcomes, including hidden skills
and Ledger's safe pending-review snapshots, are preserved without auto-approval.

The Agent receives only the propagated view via its legacy user skill directory.
`--read-only` makes the FUSE mount itself reject mutations with `EROFS`, including
after propagation. A parent volume's `readOnly: true` alone does not protect
submounts. Ledger retains write access to the separate physical source.
Its home and workspace start empty; RPM/raw system skill and extension roots are
masked. `skills.custom_paths` is additive and cannot provide this isolation.
Custom-prefix images, extra extensions or preloaded homes require equivalent
masking. This profile uses FUSE activation as its enforcement boundary and does
not enable an independent Cosh Ledger hook. Adding hooks requires checking their
path identity and daemon access separately; do not mount `/state` in the Agent.

Ledger's startup probe requires a successful `daemon.health` RPC before SkillFS
and Cosh start. Readiness uses the same RPC, and repeated liveness failures
restart the Ledger sidecar. A stale socket or an unresponsive daemon fails the
probe; socket existence alone is insufficient.

The mount probes read virtual `skill-discover/SKILL.md`, which remains available
when all business skills are hidden. Readiness confirms the mount, not approval
of a required business skill. Add an application readiness condition if needed.
The activation watcher reloads activation artifacts already published by Ledger
without a remount. This profile does not automatically rescan arbitrary source
edits: the JSONL event log is diagnostic and the daemon does not tail it.
Treat seeded content as immutable; package updates require a new source volume
and a new scan. A trusted operator changing the existing source must explicitly
request Ledger scanning and activation before expecting the view to change.

For persistence, replace the **whole** `state` emptyDir with a dedicated PVC so
signing keys and per-skill snapshots survive together. Reusing the same package
preserves existing state; a changed or unrecognized seed fails instead of
overwriting it. Quiesce the old Pod before reusing its PVC, and keep the old volume
for rollback. Never run two initializers or daemons against the same state.

Validation: `python3 scripts/test-ledger-init.py` checks seeding without external
dependencies. In a disposable privileged Linux container with `/dev/fuse`, use
the agent-sec Python 3.11 environment and place current `skillfs`, `cosh-core`,
`fusermount3` and `timeout` on PATH, then run
`python scripts/test-ledger-init.py --integration`. It uses real scanners and
snapshots, a read-only bind mount, an unprivileged Cosh process, a raw-directory
bypass negative control, and a probe after every business skill is hidden.
This does not replace Kubernetes mount-propagation validation on the target cluster.

Delete the example Pod and ConfigMap when finished; retain any PVC until its
state is no longer needed:

```bash
kubectl -n "$NS" delete pod skillfs-ledger-example
kubectl -n "$NS" delete configmap skillfs-ledger-init
```

## Prerequisites

- Kubernetes 1.29 or later.
- Linux nodes with `/dev/fuse`.
- Permission to run the SkillFS sidecar as privileged.
- `docker buildx` and `kubectl`.
- A registry that the cluster can pull from.

## Choose the image base

SkillFS provides two equivalent sidecar image definitions. They use the same
entrypoint, probes, default paths, and Kubernetes manifest.

| Dockerfile | Runtime base | Use when |
| --- | --- | --- |
| `src/skillfs/container/Dockerfile` | Debian Bookworm | You want the general-purpose image with the pinned Rust 1.86 build toolchain |
| `src/skillfs/container/Dockerfile.alinux4` | Alibaba Cloud Linux 4 | Your deployment standardizes on Alibaba Cloud Linux 4 or builds through the public Aliyun RPM and Cargo mirrors |

The Alibaba Cloud Linux 4 build uses the Aliyun Cargo mirror by default. Pass
an empty `SKILLFS_CARGO_REGISTRY_INDEX` build argument when the build
environment should use crates.io directly. Both image definitions accept a
`BASE_IMAGE` build argument when production builds need a pinned base tag or
digest.

## Build, verify, and push the image

Run the following commands from the repository root. The example builds the
Debian image for AMD64 and tags it with the source commit so an operator can
trace the deployed image back to its inputs.

```bash
export REVISION="$(git rev-parse HEAD)"
export VERSION="$(git describe --tags --match 'skillfs/v*' --always)"
export IMAGE="registry.example.com/anolisa/skillfs-sidecar:$(git rev-parse --short=12 HEAD)"
export PLATFORM=linux/amd64
export DOCKERFILE=src/skillfs/container/Dockerfile

docker buildx build \
  --platform "$PLATFORM" \
  --build-arg VERSION="$VERSION" \
  --build-arg REVISION="$REVISION" \
  -f "$DOCKERFILE" \
  -t "$IMAGE" \
  --load \
  src/skillfs

docker run --rm --platform "$PLATFORM" "$IMAGE" skillfs --version
docker push "$IMAGE"
```

Set `DOCKERFILE=src/skillfs/container/Dockerfile.alinux4` to build the Alibaba
Cloud Linux 4 variant. Set `PLATFORM` to the target node architecture and run
the smoke check on every platform before publishing a multi-platform tag. The
repository CI does not currently build or test these dedicated sidecar images.

The `docker run` command above only verifies the binary and runtime libraries.
Serving a FUSE view still requires the device, privilege, volumes, and mount
propagation described below.

## How the image starts

With no command arguments, the image starts a PID 1 supervisor that runs
preflight before each attempt and launches a foreground mount worker:

```text
skillfs-supervisor
  └─ skillfs mount "$SKILLFS_SOURCE" "$SKILLFS_MOUNTPOINT" --foreground --allow-other
```

`SKILLFS_DISCOVER_ROOT` and `SKILLFS_EXTRA_ARGS` add optional mount arguments.
Do not add `--managed`; the container supervisor owns worker recovery and
forwards shutdown signals. Passing command arguments to the image replaces
this lifecycle completely, so the version smoke check needs no `/dev/fuse`.

## Automatic mount recovery

The supervisor reuses `skillfs-mount-probe` to read `SKILLFS_PROBE_FILE` through
FUSE. After consecutive failures it stops and reaps the worker, uses preflight
to clear the residual FUSE mount at the configured mountpoint, and starts a new
worker. One transient failure does not trigger a remount. Keep the probe file
stable, nonempty, and readable under the deployed visibility policy; deleting
or hiding it is also treated as a health failure.

Both image variants accept these environment variables:

| Variable | Default | Meaning |
| --- | --- | --- |
| `SKILLFS_SUPERVISOR_PROBE_INTERVAL_SECONDS` | `2` | Delay between probes |
| `SKILLFS_SUPERVISOR_FAILURE_THRESHOLD` | `2` | Consecutive runtime failures before recovery |
| `SKILLFS_SUPERVISOR_STABLE_HEALTHY_PROBES` | `3` | Consecutive successful runtime probes needed to reset the recovery budget |
| `SKILLFS_SUPERVISOR_STARTUP_TIMEOUT_SECONDS` | `30` | Startup health budget, checked after each probe |
| `SKILLFS_SUPERVISOR_STOP_TIMEOUT_SECONDS` | `10` | Worker stop budget before SIGKILL |
| `SKILLFS_SUPERVISOR_MAX_FAILED_ATTEMPTS` | `5` | Consecutive failed cycles before the supervisor exits |
| `SKILLFS_SUPERVISOR_BACKOFF_INITIAL_SECONDS` | `1` | Initial retry delay, doubled after each failed cycle |
| `SKILLFS_SUPERVISOR_BACKOFF_MAX_SECONDS` | `30` | Maximum retry delay |

Values must be positive; counts and startup/stop budgets must be integers.
The initial retry delay must not exceed its maximum. With immediate I/O errors,
default detection takes roughly 2–4 seconds; probe timeouts, worker shutdown,
cleanup, and startup add to recovery time. Keep kubelet probes enabled: the
reference liveness probe runs every 5 seconds and restarts after two failures,
so it can take over before in-container retries are exhausted.

Recovery restores new path opens. It cannot prevent a runtime from invalidating
FUSE, guarantee uninterrupted reads, or repair already-open handles. Consumers
must close failed handles and retry fresh opens within a bounded budget. For
ACS restart validation, restart an unrelated container in a disposable Pod and
check reads from both the sidecar and workload, recovery logs, and container
restart counts; local supervisor tests do not validate mount propagation.

## Required Pod topology

The reference Pod keeps the physical source away from the workload and shares
only the propagated FUSE view.

| Volume | Sidecar mount | Workload mount | Requirement |
| --- | --- | --- | --- |
| `skill-source` | `/var/lib/skillfs/source` | Not mounted | Must be writable; use a PVC when skill changes must survive Pod recreation |
| `skill-shared` | `/var/lib/skillfs/shared` with `Bidirectional` | The same path with `HostToContainer` | Keep the FUSE mountpoint in a subdirectory such as `shared/mount`, never over the volume root |
| `fuse-device` | `/dev/fuse` | Not mounted | Use a `/dev/fuse` `hostPath` with type `CharDevice` |

Only the SkillFS sidecar runs as root with `privileged` enabled. The workload
can run as a different non-root UID because the image always adds
`--allow-other`. The manifest uses a native Kubernetes sidecar by setting the
SkillFS init container's `restartPolicy` to `Always`, so Kubernetes waits for
the FUSE startup probe before starting the workload and stops the sidecar after
the workload exits.

## Deploy

The example uses a ConfigMap-backed skill source. Replace it with a PVC for
persistent workloads.

```bash
export NS=skillfs-container-example

kubectl apply -f src/skillfs/deploy/kubernetes/00-namespace.yaml
kubectl apply -f src/skillfs/deploy/kubernetes/10-example-configmap.yaml
sed "s|skillfs-sidecar:dev|$IMAGE|g" \
  src/skillfs/deploy/kubernetes/20-pod.yaml | kubectl apply -f -

kubectl -n "$NS" wait \
  --for=condition=Ready pod/skillfs-sidecar-example \
  --timeout=300s
```

## Verify the mounted view

Read the view from the non-privileged workload container:

```bash
export POD=skillfs-sidecar-example
export VIEW=/var/lib/skillfs/shared/mount/skills

kubectl -n "$NS" exec "$POD" -c agent -- ls -1 "$VIEW"
kubectl -n "$NS" exec "$POD" -c agent -- \
  cat "$VIEW/skillfs-container-example/SKILL.md"
kubectl -n "$NS" exec "$POD" -c agent -- \
  cat "$VIEW/skill-discover/SKILL.md"
kubectl -n "$NS" exec "$POD" -c agent -- \
  cat "$VIEW/skillfs-container-reserve/SKILL.md"
```

The listing must contain `skillfs-container-example` and `skill-discover`, but
not `skillfs-container-reserve`. The `skill-discover` output must contain the
`reserve` view and the absolute path used by the last command. Secondary
skills are hidden from directory listings, while their advertised paths remain
readable.

## Verify sidecar restart

```bash
kubectl -n "$NS" exec "$POD" -c skillfs -- \
  /bin/bash -c 'kill -TERM 1'
kubectl -n "$NS" wait \
  --for=condition=Ready pod/skillfs-sidecar-example \
  --timeout=300s
```

Run the mounted-view commands again after the Pod returns to Ready.

## Use your own workload

Edit `src/skillfs/deploy/kubernetes/20-pod.yaml`:

1. replace `skill-source` with your PVC;
2. remove the example ConfigMap and `seed-example` init container;
3. set `SKILLFS_PROBE_FILE` to a stable, non-empty file that remains visible
   for the lifetime of the mount;
4. replace the `agent` image and command;
5. keep `Bidirectional` on the SkillFS mount and `HostToContainer` on the
   workload mount.

The workload readiness probe should read meaningful SkillFS content, not only
check the directory or run `skillfs --version`.

The reference manifest marks the Pod unready after one failed FUSE read and
restarts only the SkillFS sidecar after two consecutive liveness failures. The
single-failure threshold means that a transient probe timeout also immediately
marks the Pod unready, which may shift traffic under high concurrency. The
workload intentionally has no liveness probe. After an `EIO` or `ENOTCONN`, a
consumer must close the failed file descriptor and reopen the file after the
Pod becomes Ready again.

## Image configuration

The image defaults match the reference manifest. Override them in the Pod when
your volume paths or probe skill differ.

| Variable | Default | Purpose |
| --- | --- | --- |
| `SKILLFS_SOURCE` | `/var/lib/skillfs/source` | Writable physical skill source root |
| `SKILLFS_MOUNTPOINT` | `/var/lib/skillfs/shared/mount` | FUSE mountpoint inside the shared volume |
| `SKILLFS_DISCOVER_ROOT` | `/var/lib/skillfs/shared/mount/skills` | Reader-visible root advertised by `skill-discover` |
| `SKILLFS_EXTRA_ARGS` | Empty | Additional whitespace-separated `skillfs mount` arguments |
| `SKILLFS_PROBE_FILE` | `skills/skillfs-container-example/SKILL.md` | Stable, non-empty file read through FUSE by the health probe |
| `SKILLFS_PROBE_TIMEOUT` | `5` | Per-read health probe timeout in seconds |
| `RUST_LOG` | `info` | SkillFS log filter |

The preflight check requires distinct absolute source and mountpoint paths, a
writable source, an openable `/dev/fuse`, `fusermount3`, and
`user_allow_other` in `/etc/fuse.conf`. `SKILLFS_SKIP_PREFLIGHT=1` is a debugging
escape hatch and should not be used in a normal deployment.

On shutdown, SkillFS receives `SIGTERM` as PID 1 and unmounts the FUSE view. If
a previous process was killed before cleanup, the next preflight removes a
residual FUSE mount at the configured mountpoint. It refuses to unmount any
non-FUSE filesystem found there, which protects a misconfigured volume path.

## Troubleshoot

```bash
kubectl -n "$NS" describe pod "$POD"
kubectl -n "$NS" logs "$POD" -c skillfs
kubectl -n "$NS" logs "$POD" -c skillfs --previous
kubectl -n "$NS" get events --sort-by=.lastTimestamp
```

Common causes are blocked privileged containers, missing `/dev/fuse`, incorrect
mount propagation, an unreadable probe file, or a read-only source volume.

Preflight failures include a stable numeric code in the container log. Codes
10 through 16 cover invalid configuration, the FUSE device, `fusermount3`, the
source root, the mountpoint, `fuse.conf`, and residual mount cleanup in that
order.

## Cleanup

```bash
kubectl delete namespace "$NS" --wait=true
```

`emptyDir` does not survive Pod recreation. Use a PVC when skill changes must
persist across Pods.

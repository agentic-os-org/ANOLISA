# SkillFS Kubernetes Sidecar Manifests

These manifests run SkillFS as a privileged FUSE sidecar and expose its view to
a non-privileged workload container.

See the
[Kubernetes sidecar user guide](../../../../docs/user-guide/en/runtime/skillfs-kubernetes-sidecar.md)
([中文](../../../../docs/user-guide/zh/runtime/skillfs-kubernetes-sidecar.md))
for the complete workflow.

## Prerequisites

- Kubernetes 1.29 or later.
- Privileged containers and the `/dev/fuse` hostPath are allowed.
- The target nodes provide `/dev/fuse`.
- A self-built SkillFS image is available to the cluster. The repository does
  not currently publish a dedicated sidecar image.

## Files

| File | Purpose |
| --- | --- |
| `00-namespace.yaml` | Isolated example namespace |
| `10-example-configmap.yaml` | Example default and secondary skills |
| `20-pod.yaml` | SkillFS sidecar and workload Pod |
| `30-ledger-pod.yaml` | Optional read-only package → Ledger → secure SkillFS → Cosh profile |

The optional Ledger profile needs the initializer ConfigMap and an ANOLISA RPM
image in addition to the dedicated sidecar image. Follow the user guide's
read-only installation section; do not apply all Pod examples together with
`kubectl apply -f .`.

## Deploy

```bash
export IMAGE="registry.example.com/anolisa/skillfs-sidecar:$(git rev-parse --short=12 HEAD)"
export NS=skillfs-container-example

kubectl apply -f 00-namespace.yaml
kubectl apply -f 10-example-configmap.yaml
sed "s|skillfs-sidecar:dev|$IMAGE|g" 20-pod.yaml | kubectl apply -f -
kubectl -n "$NS" wait \
  --for=condition=Ready pod/skillfs-sidecar-example \
  --timeout=300s
```

Build and verify the image with `container/Dockerfile`, or use
`container/Dockerfile.alinux4` for an Alibaba Cloud Linux 4 runtime. The linked
user guide documents both variants and their runtime configuration.

The workload readiness probe reads the transformed default skill and the
virtual `skill-discover/SKILL.md`. It also opens a secondary skill through the
path advertised by `skill-discover`. Secondary skills stay out of the directory
listing but remain readable through their advertised paths.

The SkillFS readiness probe removes the Pod from service after one failed FUSE
read. Its liveness probe restarts the sidecar after two consecutive failures
and gives that probe-triggered shutdown 10 seconds to finish. The workload has
no liveness probe, so a broken FUSE view cannot restart the consumer and repeat
the same failure. Consumers should close failed file descriptors and reopen
files after the Pod becomes Ready again.

## Use your own skills

Before using the manifest for a workload:

1. replace `skill-source` with a PVC;
2. remove the example ConfigMap and `seed-example` init container;
3. update `SKILLFS_PROBE_FILE` to a stable, non-empty file that remains visible
   for the lifetime of the mount;
4. replace the `agent` image and command;
5. keep the shared-volume mount propagation settings unchanged.

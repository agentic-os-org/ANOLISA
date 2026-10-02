# Deploy ws-ckpt as a Kubernetes Sidecar

[中文版](k8s-sidecar-deploy_zh.md)

This guide deploys `ws-ckpt` as a privileged daemon sidecar alongside an
unprivileged application container. The example persists daemon state on the
host and propagates the btrfs mount into the application container.

## Prerequisites

- The host kernel has the btrfs and loop modules available
  (`modprobe btrfs && modprobe loop`).
- `/dev/loop-control` and `/dev/loop*` are available on the host.
- The image contains `ws-ckpt`, `btrfs-progs`,
  `util-linux`, `psmisc`, `rsync`, and `kmod`.

## Build an image

The following example builds an image on Alibaba Cloud Linux 3.

```bash
# 1. Build ws-ckpt (requires Rust stable 1.78 or later)
cd anolisa/src/ws-ckpt/src
cargo build --release --workspace
ls target/release/ws-ckpt

# 2. Prepare the image context
mkdir -p ~/wsckpt-image
cp target/release/ws-ckpt ~/wsckpt-image/

# 3. Copy locally built btrfs-progs when the package is unavailable from dnf
cp /usr/local/bin/btrfs /usr/local/bin/mkfs.btrfs ~/wsckpt-image/
# Skip this step and install btrfs-progs in the Dockerfile when dnf provides it

# 4. Create the Dockerfile
cat > ~/wsckpt-image/Dockerfile <<'EOF'
FROM registry.cn-hangzhou.aliyuncs.com/alinux/alinux3:latest

RUN dnf install -y --setopt=tsflags=nodocs \
      util-linux psmisc rsync kmod which findutils \
    && dnf clean all

# Replace these lines with dnf install btrfs-progs when the package is available
COPY btrfs mkfs.btrfs /usr/local/bin/
RUN chmod 0755 /usr/local/bin/btrfs /usr/local/bin/mkfs.btrfs \
    && ln -sf /usr/local/bin/btrfs /usr/bin/btrfs

COPY ws-ckpt /usr/local/bin/ws-ckpt
RUN chmod 0755 /usr/local/bin/ws-ckpt
EOF

# 5. Build and verify the image
cd ~/wsckpt-image
docker build -t localhost/ws-ckpt:smoke .
docker run --rm localhost/ws-ckpt:smoke ws-ckpt --version
docker run --rm localhost/ws-ckpt:smoke btrfs --version

# 6. Import the image into cluster nodes (k3s example)
docker save -o ws-ckpt-smoke.tar localhost/ws-ckpt:smoke
sudo k3s ctr images import ws-ckpt-smoke.tar
# For standard Kubernetes, push the image to a registry instead
# docker tag localhost/ws-ckpt:smoke your-registry/ws-ckpt:smoke
# docker push your-registry/ws-ckpt:smoke
```

## Deploy the Pod

```bash
# Ensure that the required host kernel modules are loaded
sudo modprobe loop && sudo modprobe btrfs

# Apply the example Pod manifest
kubectl apply -f k8s-sidecar-example.yaml
kubectl get pod wsckpt-smoke -w          # Wait for 2/2 Running
kubectl logs wsckpt-smoke -c daemon      # Confirm successful bootstrap
```

The complete manifest is available in
[`k8s-sidecar-example.yaml`](../k8s-sidecar-example.yaml).

## Pod design

The Pod runs two containers from the same image.

| Container | Role | Privileges |
|-----------|------|------------|
| daemon | Runs `ws-ckpt daemon` and manages the btrfs loop device | `privileged: true` |
| app | Runs the workload and invokes the CLI over UDS | Unprivileged |

## Required volumes

| Name | Type | Purpose |
|------|------|---------|
| sock | `emptyDir` | UDS socket at `/run/ws-ckpt/ws-ckpt.sock` |
| dev | `hostPath` for `/dev` | Access to the host loop devices |
| ws-state | `hostPath` for `/var/lib/ws-ckpt` | Persistent loop image, state, and daemon lock |
| ws-ckpt-mount | `hostPath` anchor | btrfs mount propagated to the application |
| data | `emptyDir` | Workspace parent directory, such as `/data` |
| config | `emptyDir` | Shared global configuration at `/etc/ws-ckpt` |

Losing the `ws-state` volume loses the loop image and its data.

## Deployment constraints

1. Set `privileged: true` for the daemon. `losetup`, `mount`,
   and `mkfs.btrfs` require it.

2. Mount the host `/dev` into the daemon. A privileged container under
   k3s containerd does not automatically receive the host loop device nodes.

3. Persist `/var/lib/ws-ckpt` with a `hostPath` or PVC. The btrfs
   image path is not configurable. If this directory remains in the writable
   container layer, a Pod restart removes the image while leaving host-level
   loop devices or mounts behind.

4. Point `--mount-path` to a shared volume mounted at the same path in
   both containers. Use `Bidirectional` propagation for the daemon and
   `HostToContainer` for the app. A root filesystem directory is private
   to the daemon's mount namespace, so the app would receive a dangling symlink
   after `init`.

5. Place the workspace below the shared data volume instead of using the
   volume mount point itself. `ws-ckpt init` renames the workspace, and
   renaming a mount point fails with `EBUSY`.

6. Enable `shareProcessNamespace: true`. This lets `fuser -m` see
   application processes while rollback, recovery, and image shrinking check
   whether a mount is busy.

7. Backend selection is automatic. When the filesystem containing
   `/var/lib` is btrfs, `auto_detect` selects `BtrfsBase` and
   operates on subvolumes without a loop device. Filesystems such as ext4 and
   XFS use the `BtrfsLoop` overmount path.

8. Share `/etc/ws-ckpt` between both containers.
   `ws-ckpt config --global` writes
   `/etc/ws-ckpt/config.toml` directly, and the daemon reloads its own
   view of that path. Without the shared volume, the daemon keeps its built-in
   defaults and policies such as `auto-cleanup` do not take effect.
   Per-workspace configuration through `config -w` is stored in daemon
   state and does not have this requirement.

## SIGTERM and loop device cleanup

The daemon exits on `SIGTERM` without running `umount` or
`losetup -d`. Without cleanup, the host-level btrfs mount and loop device
can outlive the container. Repeated Pod recreation can then accumulate stacked
mounts and exhaust `/dev/loop*` devices.

Configure this `preStop` lifecycle hook on the daemon container.

```yaml
lifecycle:
  preStop:
    exec:
      command:
        - /bin/sh
        - -c
        - |
          own_loops=$(losetup -j /var/lib/ws-ckpt/btrfs-data.img 2>/dev/null | cut -d: -f1)
          umount /opt/ws-ckpt-mount 2>/dev/null || true
          for dev in $own_loops; do
            losetup -d "$dev" 2>/dev/null || true
          done
          losetup -ln -O NAME,BACK-FILE | while read dev file; do
            case "$file" in
              "/var/lib/ws-ckpt/btrfs-data.img (deleted)"|"/data/ws-ckpt/btrfs-data.img (deleted)")
                losetup -d "$dev" 2>/dev/null || true ;;
            esac
          done
```

The hook covers two kinds of leftovers. Neither rule uses path existence as
the identifying signal.

- Before unmounting, `losetup -j` captures the loop devices owned by the
  current Pod. It matches the backing file by device and inode. A previous Pod
  may still hold an unlinked inode after a new Pod creates another file at the
  same path.
- The kernel's ` (deleted)` suffix identifies historical orphan devices
  whose backing files have been unlinked. It does not match a live container's
  device. The legacy `/data/ws-ckpt` image path is also included.

The hook is best effort. If an application process still uses the mount,
`umount` can fail with `EBUSY`. On Linux 3.7 and later,
`losetup -d` uses lazy device destruction for a busy loop device and
marks it for automatic clearing when its final user exits. The next daemon
startup also detaches a loop device that still backs the persistent image but
is no longer mounted.

A `preStop` hook does not run in every failure mode, including some node
failures. Persisting `ws-state` is therefore essential for keeping the
image and for inode-based orphan recovery on the next startup.

With bare `docker run`, mount `/var/lib/ws-ckpt` from persistent
storage. Before manually cleaning existing leftovers, make sure that no
`ws-ckpt` container is running. Limit cleanup to the current and legacy
image paths instead of detaching every loop device on the host.

```bash
sudo sh <<'CLEANUP'
losetup -ln -O NAME,BACK-FILE | while read dev file; do
    clean=${file%" (deleted)"}
    case "$clean" in
        /var/lib/ws-ckpt/btrfs-data.img|/data/ws-ckpt/btrfs-data.img) ;;
        *) continue ;;
    esac
    # "(deleted)" means that the backing inode was removed with its container
    # An existing host-level image is skipped
    if [ "$file" != "$clean" ] || [ ! -e "$clean" ]; then
        losetup -d "$dev"
    fi
done
CLEANUP
```

Run the block with `sudo` because `losetup -d` requires root.
Detach failures remain visible so that they can be investigated.

## Smoke test

After the Pod reaches `2/2 Running`, run the following commands.

```bash
# Create and initialize a workspace
kubectl exec wsckpt-smoke -c app -- mkdir -p /data/workspace
kubectl exec wsckpt-smoke -c app -- sh -c 'echo hello > /data/workspace/file.txt'
kubectl exec wsckpt-smoke -c app -- ws-ckpt init --workspace /data/workspace
kubectl exec wsckpt-smoke -c app -- ws-ckpt checkpoint --workspace /data/workspace --message smoke-v1
kubectl exec wsckpt-smoke -c app -- ws-ckpt list --workspace /data/workspace

# Verify that the workspace remains writable after init
kubectl exec wsckpt-smoke -c app -- sh -c 'echo bug6-verify-$(date +%s) > /data/workspace/verify.txt'
kubectl exec wsckpt-smoke -c app -- cat /data/workspace/verify.txt
kubectl exec wsckpt-smoke -c app -- ls -la /data/workspace/
```

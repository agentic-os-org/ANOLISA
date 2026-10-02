# 在 Kubernetes 中以 Sidecar 方式部署 ws-ckpt

[English](k8s-sidecar-deploy.md)

本文介绍如何把 `ws-ckpt` 作为特权 daemon sidecar，与无特权的业务容器
部署在同一个 Pod 中。示例会在宿主机上持久化 daemon 状态，并把 btrfs
挂载传播给业务容器。

## 前置条件

- 宿主机内核提供 btrfs 和 loop 模块，可通过
  `modprobe btrfs && modprobe loop` 加载
- 宿主机上可以使用 `/dev/loop-control` 和 `/dev/loop*`
- 容器镜像包含 `ws-ckpt`、`btrfs-progs`、
  `util-linux`、`psmisc`、`rsync` 和 `kmod`

## 构建镜像

下面以 Alibaba Cloud Linux 3 为例构建镜像。

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

## 部署 Pod

```bash
# Ensure that the required host kernel modules are loaded
sudo modprobe loop && sudo modprobe btrfs

# Apply the example Pod manifest
kubectl apply -f k8s-sidecar-example.yaml
kubectl get pod wsckpt-smoke -w          # Wait for 2/2 Running
kubectl logs wsckpt-smoke -c daemon      # Confirm successful bootstrap
```

完整清单见
[`k8s-sidecar-example.yaml`](../k8s-sidecar-example.yaml)。

## Pod 设计

Pod 使用同一镜像运行两个容器。

| 容器 | 作用 | 权限 |
|------|------|------|
| daemon | 运行 `ws-ckpt daemon` 并管理 btrfs loop 设备 | `privileged: true` |
| app | 运行业务负载，并通过 UDS 调用 CLI | 无特权 |

## 必需的 Volume

| 名称 | 类型 | 用途 |
|------|------|------|
| sock | `emptyDir` | 位于 `/run/ws-ckpt/ws-ckpt.sock` 的 UDS socket |
| dev | `/dev` 对应的 `hostPath` | 访问宿主机 loop 设备 |
| ws-state | `/var/lib/ws-ckpt` 对应的 `hostPath` | 持久保存 loop 镜像、状态和 daemon lock |
| ws-ckpt-mount | 作为锚点目录的 `hostPath` | 向业务容器传播 btrfs 挂载 |
| data | `emptyDir` | workspace 的父目录，例如 `/data` |
| config | `emptyDir` | 两个容器共享的全局配置目录 `/etc/ws-ckpt` |

一旦丢失 `ws-state` volume，loop 镜像及其中的数据也会丢失。

## 部署约束

1. daemon 需要设置 `privileged: true`。`losetup`、
   `mount` 和 `mkfs.btrfs` 都依赖这项权限。

2. daemon 需要挂载宿主机的 `/dev`。k3s containerd 不会自动把
   宿主机上的 loop 设备节点暴露给特权容器。

3. 需要用 `hostPath` 或 PVC 持久化 `/var/lib/ws-ckpt`。
   btrfs 镜像路径目前不可配置。这个目录如果留在容器可写层，Pod 重启后
   镜像会消失，宿主机上的 loop 设备或挂载却可能继续存在。

4. `--mount-path` 需要指向两个容器以相同路径挂载的共享 volume。
   daemon 使用 `Bidirectional`，app 使用 `HostToContainer`。
   daemon rootfs 中的普通目录只属于它自己的 mount namespace，
   挂载无法传播给 app，`init` 后 app 会得到悬空 symlink。

5. workspace 需要放在共享 data volume 的子目录中，不能直接使用 volume
   挂载点。`ws-ckpt init` 会重命名 workspace，而重命名挂载点会返回
   `EBUSY`。

6. 需要启用 `shareProcessNamespace: true`。rollback、recover 和
   image shrink 检查挂载是否繁忙时会调用 `fuser -m`，共享进程
   namespace 后才能看到 app 容器中的进程。

7. 后端会自动选择。`/var/lib` 所在文件系统为 btrfs 时，
   `auto_detect` 会选择 `BtrfsBase`，直接操作 subvolume，
   不需要 loop 设备。ext4 和 XFS 等文件系统会使用 `BtrfsLoop`
   overmount 路径。

8. 两个容器需要共享 `/etc/ws-ckpt`。
   `ws-ckpt config --global` 会直接写入
   `/etc/ws-ckpt/config.toml`，daemon 则从自己看到的同一路径
   重新加载配置。如果缺少共享 volume，daemon 会继续使用内置默认值，
   `auto-cleanup` 等策略不会生效。通过 `config -w` 设置的
   workspace 配置保存在 daemon 状态中，不受这项约束。

## SIGTERM 与 Loop 设备清理

daemon 收到 `SIGTERM` 后会直接退出，不会执行 `umount` 或
`losetup -d`。如果没有清理，宿主机级 btrfs 挂载和 loop 设备可能
活得比容器更久。反复重建 Pod 会逐渐叠加挂载，并耗尽
`/dev/loop*` 设备。

请在 daemon 容器中配置下面的 `preStop` lifecycle hook。

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

这段 hook 处理两类遗留，两条规则都不把路径是否存在当作判断依据。

- 卸载前先用 `losetup -j` 找到当前 Pod 的 loop 设备。这个命令
  按 backing 文件的设备号和 inode 匹配。旧 Pod 可能仍持有已经 unlink
  的 inode，而新 Pod 已经在同一路径创建了另一个文件
- 内核追加的 ` (deleted)` 后缀用于识别 backing 文件已经 unlink
  的历史孤儿设备，不会匹配仍在运行的容器。旧部署使用过的
  `/data/ws-ckpt` 镜像路径也包含在清理范围内

这段 hook 只提供尽力而为的清理。业务进程仍在使用挂载时，
`umount` 可能返回 `EBUSY`。Linux 3.7 及更高版本中的
`losetup -d` 会对繁忙设备使用延迟销毁，并把设备标记为
autoclear，最后一个使用者退出后才会释放。下次 daemon 启动时，
如果持久化镜像仍由某个 loop 设备承载但没有挂载，也会先将其分离。

部分节点故障等异常场景不会执行 `preStop`。因此必须持久化
`ws-state`，这样既能保住镜像，也能让下次启动按 inode 识别
和处理孤儿设备。

直接使用 `docker run` 时，也应当为 `/var/lib/ws-ckpt`
挂载持久存储。手工清理历史遗留前，先确认没有 `ws-ckpt`
容器正在运行。清理范围只覆盖当前镜像路径和旧版镜像路径，不要扫描并
分离宿主机上的所有 loop 设备。

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

`losetup -d` 需要 root 权限，因此整段命令通过 `sudo` 运行。
命令不会静默忽略 detach 错误，方便继续排查。

## 冒烟验证

Pod 达到 `2/2 Running` 后，执行下面的命令。

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

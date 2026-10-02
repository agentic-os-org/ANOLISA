# 操作系统备份与恢复

适用范围：完整系统备份、磁盘镜像、分区快照、系统状态、裸机恢复。

> 适用系统：Alibaba Cloud Linux 4 (ALinux 4) / 纯命令行 / x86_64 / aarch64

---

## 操作前注意事项

- 系统级备份/恢复通常需要 **root 权限**
- 使用 `dd` 对系统盘操作时，须**从 Live 环境或挂载备用根**进行，不能在运行中的系统上直接操作目标分区
- 操作前用 `lsblk` 确认磁盘/分区标识，**绝对不能搞错设备名**
- 恢复操作会**完全覆盖目标磁盘**，无法撤销

---

## 工具选择指南

| 工具 | 适用场景 | ALinux 4 安装方式 |
|---|---|---|
| `restic` | 增量加密备份，支持本地/OSS/SSH | `yum install restic` |
| `tar` | 系统文件打包，无需额外工具 | 预装 |
| `dd` | 原始磁盘/分区镜像 | 预装 |
| LVM 快照 | 逻辑卷即时快照（需使用 LVM） | 预装（`lvm2`） |
| `rsync` | 增量文件同步备份 | `yum install rsync` |

---

## 方法一：restic（推荐——增量加密备份）

### 安装与初始化

```bash
sudo yum install -y restic

# 创建密码文件（比在脚本中硬编码密码安全得多）
# 生成后请妥善保管此文件，丢失密码将无法恢复备份
sudo sh -c 'echo "你的强密码" > /root/.restic-password'
sudo chmod 600 /root/.restic-password

# 在本地初始化备份仓库
sudo restic init \
    --repo /backups/system-repo \
    --password-file /root/.restic-password
```

### 备份整个系统

```bash
# 排除虚拟文件系统和临时目录（/proc /sys /dev /run /tmp 等不含需要备份的持久数据）
sudo restic -r /backups/system-repo \
    --password-file /root/.restic-password \
    backup / \
    --exclude=/proc \
    --exclude=/sys \
    --exclude=/dev \
    --exclude=/run \
    --exclude=/tmp \
    --exclude=/mnt \
    --exclude=/media \
    --exclude=/lost+found \
    --exclude=/backups \
    --tag "full-system"

# 查看所有快照
sudo restic -r /backups/system-repo \
    --password-file /root/.restic-password \
    snapshots

# 查看统计信息
sudo restic -r /backups/system-repo \
    --password-file /root/.restic-password \
    stats
```

### 从 restic 恢复

```bash
# 列出可用快照，找到目标快照 ID
sudo restic -r /backups/system-repo \
    --password-file /root/.restic-password \
    snapshots

# 恢复最新快照到指定目录
sudo restic -r /backups/system-repo \
    --password-file /root/.restic-password \
    restore latest --target /mnt/restore

# 恢复指定快照（用快照 ID 前 8 位即可）
sudo restic -r /backups/system-repo \
    --password-file /root/.restic-password \
    restore abc12345 --target /mnt/restore

# 仅恢复指定目录（例如只恢复 /etc）
sudo restic -r /backups/system-repo \
    --password-file /root/.restic-password \
    restore latest --include /etc --target /tmp/etc-restore
```

### 快照清理策略

```bash
# 保留策略：7 个每日 + 4 个每周 + 3 个每月，同时清理孤立数据块
sudo restic -r /backups/system-repo \
    --password-file /root/.restic-password \
    forget \
    --keep-daily 7 \
    --keep-weekly 4 \
    --keep-monthly 3 \
    --prune

# 验证仓库数据完整性
sudo restic -r /backups/system-repo \
    --password-file /root/.restic-password \
    check
```

---

## 方法二：tar 全量系统打包

适合：无需额外工具、离线环境

```bash
#!/bin/bash
set -euo pipefail

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
DEST="/backups/system"
mkdir -p "$DEST"

# -p 保留文件权限，排除虚拟文件系统和备份目录本身
sudo tar -czpf "${DEST}/system_${TIMESTAMP}.tar.gz" \
    --exclude=/proc \
    --exclude=/sys \
    --exclude=/dev \
    --exclude=/run \
    --exclude=/tmp \
    --exclude=/mnt \
    --exclude=/media \
    --exclude=/lost+found \
    --exclude="$DEST" \
    /

echo "系统备份完成：${DEST}/system_${TIMESTAMP}.tar.gz"
echo "文件大小：$(du -sh "${DEST}/system_${TIMESTAMP}.tar.gz" | cut -f1)"

# 验证归档可读性
tar -tzf "${DEST}/system_${TIMESTAMP}.tar.gz" | tail -5
```

### 从 tar 恢复系统

从 Live 环境（或挂载目标盘后）执行：

```bash
# 挂载目标磁盘（如恢复到新盘）
# sudo mkfs.xfs /dev/vda1
# sudo mount /dev/vda1 /mnt/newroot

sudo tar -xzpf /backups/system/system_20240315.tar.gz -C /mnt/newroot

# 恢复后重装 GRUB 引导（见下方裸机恢复章节）
```

---

## 方法三：dd 磁盘镜像

适合：完整磁盘克隆、更换硬件

**dd 直接操作块设备，写错目标会不可逆地销毁数据。执行前必须：**
1. **从救援环境操作**，不要在运行中的系统上 dd 自己的系统盘
2. **用 `lsblk` 确认源和目标设备名**，让用户二次确认
3. **确保目标设备没有被挂载**

```bash
# 第一步：查看磁盘分区信息，确认设备名
lsblk
sudo fdisk -l

# 第二步：让用户确认后再执行
echo "请确认以下信息："
echo "  源设备：/dev/vda"
echo "  目标文件：/backups/disk_vda_$(date +%Y%m%d).img.gz"
echo "输入 YES 继续（区分大小写）："
read -r CONFIRM
if [ "$CONFIRM" != "YES" ]; then
    echo "已取消"
    exit 0
fi

# 第三步：备份整块磁盘（含分区表）
# bs=4M 提高吞吐，conv=fsync 确保数据落盘
sudo dd if=/dev/vda bs=4M status=progress conv=fsync \
    | gzip > "/backups/disk_vda_$(date +%Y%m%d).img.gz"
```

### dd 恢复

```bash
# 恢复前再次确认目标设备——此操作会完全覆盖目标磁盘，不可逆
lsblk
echo "即将恢复到 /dev/vdb，该磁盘所有数据将被覆盖"
echo "输入 YES 继续："
read -r CONFIRM
if [ "$CONFIRM" != "YES" ]; then
    echo "已取消"
    exit 0
fi

gunzip -c /backups/disk_vda_20240315.img.gz \
    | sudo dd of=/dev/vdb bs=4M status=progress conv=fsync
```

---

## 方法四：LVM 快照（适用于 LVM 环境）

若根分区在 LVM 逻辑卷上，可创建即时快照，不影响系统运行。

```bash
# 查看当前 LVM 卷组和逻辑卷
sudo lvs
sudo vgs

# 创建快照（快照大小建议为原始卷的 20%，根据写入频率调整）
sudo lvcreate -L 5G -s -n root_snap /dev/aliyun_vg/root

# 查看快照状态（snap_percent 显示空间使用率）
sudo lvs -o +snap_percent

# 挂载快照（只读查看或备份）
sudo mkdir -p /mnt/snap
sudo mount -o ro /dev/aliyun_vg/root_snap /mnt/snap

# 从挂载的快照创建 tar 备份
sudo tar -czf "/backups/system/system_snap_$(date +%Y%m%d).tar.gz" \
    --exclude=/mnt/snap/proc \
    --exclude=/mnt/snap/sys \
    -C /mnt/snap .

# 卸载并删除快照（快照空间写满会失效，用完及时清理）
sudo umount /mnt/snap
sudo lvremove -f /dev/aliyun_vg/root_snap
```

---

## /etc 配置目录单独备份

系统配置改动频繁，建议单独高频备份 `/etc`：

```bash
#!/bin/bash
set -euo pipefail

DEST="/backups/etc"
KEEP_DAYS=60
TIMESTAMP=$(date +%Y%m%d_%H%M%S)

mkdir -p "$DEST"

sudo tar -czf "${DEST}/etc_${TIMESTAMP}.tar.gz" /etc

echo "[OK] /etc 备份完成：$(du -sh "${DEST}/etc_${TIMESTAMP}.tar.gz" | cut -f1)"

find "$DEST" -name "etc_*.tar.gz" -mtime +"${KEEP_DAYS}" -delete
```

---

## 裸机恢复完整流程

恢复到新服务器时按此顺序操作：

```bash
# 1. 进入救援/Live 环境，挂载目标磁盘
lsblk
sudo mkfs.xfs /dev/vda1       # 如需格式化（全新磁盘）
sudo mount /dev/vda1 /mnt

# 2. 恢复系统文件（二选一）
# 方式 A：从 tar 备份
sudo tar -xzpf /backups/system/system_20240315.tar.gz -C /mnt
# 方式 B：从 restic
sudo restic -r /backups/system-repo \
    --password-file /root/.restic-password \
    restore latest --target /mnt

# 3. 挂载虚拟文件系统（chroot 前必须）
sudo mount --bind /proc /mnt/proc
sudo mount --bind /sys /mnt/sys
sudo mount --bind /dev /mnt/dev
sudo mount --bind /run /mnt/run

# 4. chroot 进入恢复的系统
sudo chroot /mnt

# 5. 检查并修复 /etc/fstab（磁盘 UUID 可能已变）
blkid
cat /etc/fstab
vi /etc/fstab

# 6. 重装 GRUB 引导
grub2-install /dev/vda
grub2-mkconfig -o /boot/grub2/grub.cfg

# 7. 退出 chroot 并卸载
exit
sudo umount /mnt/proc /mnt/sys /mnt/dev /mnt/run
sudo umount /mnt

# 8. 重启
sudo reboot
```

---

## 磁盘空间检查（备份前必做）

```bash
# 查看备份目标磁盘剩余空间
df -h /backups

# 估算系统占用大小（排除虚拟文件系统）
sudo du -sh / \
    --exclude=/proc \
    --exclude=/sys \
    --exclude=/dev \
    --exclude=/tmp \
    2>/dev/null

# 查看各目录大小，定位大文件
sudo du -h --max-depth=2 / \
    --exclude=/proc \
    --exclude=/sys \
    2>/dev/null | sort -rh | head -20
```

---

## 自动化系统备份脚本

```bash
#!/bin/bash
set -euo pipefail
trap 'echo "[FAIL] 系统备份在第 $LINENO 行失败"; exit 1' ERR

export RESTIC_REPOSITORY="/backups/system-repo"
export RESTIC_PASSWORD_FILE="/root/.restic-password"

# 执行备份
restic backup / \
    --exclude=/proc \
    --exclude=/sys \
    --exclude=/dev \
    --exclude=/run \
    --exclude=/tmp \
    --exclude=/mnt \
    --exclude=/media \
    --exclude=/lost+found \
    --exclude="$RESTIC_REPOSITORY" \
    --tag "scheduled"

# 应用保留策略并清理旧数据
restic forget \
    --keep-daily 7 \
    --keep-weekly 4 \
    --keep-monthly 3 \
    --prune

# 验证仓库完整性
restic check

echo "[OK] 系统备份完成：$(date)"
```

加入定时任务（root 的 crontab）：

```bash
sudo crontab -e
# 每周日凌晨 3 点执行系统备份
0 3 * * 0 /root/os_backup.sh >> /var/log/os_backup.log 2>&1
```

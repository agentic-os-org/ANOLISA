---
name: storage-resize
version: 1.0.0
description: Linux 磁盘扩容技能（RPM 包管理系统），实现云盘扩容后自动完成分区调整和文件系统扩容。支持系统盘/数据盘在线扩容，适配 XFS/EXT4/Btrfs 文件系统。适用于 RPM 包管理的 Linux 系统（如 Alibaba Cloud Linux）。
layer: system
lifecycle: operations
dependencies:
  - aliyun-ecs
---

# Storage resize - 磁盘扩容

## 核心定位

**两阶段磁盘扩容流程：**
1. **云平台阶段**：使用 `aliyun-ecs` skill 完成云盘扩容
2. **操作系统阶段**：执行分区调整和文件系统扩容

**重要约束：**
- ☁️ 阿里云云盘**仅支持扩容，不支持缩容**
- 📀 支持**系统盘**和**数据盘**两种场景
- ⚡ **在线扩容**：云盘扩容后无需重启实例，通过 NVMe 重新扫描即可识别（NVMe 设备）

---

## 前置要求

### 1. 依赖技能

本技能依赖 `aliyun-ecs` skill 进行云平台操作：
- 查询磁盘信息 (`DescribeDisks`)
- 执行磁盘扩容 (`ResizeDisk`)
- 查询实例信息 (`DescribeInstances`)

### 2. 工具安装

```bash
# 检查并安装必要工具
yum install -y cloud-utils-growpart xfsprogs e2fsprogs btrfs-progs

# 验证工具已安装
which growpart xfs_growfs resize2fs
```

### 3. 权限要求

- **阿里云 CLI 认证**：需配置 `aliyun configure`（优先使用 EcsRamRole）
- **系统权限**：需要 root 权限执行分区和文件系统操作

---

## 快速开始

### 一键扩容流程

```bash
# 1. 确认设备名称和文件系统类型
lsblk -f /dev/nvme0n1

# 2. 云平台扩容（使用 aliyun-ecs skill）
# aliyun ecs ResizeDisk --DiskId <disk-id> --NewSize <new-size> --RegionId <region>

# 3. NVMe 设备重新扫描（让 OS 识别新容量）
echo 1 > /sys/block/nvme0n1/device/rescan_controller

# 4. 确认新容量已识别
lsblk /dev/nvme0n1

# 5. 扩容分区
growpart /dev/nvme0n1 3

# 6. 扩容文件系统（根据类型选择命令）
xfs_growfs /          # XFS（使用挂载点）
resize2fs /dev/nvme0n1p3   # EXT4（使用设备路径）

# 7. 验证
df -h /
```

---

## 文件系统支持

| 文件系统 | 扩容命令 | 参数类型 | 备注 |
|---------|---------|---------|------|
| XFS | `xfs_growfs /mountpoint` | 挂载点 | Alinux4 默认文件系统 |
| EXT4 | `resize2fs /dev/vdb1` | 设备路径 | 支持在线扩容 |
| Btrfs | `btrfs filesystem resize max /mountpoint` | 挂载点 | 需要 btrfs-progs |

### 文件系统类型检测

```bash
# 方法 1: 使用 df
df -T /

# 方法 2: 使用 lsblk
lsblk -f /dev/nvme0n1p3

# 方法 3: 使用 mount
mount | grep "on / "
```

---

## 完整流程

### 步骤 1: 云平台扩容（使用 aliyun-ecs skill）

```bash
# 获取实例 ID 和地域（ECS 实例内）
INSTANCE_ID=$(curl -s http://100.100.100.200/latest/meta-data/instance-id)
REGION_ID=$(curl -s http://100.100.100.200/latest/meta-data/region-id)

# 查询系统盘信息
aliyun ecs DescribeDisks \
  --RegionId $REGION_ID \
  --InstanceId $INSTANCE_ID \
  --DiskType system

# 执行扩容（例如：40GB → 90GB）
aliyun ecs ResizeDisk \
  --DiskId d-bp1234567890abcdef \
  --NewSize 90 \
  --RegionId $REGION_ID
```

### 步骤 2: 操作系统内扩容

```bash
# 2.1 识别设备类型
lsblk

# 2.2 NVMe 设备重新扫描（关键步骤！）
# 云盘扩容后，NVMe 设备需要重新扫描控制器才能识别新容量
echo 1 > /sys/block/nvme0n1/device/rescan_controller

# 2.3 验证新容量已识别
lsblk /dev/nvme0n1

# 2.4 扩容分区
growpart /dev/nvme0n1 3

# 2.5 扩容文件系统
# 先确认文件系统类型
FS_TYPE=$(df -T / | tail -1 | awk '{print $2}')

if [ "$FS_TYPE" = "xfs" ]; then
    xfs_growfs /
elif [ "$FS_TYPE" = "ext4" ]; then
    resize2fs /dev/nvme0n1p3
elif [ "$FS_TYPE" = "btrfs" ]; then
    btrfs filesystem resize max /
fi

# 2.6 验证结果
df -h /
lsblk /dev/nvme0n1
```

---

## 设备类型识别

| 设备类型 | 示例 | 重新扫描方法 |
|---------|------|-------------|
| NVMe | `/dev/nvme0n1` | `echo 1 > /sys/block/nvme0n1/device/rescan_controller` |
| VirtIO | `/dev/vda` | `blockdev --rereadpt /dev/vda` |
| SATA | `/dev/sda` | `blockdev --rereadpt /dev/sda` |

### 自动识别脚本

```bash
DEVICE="/dev/nvme0n1"

if [[ "$DEVICE" == /dev/nvme* ]]; then
    # NVMe 设备
    echo 1 > /sys/block/$(basename $DEVICE)/device/rescan_controller
elif [[ "$DEVICE" == /dev/vd* ]] || [[ "$DEVICE" == /dev/sd* ]]; then
    # VirtIO 或 SATA 设备
    blockdev --rereadpt $DEVICE
fi
```

---

## 故障排查

### 问题 1: 未找到 growpart 命令

```bash
# 安装工具包
yum install -y cloud-utils-growpart
```

### 问题 2: 云盘扩容后 OS 未识别新容量

```bash
# NVMe 设备：重新扫描控制器
echo 1 > /sys/block/nvme0n1/device/rescan_controller

# 验证
lsblk /dev/nvme0n1
```

### 问题 3: xfs_growfs 报错 "not mounted"

```bash
# XFS 必须使用挂载点，不能使用设备路径
xfs_growfs /data        # ✓ 正确
xfs_growfs /dev/vdb1    # ✗ 错误

# 确认挂载点
mount | grep vdb
```

### 问题 4: resize2fs 报错 "Device busy"

```bash
# EXT4 支持在线扩容，确保设备已挂载
# 如果未挂载，先挂载再扩容
mount /dev/vdb1 /data
resize2fs /dev/vdb1
```

### 问题 5: 扩容后容量未变化

```bash
# 逐步检查
lsblk /dev/nvme0n1      # 检查云盘容量（应显示新容量）
lsblk /dev/nvme0n1p3    # 检查分区容量（应显示新容量）
df -h /                 # 检查文件系统容量（应显示新容量）

# 根据结果定位问题所在步骤
```

### 问题 6: blockdev --rereadpt 报错 "Device or resource busy"

```bash
# 这是正常现象，当设备已挂载时无法重新读取分区表
# NVMe 设备使用 rescan_controller 方法
echo 1 > /sys/block/nvme0n1/device/rescan_controller

# 如果仍然无效，可能需要重启实例
```

---

## 最佳实践

### 1. 扩容前备份

```bash
# 创建快照备份
aliyun ecs CreateSnapshot \
  --DiskId d-bp1234567890abcdef \
  --SnapshotName "pre-resize-backup-$(date +%Y%m%d)"
```

### 2. 在线扩容流程

```bash
# 1. 云平台扩容
aliyun ecs ResizeDisk --DiskId <disk-id> --NewSize <new-size> --RegionId <region>

# 2. 等待云平台生效
sleep 10

# 3. 验证云平台扩容完成
aliyun ecs DescribeDisks --RegionId <region> --DiskIds "[\"<disk-id>\"]"

# 4. OS 内重新扫描
echo 1 > /sys/block/nvme0n1/device/rescan_controller

# 5. 扩容分区和文件系统
growpart /dev/nvme0n1 3
resize2fs /dev/nvme0n1p3  # 或 xfs_growfs /
```

### 3. 自动化脚本

```bash
#!/bin/bash
# auto-expand-disk.sh

set -e

DEVICE=$1
PARTITION=$2
NEW_SIZE=$3

# 自动检测文件系统类型
FS_TYPE=$(lsblk -f /dev/${DEVICE}${PARTITION} --noheadings --output FSTYPE | tr -d ' ')

# 扩容分区
growpart /dev/$DEVICE $PARTITION

# 扩容文件系统
if [ "$FS_TYPE" = "xfs" ]; then
    MOUNTPOINT=$(df /dev/${DEVICE}${PARTITION} | tail -1 | awk '{print $6}')
    xfs_growfs $MOUNTPOINT
elif [ "$FS_TYPE" = "ext4" ]; then
    resize2fs /dev/${DEVICE}${PARTITION}
fi

echo "✅ 扩容完成！"
df -h /
```

---

## 扩展资源

- **详细示例**: 参见 [examples.md](examples.md)
- **技术参考**: 参见 [reference.md](reference.md)
- **依赖技能**: [aliyun-ecs](../aliyun-ecs/SKILL.md)

---

**适用版本**: Alinux4  
**最后更新**: 2026-03-19  
**依赖技能**: aliyun-ecs

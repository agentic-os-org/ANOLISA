# Storage resize - 技术参考

## 完整 CLI 扩容脚本

### 系统盘扩容脚本（增强版）

```bash
#!/bin/bash
# expand_system_disk.sh
# 系统盘在线扩容完整脚本

set -e

REGION_ID=$(curl -s http://100.100.100.200/latest/meta-data/region-id)
INSTANCE_ID=$(curl -s http://100.100.100.200/latest/meta-data/instance-id)
NEW_SIZE=90

echo "========================================"
echo "  系统盘在线扩容脚本"
echo "========================================"
echo ""
echo "📋 实例信息:"
echo "   实例 ID: $INSTANCE_ID"
echo "   地域：$REGION_ID"
echo ""

# ========== 步骤 1: 获取系统盘信息 ==========
echo "🔍 查询系统盘信息..."
DISK_INFO=$(aliyun ecs DescribeDisks \
  --RegionId $REGION_ID \
  --InstanceId $INSTANCE_ID \
  --DiskType system)

SYSTEM_DISK_ID=$(echo "$DISK_INFO" | grep -o '"DiskId": "[^"]*"' | head -1 | cut -d'"' -f4)
CURRENT_SIZE=$(echo "$DISK_INFO" | grep -o '"Size": [0-9]*' | head -1 | awk '{print $2}')
DISK_CATEGORY=$(echo "$DISK_INFO" | grep -o '"Category": "[^"]*"' | head -1 | cut -d'"' -f4)

echo "   云盘 ID: $SYSTEM_DISK_ID"
echo "   当前容量：${CURRENT_SIZE}GB"
echo "   云盘类型：$DISK_CATEGORY"
echo ""

# ========== 步骤 2: 验证容量 ==========
if [ "$CURRENT_SIZE" -ge "$NEW_SIZE" ]; then
  echo "⚠️  警告：当前容量已大于等于目标容量"
  echo "   当前：${CURRENT_SIZE}GB, 目标：${NEW_SIZE}GB"
  exit 0
fi

# ========== 步骤 3: 创建快照（可选但推荐）==========
echo "📸 创建快照备份..."
SNAPSHOT_NAME="pre-resize-$(date +%Y%m%d-%H%M%S)"
aliyun ecs CreateSnapshot \
  --DiskId $SYSTEM_DISK_ID \
  --SnapshotName "$SNAPSHOT_NAME"
echo "   ✅ 快照创建请求已提交：$SNAPSHOT_NAME"
echo ""

# ========== 步骤 4: 执行扩容 ==========
echo "💳 执行云盘扩容：${CURRENT_SIZE}GB → ${NEW_SIZE}GB..."
RESIZE_RESULT=$(aliyun ecs ResizeDisk \
  --DiskId $SYSTEM_DISK_ID \
  --NewSize $NEW_SIZE \
  --RegionId $REGION_ID \
  --ClientToken $(cat /proc/sys/kernel/random/uuid 2>/dev/null || echo "resize-$(date +%s)"))

REQUEST_ID=$(echo "$RESIZE_RESULT" | grep -o '"RequestId": "[^"]*"' | cut -d'"' -f4)
echo "   RequestID: $REQUEST_ID"
echo ""

# ========== 步骤 5: 等待扩容完成 ==========
echo "⏳ 等待云平台扩容完成..."
sleep 15

# 验证云盘容量
NEW_DISK_INFO=$(aliyun ecs DescribeDisks \
  --RegionId $REGION_ID \
  --DiskIds "[\"$SYSTEM_DISK_ID\"]")

ACTUAL_SIZE=$(echo "$NEW_DISK_INFO" | grep -o '"Size": [0-9]*' | head -1 | awk '{print $2}')
echo "   云平台容量：${ACTUAL_SIZE}GB"

if [ "$ACTUAL_SIZE" -ne "$NEW_SIZE" ]; then
  echo "⚠️  警告：云平台容量未达预期"
fi
echo ""

# ========== 步骤 6: OS 内重新扫描设备 ==========
echo "🔍 OS 内重新扫描设备..."

# 识别设备类型
if lsblk | grep -q "nvme"; then
  DEVICE="nvme0n1"
  DEVICE_TYPE="NVMe"
  echo 1 > /sys/block/$DEVICE/device/rescan_controller
elif lsblk | grep -q "vd"; then
  DEVICE="vda"
  DEVICE_TYPE="VirtIO"
  blockdev --rereadpt /dev/$DEVICE 2>/dev/null || true
else
  DEVICE="sda"
  DEVICE_TYPE="SATA"
  blockdev --rereadpt /dev/$DEVICE 2>/dev/null || true
fi

sleep 2
OS_SIZE=$(lsblk /dev/$DEVICE --noheadings --output SIZE | tr -d ' ')
echo "   设备类型：$DEVICE_TYPE"
echo "   OS 识别容量：$OS_SIZE"
echo ""

# ========== 步骤 7: 扩容分区 ==========
echo "📀 扩容分区..."

# 获取根分区号
ROOT_PARTITION=$(df / | tail -1 | awk '{print $1}' | grep -o 'p[0-9]*$' | tr -d 'p')
if [ -z "$ROOT_PARTITION" ]; then
  ROOT_PARTITION=$(df / | tail -1 | awk '{print $1}' | grep -o '[0-9]*$')
fi

echo "   根分区号：$ROOT_PARTITION"
growpart /dev/$DEVICE $ROOT_PARTITION
echo ""

# ========== 步骤 8: 扩容文件系统 ==========
echo "📁 扩容文件系统..."

FS_TYPE=$(lsblk -f /dev/${DEVICE}p${ROOT_PARTITION} --noheadings --output FSTYPE | tr -d ' ')
echo "   文件系统类型：$FS_TYPE"

if [ "$FS_TYPE" = "xfs" ]; then
  xfs_growfs /
  echo "   ✅ XFS 文件系统扩容完成"
elif [ "$FS_TYPE" = "ext4" ]; then
  resize2fs /dev/${DEVICE}p${ROOT_PARTITION}
  echo "   ✅ EXT4 文件系统扩容完成"
elif [ "$FS_TYPE" = "btrfs" ]; then
  btrfs filesystem resize max /
  echo "   ✅ Btrfs 文件系统扩容完成"
else
  echo "⚠️  未知文件系统类型：$FS_TYPE"
fi
echo ""

# ========== 步骤 9: 验证结果 ==========
echo "========================================"
echo "  ✅ 扩容完成！"
echo "========================================"
echo ""
echo "📊 最终状态:"
df -h /
echo ""
lsblk /dev/$DEVICE
```

---

## 文件系统详细操作

### XFS 文件系统（Alinux4 默认）

```bash
# 1. 确认挂载点和文件系统类型
df -T | grep xfs
# 输出示例：/dev/nvme0n1p3  xfs      41943040  23253876  18689164  60% /

# 2. 确认分区信息
lsblk -f /dev/nvme0n1
# 输出示例：
# NAME        FSTYPE  LABEL  UUID                                 MOUNTPOINTS
# nvme0n1
# ├─nvme0n1p1
# ├─nvme0n1p2 vfat           xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx /boot/efi
# └─nvme0n1p3 xfs     root   xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx /

# 3. 扩容分区
growpart /dev/nvme0n1 3

# 4. 扩容文件系统（必须使用挂载点！）
xfs_growfs /

# 5. 验证
df -h /
xfs_info /
```

**重要注意事项：**
- `xfs_growfs` 必须使用挂载点作为参数，不能使用设备路径
- XFS 文件系统只能扩容，不能缩容
- XFS 支持在线扩容，无需卸载

### EXT4 文件系统

```bash
# 1. 确认设备路径
df -T | grep ext4
# 输出示例：/dev/nvme0n1p3  ext4     41943040  23253876  15757068  60% /

# 2. 扩容分区
growpart /dev/nvme0n1 3

# 3. 扩容文件系统（使用设备路径）
resize2fs /dev/nvme0n1p3

# 4. 验证
df -h /
dumpe2fs -h /dev/nvme0n1p3 | grep "Block count"
```

**重要注意事项：**
- `resize2fs` 使用设备路径作为参数
- EXT4 支持在线扩容和缩容
- 缩容前必须先卸载文件系统

### Btrfs 文件系统

```bash
# 1. 确认挂载点
df -T | grep btrfs

# 2. 扩容分区
growpart /dev/nvme0n1 3

# 3. 扩容文件系统
btrfs filesystem resize max /

# 4. 验证
df -h /
btrfs filesystem usage /
```

---

## NVMe 设备重新扫描详解

### 为什么需要重新扫描？

云盘在阿里云控制台扩容后，云平台的存储系统已经完成了扩容，但操作系统内核仍然使用旧的设备容量信息。对于 NVMe 设备，需要通过重新扫描控制器来让内核识别新的容量。

### 重新扫描方法

```bash
# 方法 1: 使用 sysfs（推荐）
echo 1 > /sys/block/nvme0n1/device/rescan_controller

# 方法 2: 使用 nvme-cli（需要安装）
nvme ns-rescan /dev/nvme0n1

# 验证扫描结果
lsblk /dev/nvme0n1
cat /sys/block/nvme0n1/size  # 单位：512 字节扇区
```

### 设备容量计算

```bash
# 从扇区数计算容量（GB）
SECTORS=$(cat /sys/block/nvme0n1/size)
SIZE_GB=$((SECTORS * 512 / 1024 / 1024 / 1024))
echo "磁盘容量：${SIZE_GB}GB"
```

---

## 故障排查详解

### 问题 1: 提示"未找到 growpart 命令"

```bash
# 原因：cloud-utils-growpart 未安装
# 解决：
yum install -y cloud-utils-growpart

# 验证：
which growpart
```

### 问题 2: 云盘扩容后 OS 未识别新容量

```bash
# 原因：NVMe 设备需要重新扫描控制器
# 解决：
echo 1 > /sys/block/nvme0n1/device/rescan_controller

# 验证：
lsblk /dev/nvme0n1
cat /sys/block/nvme0n1/size
```

### 问题 3: xfs_growfs 报错 "not mounted"

```bash
# 原因：XFS 必须使用挂载点，不能使用设备路径
# 错误示例：
xfs_growfs /dev/nvme0n1p3  # ✗

# 正确示例：
xfs_growfs /               # ✓

# 确认挂载点：
mount | grep nvme0n1p3
df /
```

### 问题 4: resize2fs 报错 "Device or resource busy"

```bash
# 原因 1: 设备未正确识别
# 解决：
lsblk /dev/nvme0n1  # 确认分区已更新

# 原因 2: 文件系统正在使用中（对于未挂载设备）
# 解决：EXT4 支持在线扩容，确保设备已挂载
mount /dev/nvme0n1p3 /data
resize2fs /dev/nvme0n1p3
```

### 问题 5: 扩容后容量未变化

```bash
# 逐步检查，定位问题所在步骤

# 1. 检查云盘容量（云平台层面）
aliyun ecs DescribeDisks --RegionId cn-hangzhou --DiskIds "[\"d-xxx\"]"
# 应显示新容量

# 2. 检查 OS 识别容量（内核层面）
lsblk /dev/nvme0n1
# 应显示新容量

# 3. 检查分区容量（分区表层面）
lsblk /dev/nvme0n1p3
# 如未更新，执行 growpart

# 4. 检查文件系统容量（文件系统层面）
df -h /
# 如未更新，执行 xfs_growfs 或 resize2fs
```

### 问题 6: blockdev --rereadpt 报错 "Device or resource busy"

```bash
# 原因：设备已挂载时无法重新读取分区表
# 解决：NVMe 设备使用 rescan_controller 方法

# NVMe 设备：
echo 1 > /sys/block/nvme0n1/device/rescan_controller

# 如果仍然无效，检查设备是否被占用：
lsof /dev/nvme0n1
fuser -vm /dev/nvme0n1
```

### 问题 7: growpart 报错 "NOCHANGE"

```bash
# 原因：分区表已经是最新状态
# 解决：检查分区容量是否已更新

lsblk /dev/nvme0n1
# 如果分区容量已更新，跳过此步骤

# 如果未更新，尝试手动删除并重建分区（危险操作！）
# 建议先创建快照备份
```

---

## 性能优化建议

### 1. 扩容前清理空间

```bash
# 清理包管理器缓存
yum clean all

# 清理旧内核
package-cleanup --oldkernels --count=1

# 清理临时文件
rm -rf /tmp/*
```

### 2. 扩容后优化

```bash
# XFS 文件系统优化
xfs_info /

# 检查是否需要重新平衡
xfs_fsr /
```

### 3. 监控扩容过程

```bash
# 监控磁盘 I/O
iostat -x 1

# 监控文件系统使用率
watch -n 1 'df -h /'
```

---

## 参考文档

### 阿里云官方文档
- [阿里云 CLI 安装与配置](https://help.aliyun.com/zh/cli/)
- [ResizeDisk API](https://help.aliyun.com/zh/ecs/API/ResizeDisk)
- [DescribeDisks API](https://help.aliyun.com/zh/ecs/API/DescribeDisks)
- [云盘扩容 FAQ](https://help.aliyun.com/zh/ecs/disk-resizing-and-shrinking-faqs)
- [NVMe 云盘在线扩容](https://help.aliyun.com/zh/ecs/user-guide/online-expand-nvme-disk)

### Alinux4 文档
- [Alinux4 产品使用手册](../../docs/Alinux4/KB704267_阿里云服务器操作系统 V4 产品使用手册-V2.pdf)
- [Alinux4 系统管理员手册](../../docs/Alinux4/KB704270_阿里云服务器操作系统 V4 系统管理员手册-V5.pdf)

### Linux 内核文档
- [XFS 文件系统文档](https://xfs.org/)
- [EXT4 文件系统文档](https://ext4.wiki.kernel.org/)
- [Btrfs 文件系统文档](https://btrfs.wiki.kernel.org/)

---

**适用版本**: Alinux4  
**最后更新**: 2026-03-19  
**依赖技能**: aliyun-ecs

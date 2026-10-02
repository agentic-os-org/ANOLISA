# Storage resize - 使用示例

## 示例一：系统盘在线扩容（NVMe 设备）

### 场景说明
- 实例类型：ECS（NVMe 存储）
- 当前容量：40GB
- 目标容量：90GB（扩容 50GB）
- 文件系统：EXT4

### 完整流程

```bash
#!/bin/bash
set -e

# ========== 步骤 1: 获取实例元数据 ==========
INSTANCE_ID=$(curl -s http://100.100.100.200/latest/meta-data/instance-id)
REGION_ID=$(curl -s http://100.100.100.200/latest/meta-data/region-id)

echo "📋 实例信息:"
echo "   实例 ID: $INSTANCE_ID"
echo "   地域：$REGION_ID"

# ========== 步骤 2: 查询系统盘信息 ==========
echo -e "\n🔍 查询系统盘信息..."
DISK_INFO=$(aliyun ecs DescribeDisks \
  --RegionId $REGION_ID \
  --InstanceId $INSTANCE_ID \
  --DiskType system)

SYSTEM_DISK_ID=$(echo "$DISK_INFO" | grep -o '"DiskId": "[^"]*"' | head -1 | cut -d'"' -f4)
CURRENT_SIZE=$(echo "$DISK_INFO" | grep -o '"Size": [0-9]*' | head -1 | awk '{print $2}')

echo "   系统盘 ID: $SYSTEM_DISK_ID"
echo "   当前容量：${CURRENT_SIZE}GB"

# ========== 步骤 3: 执行云平台扩容 ==========
NEW_SIZE=90
echo -e "\n💳 执行云盘扩容：${CURRENT_SIZE}GB → ${NEW_SIZE}GB..."

aliyun ecs ResizeDisk \
  --DiskId $SYSTEM_DISK_ID \
  --NewSize $NEW_SIZE \
  --RegionId $REGION_ID

echo "   ✅ 云平台扩容请求已提交"

# ========== 步骤 4: 等待云平台生效 ==========
echo -e "\n⏳ 等待云平台扩容完成..."
sleep 15

# 验证云平台扩容完成
NEW_DISK_INFO=$(aliyun ecs DescribeDisks \
  --RegionId $REGION_ID \
  --DiskIds "[\"$SYSTEM_DISK_ID\"]")

ACTUAL_SIZE=$(echo "$NEW_DISK_INFO" | grep -o '"Size": [0-9]*' | head -1 | awk '{print $2}')
echo "   云平台容量：${ACTUAL_SIZE}GB"

if [ "$ACTUAL_SIZE" -ne "$NEW_SIZE" ]; then
    echo "   ⚠️  云平台容量未达预期，继续尝试..."
fi

# ========== 步骤 5: OS 内重新扫描设备 ==========
echo -e "\n🔍 OS 内重新扫描 NVMe 设备..."
DEVICE="nvme0n1"

# NVMe 设备重新扫描控制器
echo 1 > /sys/block/$DEVICE/device/rescan_controller
sleep 2

# 验证 OS 识别新容量
OS_SIZE=$(lsblk /dev/$DEVICE --noheadings --output SIZE | tr -d ' ')
echo "   OS 识别容量：$OS_SIZE"

# ========== 步骤 6: 扩容分区 ==========
echo -e "\n📀 扩容分区..."
PARTITION=3  # 系统盘通常是分区 3

growpart /dev/$DEVICE $PARTITION

# ========== 步骤 7: 扩容文件系统 ==========
echo -e "\n📁 扩容文件系统..."

# 检测文件系统类型
FS_TYPE=$(lsblk -f /dev/${DEVICE}p${PARTITION} --noheadings --output FSTYPE | tr -d ' ')
echo "   文件系统类型：$FS_TYPE"

if [ "$FS_TYPE" = "xfs" ]; then
    xfs_growfs /
elif [ "$FS_TYPE" = "ext4" ]; then
    resize2fs /dev/${DEVICE}p${PARTITION}
elif [ "$FS_TYPE" = "btrfs" ]; then
    btrfs filesystem resize max /
fi

# ========== 步骤 8: 验证结果 ==========
echo -e "\n✅ 扩容完成！验证结果:"
df -h /
lsblk /dev/$DEVICE
```

---

## 示例二：数据盘在线扩容

### 场景说明
- 磁盘类型：数据盘
- 当前容量：100GB
- 目标容量：200GB
- 挂载点：/data
- 文件系统：XFS

### 完整流程

```bash
#!/bin/bash
set -e

DISK_ID="disk-bp1234567890abcdef"
REGION_ID="cn-hangzhou"
NEW_SIZE=200
MOUNTPOINT="/data"

# ========== 步骤 1: 查询数据盘信息 ==========
echo "🔍 查询数据盘信息..."
DISK_INFO=$(aliyun ecs DescribeDisks \
  --RegionId $REGION_ID \
  --DiskIds "[\"$DISK_ID\"]")

CURRENT_SIZE=$(echo "$DISK_INFO" | grep -o '"Size": [0-9]*' | head -1 | awk '{print $2}')
DEVICE_NAME=$(echo "$DISK_INFO" | grep -o '"Device": "[^"]*"' | head -1 | cut -d'"' -f4)

echo "   数据盘 ID: $DISK_ID"
echo "   当前容量：${CURRENT_SIZE}GB"
echo "   设备名称：$DEVICE_NAME"
echo "   挂载点：$MOUNTPOINT"

# ========== 步骤 2: 执行云平台扩容 ==========
echo -e "\n💳 执行云盘扩容：${CURRENT_SIZE}GB → ${NEW_SIZE}GB..."

aliyun ecs ResizeDisk \
  --DiskId $DISK_ID \
  --NewSize $NEW_SIZE \
  --RegionId $REGION_ID

echo "   ✅ 云平台扩容请求已提交"

# ========== 步骤 3: 等待云平台生效 ==========
echo -e "\n⏳ 等待云平台扩容完成..."
sleep 15

# ========== 步骤 4: OS 内重新扫描设备 ==========
echo -e "\n🔍 OS 内重新扫描设备..."

# 从设备名称提取基本设备名（如 /dev/vdb -> vdb）
BASE_DEVICE=$(basename $DEVICE_NAME | sed 's/p[0-9]*$//')

if [[ "$BASE_DEVICE" == nvme* ]]; then
    echo 1 > /sys/block/$BASE_DEVICE/device/rescan_controller
else
    blockdev --rereadpt /dev/$BASE_DEVICE 2>/dev/null || true
fi

sleep 2

# ========== 步骤 5: 扩容分区 ==========
echo -e "\n📀 扩容分区..."

# 获取分区号
PARTITION_NUM=$(lsblk /dev/$BASE_DEVICE --noheadings --output NAME | grep -o "${BASE_DEVICE}p[0-9]*" | head -1 | sed "s/${BASE_DEVICE}p//")

growpart /dev/$BASE_DEVICE $PARTITION_NUM

# ========== 步骤 6: 扩容文件系统 ==========
echo -e "\n📁 扩容文件系统..."

FS_TYPE=$(lsblk -f /dev/${BASE_DEVICE}p${PARTITION_NUM} --noheadings --output FSTYPE | tr -d ' ')

if [ "$FS_TYPE" = "xfs" ]; then
    xfs_growfs $MOUNTPOINT
elif [ "$FS_TYPE" = "ext4" ]; then
    resize2fs /dev/${BASE_DEVICE}p${PARTITION_NUM}
fi

# ========== 步骤 7: 验证结果 ==========
echo -e "\n✅ 扩容完成！验证结果:"
df -h $MOUNTPOINT
lsblk /dev/$BASE_DEVICE
```

---

## 示例三：不同文件系统扩容对比

### XFS 文件系统扩容

```bash
# 设备：/dev/nvme0n1p3，挂载点：/

# 1. 扩容分区
growpart /dev/nvme0n1 3

# 2. 扩容文件系统（使用挂载点！）
xfs_growfs /

# 3. 验证
df -h /
```

### EXT4 文件系统扩容

```bash
# 设备：/dev/nvme0n1p3，挂载点：/

# 1. 扩容分区
growpart /dev/nvme0n1 3

# 2. 扩容文件系统（使用设备路径！）
resize2fs /dev/nvme0n1p3

# 3. 验证
df -h /
```

### Btrfs 文件系统扩容

```bash
# 设备：/dev/nvme0n1p3，挂载点：/

# 1. 扩容分区
growpart /dev/nvme0n1 3

# 2. 扩容文件系统
btrfs filesystem resize max /

# 3. 验证
df -h /
```

---

## 示例四：创建快照后备份再扩容

```bash
#!/bin/bash
set -e

DISK_ID="d-bp1234567890abcdef"
REGION_ID="cn-hangzhou"
SNAPSHOT_NAME="pre-resize-backup-$(date +%Y%m%d-%H%M%S)"

# ========== 步骤 1: 创建快照 ==========
echo "📸 创建快照备份..."

aliyun ecs CreateSnapshot \
  --DiskId $DISK_ID \
  --SnapshotName "$SNAPSHOT_NAME"

echo "   ✅ 快照创建请求已提交"

# ========== 步骤 2: 等待快照完成 ==========
echo -e "\n⏳ 等待快照完成..."
# 实际使用中应轮询检查快照状态

# ========== 步骤 3: 执行扩容 ==========
# ...（参考示例一的扩容流程）
```

---

## 示例五：批量扩容多个数据盘

```bash
#!/bin/bash
set -e

REGION_ID="cn-hangzhou"
NEW_SIZE=500

# 磁盘 ID 列表
DISK_IDS=(
    "disk-bp1111111111111111"
    "disk-bp2222222222222222"
    "disk-bp3333333333333333"
)

for DISK_ID in "${DISK_IDS[@]}"; do
    echo -e "\n========================================"
    echo "处理磁盘：$DISK_ID"
    echo "========================================"
    
    # 云平台扩容
    aliyun ecs ResizeDisk \
      --DiskId $DISK_ID \
      --NewSize $NEW_SIZE \
      --RegionId $REGION_ID
    
    echo "   ✅ 云平台扩容请求已提交"
    sleep 5
done

echo -e "\n✅ 所有磁盘云平台扩容请求已提交"
echo "   请分别登录各实例执行 OS 内扩容操作"
```

---

## 示例六：健康检查

```bash
#!/bin/bash

DEVICE="/dev/nvme0n1"
PARTITION="/dev/nvme0n1p3"

echo "🔍 磁盘健康检查"
echo "========================================"

# 1. 检查磁盘容量
echo -e "\n1. 磁盘容量:"
lsblk $DEVICE

# 2. 检查分区表
echo -e "\n2. 分区表:"
fdisk -l $DEVICE

# 3. 检查文件系统类型
echo -e "\n3. 文件系统类型:"
lsblk -f $PARTITION

# 4. XFS 文件系统检查
echo -e "\n4. XFS 文件系统检查:"
xfs_repair -n $PARTITION

# 5. EXT4 文件系统检查（如果是 EXT4）
# e2fsck -n -f $PARTITION

# 6. 检查挂载状态
echo -e "\n5. 挂载状态:"
df -h | grep -E "Filesystem|${PARTITION}"

echo -e "\n✅ 健康检查完成"
```

---

## 示例七：使用 Metadata 自动获取实例信息

```bash
#!/bin/bash
set -e

# 从 Metadata 服务获取实例信息
INSTANCE_ID=$(curl -s http://100.100.100.200/latest/meta-data/instance-id)
REGION_ID=$(curl -s http://100.100.100.200/latest/meta-data/region-id)
ZONE_ID=$(curl -s http://100.100.100.200/latest/meta-data/zone-id)

echo "📋 实例元数据:"
echo "   实例 ID: $INSTANCE_ID"
echo "   地域：$REGION_ID"
echo "   可用区：$ZONE_ID"

# 查询系统盘
SYSTEM_DISK=$(aliyun ecs DescribeDisks \
  --RegionId $REGION_ID \
  --InstanceId $INSTANCE_ID \
  --DiskType system)

echo -e "\n💿 系统盘信息:"
echo "$SYSTEM_DISK" | grep -E '"DiskId"|"Size"|"Category"'
```

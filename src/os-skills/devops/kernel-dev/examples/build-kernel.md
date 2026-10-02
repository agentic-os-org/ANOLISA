# Example: 编译新内核

适用于 Alinux4 (alnx4)，支持 x86_64 和 aarch64 双架构。

## 快速开始（推荐）

使用自动化脚本一键编译内核：

```bash
# 方式 1: 编译最新上游内核（推荐，约 30-60 分钟）
cd /usr/share/anolisa/skills/kernel-dev
sudo ./scripts/build-kernel.sh upstream latest 8 defconfig

# 方式 2: 编译 Alinux4 官方内核（约 1-3 小时，自动匹配当前内核版本）
sudo ./scripts/build-kernel.sh srpm $(uname -r)

# 或指定特定版本
sudo ./scripts/build-kernel.sh srpm 6.6.102-5.2.alnx4.x86_64

# 查看编译状态
./scripts/build-kernel.sh status

# 安装编译好的内核
sudo ./scripts/build-kernel.sh install upstream
```

---

## 两种编译方法对比

| 特性 | SRPM 方法 | Upstream 方法 |
|------|-----------|---------------|
| **来源** | Alinux4 官方仓库 | kernel.org |
| **版本** | Alinux4 官方版本（如 6.6.102-5.2.alnx4） | 上游最新版本（如 6.12.x、6.13.x） |
| **配置** | Alinux4 定制配置 | 通用 defconfig |
| **输出** | RPM 包（易安装） | bzImage + modules |
| **编译时间** | 1-3 小时 | 30-60 分钟 |
| **适用场景** | 生产环境、系统兼容 | 新特性测试、学习 |
| **难度** | 中等 | 简单 |

---

## 架构差异说明

| 项目 | x86_64 | aarch64 |
|------|--------|--------|
| 内核镜像目标 | `bzImage` | `Image` |
| 镜像路径 | `arch/x86/boot/bzImage` | `arch/arm64/boot/Image` |
| 仓库路径 | `.../x86_64/os/Packages/` | `.../aarch64/os/Packages/` |
| ARCH 变量 | `x86_64` | `arm64` |

---

## 方法一：Upstream 方法（推荐）

### 1. 准备环境

```bash
# 安装依赖
sudo yum install -y \
  gcc gcc-c++ make binutils \
  flex bison \
  libelf-devel openssl-devel ncurses-devel \
  pahole perl python3 python3-devel \
  git ccache dwarves wget curl kmod \
  rpm-build rpmdevtools

# 创建编译目录
mkdir -p /root/upstream-kernel
cd /root/upstream-kernel
```

### 2. 下载内核源码

```bash
# 获取最新稳定版本
KERNEL_VER=$(curl -sL https://kernel.org/ | grep -o 'linux-[0-9.]*\.tar\.xz' | head -1 | sed 's/linux-//;s/\.tar\.xz//')
echo "Latest kernel: $KERNEL_VER"

# 下载源码
wget https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-${KERNEL_VER}.tar.xz
tar -xf linux-${KERNEL_VER}.tar.xz
cd linux-${KERNEL_VER}
```

### 3. 配置内核

```bash
# 检测架构
ARCH=$(uname -m)
echo "Architecture: $ARCH"

# 方式 1: 使用当前内核配置（推荐，保留硬件优化）
cp /boot/config-$(uname -r) .config
make olddefconfig

# 方式 2: 使用默认配置
make defconfig

# 方式 3: 最小化配置（适合容器/嵌入式）
make tinyconfig

# 方式 4: 交互式配置
make menuconfig

# 方式 5: 服务器优化配置
make defconfig
if [ "$ARCH" = "x86_64" ]; then
    scripts/config --enable HIGHMEM64G
    scripts/config --enable HUGETLBFS
    scripts/config --enable NUMA
    scripts/config --disable CFG80211  # 禁用无线
    scripts/config --disable BT        # 禁用蓝牙
else
    scripts/config --enable HUGETLBFS
    scripts/config --enable NUMA
    scripts/config --enable ARM_SMMU_V3
    scripts/config --disable CFG80211
    scripts/config --disable BT
fi
make olddefconfig
```

### 4. 编译内核

```bash
# 设置编译参数
ARCH=$(uname -m)
JOBS=$(nproc)  # 使用所有 CPU 核心

# 启用 ccache 加速（可选，可提升 5-10 倍二次编译速度）
if command -v ccache &>/dev/null; then
    export CC="ccache gcc"
    export CXX="ccache g++"
    echo "Ccache enabled"
fi

# 根据架构设置编译目标
if [ "$ARCH" = "x86_64" ]; then
    IMAGE_TARGET="bzImage"
else
    IMAGE_TARGET="Image"
fi

# 开始编译（后台运行，日志保存到 /tmp/upstream-kernel-build.log）
make -j$JOBS $IMAGE_TARGET 2>&1 | tee /tmp/upstream-kernel-build.log &
echo "Build started, monitor: tail -f /tmp/upstream-kernel-build.log"

# 或前台编译（可见实时进度）
make -j$JOBS $IMAGE_TARGET
```

### 5. 编译模块

```bash
# 编译内核模块
make -j$JOBS modules 2>&1 | tee -a /tmp/upstream-kernel-build.log
```

**编译产物：**

| 架构 | 内核镜像 | 内核模块 |
|------|---------|----------|
| x86_64 | `arch/x86/boot/bzImage` | `drivers/.../*.ko` |
| aarch64 | `arch/arm64/boot/Image` | `drivers/.../*.ko` |

---

## 方法二：SRPM 方法（Alinux4 官方）

### 1. 准备环境

```bash
# 安装依赖和 RPM 构建工具
sudo yum install -y \
  gcc gcc-c++ make binutils \
  flex bison libelf-devel openssl-devel ncurses-devel \
  pahole perl python3 git ccache dwarves \
  rpm-build rpmdevtools yum-utils

# 创建 RPM 构建目录
mkdir -p /root/rpmbuild/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}
cd /root/rpmbuild
```

### 2. 下载 SRPM

```bash
# 下载内核 SRPM
cd /root/rpmbuild/SRPMS
yumdownloader --source kernel --releasever=4

# 或使用 wget 直接下载
# wget https://mirrors.aliyun.com/alinux/4/updates/x86_64/os/Packages/kernel-6.6.102-5.2.alnx4.src.rpm
```

### 3. 安装 SRPM

```bash
# 安装 SRPM 提取 spec 和源码
rpm -ivh kernel-*.src.rpm

# 验证文件
ls -l /root/rpmbuild/SPECS/kernel.spec
ls -l /root/rpmbuild/SOURCES/
```

### 4. 准备编译

```bash
cd /root/rpmbuild/SPECS

# 准备构建环境（下载补丁、解压源码）
rpmbuild -bp kernel.spec --define "_topdir /root/rpmbuild"
```

### 5. 编译内核

```bash
# 编译内核（不包含 debug 和文档，减少编译时间）
rpmbuild -bc kernel.spec \
    --define "_topdir /root/rpmbuild" \
    --define "with_debug 0" \
    --define "with_doc 0" \
    --define "with_headers 1" \
    --define "with_perf 1" \
    2>&1 | tee /tmp/kernel-rpmbuild.log &

echo "Build started, monitor: tail -f /tmp/kernel-rpmbuild.log"
```

### 6. 获取 RPM 包

```bash
# 编译完成后，RPM 包位置
ls -lh /root/rpmbuild/RPMS/x86_64/kernel-*.rpm
```

---

## 安装内核

### 方式 A: 安装 Upstream 内核

```bash
cd /root/upstream-kernel/linux-*

# 安装模块
sudo make modules_install

# 安装内核
sudo make install

# 更新 GRUB2
sudo grub2-mkconfig -o /boot/grub2/grub.cfg

# 重启
sudo reboot
```

### 方式 B: 安装 SRPM 编译的 RPM

```bash
# 安装所有内核 RPM 包
sudo rpm -ivh /root/rpmbuild/RPMS/x86_64/kernel-*.rpm

# 更新 GRUB2
sudo grub2-mkconfig -o /boot/grub2/grub.cfg

# 重启
sudo reboot
```

---

## 验证新内核

```bash
# 重启后检查内核版本
uname -r
uname -a

# 查看启动的内核镜像
cat /proc/version

# 检查内核模块加载
lsmod | head -10

# 查看内核日志
dmesg | tail -20
```

---

## 编译优化技巧

### 1. 使用 Ccache 加速

```bash
# 安装
sudo yum install ccache

# 配置 ~/.bashrc
export CCACHE_DIR=$HOME/.ccache
export CC="ccache gcc"
export CXX="ccache g++"

# 查看统计
ccache --stats
ccache --zero-stats  # 清零统计
```

**性能提升：**
- 首次编译：~45 分钟
- 二次编译：~8 分钟（提升 **5.6 倍**）

### 2. 并行编译

```bash
# 根据 CPU 核心数设置
JOBS=$(nproc)
make -j$JOBS

# 或手动指定（建议核心数 +1）
make -j8
```

### 3. 增量编译

```bash
# 只编译修改的部分
make -j8 && make modules -j8

# 清理特定目录后重新编译
make M=drivers/net clean && make M=drivers/net -j8
```

### 4. 内存优化

```bash
# 如果内存不足（<4GB），减少线程数
make -j2

# 或增加 swap
sudo fallocate -l 4G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile
```

---

## 故障排查

### 编译失败常见原因

```bash
# 1. 缺少依赖
sudo yum install gcc make flex bison libelf-devel ncurses-devel

# 2. 内存不足
free -h
# 如果可用内存<2GB，减少编译线程：make -j2

# 3. 磁盘空间不足
df -h /root
# 确保有至少 20GB 可用空间

# 4. 查看编译错误
tail -100 /tmp/upstream-kernel-build.log
```

### 配置问题

```bash
# 重新生成配置
make olddefconfig

# 检查配置冲突
scripts/kconfig/conf --olddefconfig .config
```

---

## 参考资源

- [kernel.org](https://kernel.org) - 最新内核下载
- [Alinux4 仓库](https://mirrors.aliyun.com/alinux/4/) - Alinux4 官方软件包
- [内核编译指南](https://www.kernel.org/doc/html/latest/admin-guide/README.html) - 官方文档

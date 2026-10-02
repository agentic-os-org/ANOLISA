---
name: kernel-dev
version: 1.0.0
description: Linux 内核研发自动化技能（RPM 包管理系统），提供内核编译（SRPM/Upstream 双方法）、内核 module 开发环境搭建、依赖安装、示例代码生成等功能。Linux 官方内核及上游最新内核，兼容 x86_64 和 aarch64 双架构。使用场景：内核定制开发、驱动模块研发、内核漏洞修复验证。适用于 RPM 包管理的 Linux 系统（如 Alibaba Cloud Linux）。
layer: core
lifecycle: production
---

# Kernel Development - Linux 内核研发自动化

## 核心定位

**五大功能：**
1. **依赖检测与安装** - 自动检测并安装内核开发所需的软件包
2. **工具链安装** - 安装编译器（gcc）、make、git 等开发工具
3. **内核 devel 包安装** - 下载并安装当前系统内核的 devel 包
4. **内核编译** - 支持两种编译方法：
   - **SRPM 方法**：Alinux4 官方内核，输出 RPM 包，适合生产环境
   - **Upstream 方法**：kernel.org 最新内核，编译快速，适合新特性测试
5. **Module 编译测试** - 测试编译示例内核 module

**支持架构：**
- `x86_64` — Intel/AMD 64 位服务器
- `aarch64` — ARM 64 位服务器（如倚天 710）

**重要约束：**
- 🔒 需要 root 权限执行内核相关操作
- 📦 仅Linux操作系统
- ⚠️ 内核编译会消耗大量系统资源（建议至少 4GB 内存）
- ⏱️ 编译时间：SRPM 方法 1-3 小时，Upstream 方法 30-60 分钟

---

## 快速开始

### 一键搭建开发环境

```bash
# 1. 检测系统架构
ARCH=$(uname -m)
KERNEL_VER=$(uname -r)
echo "架构: $ARCH | 内核: $KERNEL_VER"
# x86_64 示例输出：架构: x86_64 | 内核: 6.6.102-5.2.alnx4.x86_64
# aarch64 示例输出：架构: aarch64 | 内核: 6.6.102-5.2.alnx4.aarch64

# 2. 检查 Alinux4 系统
grep -i 'alinux\|alnx' /etc/os-release || echo "[警告] 非 Alinux4 系统"

# 3. 安装所有依赖（两种架构通用）
sudo yum install -y \
  gcc gcc-c++ make binutils \
  flex bison \
  libelf-devel openssl-devel ncurses-devel \
  pahole perl python3 python3-devel \
  git ccache dwarves wget curl kmod \
  rpm-build rpmdevtools

# 4. 安装内核 devel 包（yum 自动匹配当前架构）
sudo yum install -y kernel-devel-$KERNEL_VER kernel-headers-$KERNEL_VER

# 5. 验证环境
ls -l /lib/modules/$KERNEL_VER/build
gcc --version

# 6. 编译内核（可选，两种方法）
# 方法 A: Upstream 方法（推荐，快速）
sudo ./scripts/build-kernel.sh upstream latest 8 defconfig

# 方法 B: SRPM 方法（Alinux4 官方）
sudo ./scripts/build-kernel.sh srpm 6.6.102-5.2.alnx4.x86_64
```

### 创建第一个内核模块

```bash
MODULE_NAME="hello_module"
mkdir -p $MODULE_NAME && cd $MODULE_NAME

# 创建模块代码
cat > ${MODULE_NAME}.c << 'EOF'
#include <linux/init.h>
#include <linux/module.h>
#include <linux/kernel.h>

MODULE_LICENSE("GPL");
MODULE_AUTHOR("Alinux4 Kernel Dev");
MODULE_DESCRIPTION("Hello World kernel module for alnx4");
MODULE_VERSION("0.1");

static int __init hello_init(void)
{
    printk(KERN_INFO "hello_module: Module loaded (alnx4)\n");
    return 0;
}

static void __exit hello_exit(void)
{
    printk(KERN_INFO "hello_module: Module unloaded\n");
}

module_init(hello_init);
module_exit(hello_exit);
EOF

# 创建 Makefile
cat > Makefile << 'EOF'
obj-m += hello_module.o

KERNEL_DIR := /lib/modules/$(shell uname -r)/build
PWD := $(shell pwd)

all:
	make -C $(KERNEL_DIR) M=$(PWD) modules

clean:
	make -C $(KERNEL_DIR) M=$(PWD) clean

install:
	sudo insmod hello_module.ko

unload:
	sudo rmmod hello_module

.PHONY: all clean install unload
EOF

# 编译和加载
make
sudo make install

# 查看日志
dmesg | tail

# 卸载
sudo make unload
```

---

## Step-by-Step 环境搭建

### Step 1: 检测系统架构与信息

```bash
# 检测架构并设置架构相关变量
ARCH=$(uname -m)
KERNEL_VER=$(uname -r)

echo "系统架构: $ARCH"
echo "内核版本: $KERNEL_VER"
# x86_64 典型输出：6.6.102-5.2.alnx4.x86_64
# aarch64 典型输出：6.6.102-5.2.alnx4.aarch64

# 查看操作系统版本
cat /etc/os-release

# 检查是否为 Alinux4 系统
grep -i 'alinux\|alnx' /etc/os-release

# 确认架构支持
case "$ARCH" in
    x86_64)  echo "[✓] x86_64 架构，支持" ;;
    aarch64) echo "[✓] aarch64 架构，支持" ;;
    *)       echo "[✗] 不支持的架构: $ARCH" ; exit 1 ;;
esac
```

### Step 2: 安装内核开发依赖

以下依赖包为 x86_64 和 aarch64 通用，yum 会自动匹配当前架构下载对应包：

```bash
# 通用依赖（两种架构均适用）
sudo yum install -y \
  gcc gcc-c++ make binutils \
  flex bison \
  libelf-devel openssl-devel ncurses-devel \
  pahole perl python3 python3-devel \
  git ccache dwarves wget curl kmod
```

**依赖包说明：**

| 软件包 | 用途 | 必需 | 架构 |
|--------|------|------|------|
| gcc, gcc-c++ | GNU 编译器 | ✓ | 通用 |
| make | 构建工具 | ✓ | 通用 |
| binutils | 二进制工具 | ✓ | 通用 |
| flex, bison | 词法/语法分析器 | ✓ | 通用 |
| libelf-devel | ELF 文件处理 | ✓ | 通用 |
| openssl-devel | SSL/TLS 库 | ✓ | 通用 |
| ncurses-devel | 终端 UI 库 | ✓ | 通用 |
| pahole | DWARF 调试工具 | ✓ | 通用 |
| kmod | 内核模块工具 | ✓ | 通用 |
| git | 版本控制 | ○ | 通用 |
| ccache | 编译加速 | ○ | 通用 |

> **注意**：yum 仓库的 baseurl 中架构路径不同：
> - x86_64: `https://mirrors.aliyun.com/alinux/4/updates/x86_64/os/`
> - aarch64: `https://mirrors.aliyun.com/alinux/4/updates/aarch64/os/`

**检查依赖是否已安装：**

```bash
echo "当前架构: $(uname -m)"
for pkg in gcc make flex bison libelf-devel openssl-devel ncurses-devel pahole perl python3 kmod; do
    if rpm -q $pkg &>/dev/null; then
        echo "[✓] $pkg 已安装"
    else
        echo "[✗] $pkg 未安装"
    fi
done
```

### Step 3: 安装内核 devel 包

```bash
# 获取当前内核版本和架构
KERNEL_VER=$(uname -r)
ARCH=$(uname -m)
echo "当前内核：$KERNEL_VER （架构：$ARCH）"
# x86_64 示例：6.6.102-5.2.alnx4.x86_64
# aarch64 示例：6.6.102-5.2.alnx4.aarch64

# 安装匹配的 devel 包和 headers 包（yum 自动匹配架构）
sudo yum install -y kernel-devel-$KERNEL_VER kernel-headers-$KERNEL_VER

# 验证安装
ls -l /lib/modules/$KERNEL_VER/build
```

**验证成功标志：**
```
# x86_64:
/lib/modules/6.6.102-5.2.alnx4.x86_64/build -> ../../usr/src/kernels/...
# aarch64:
/lib/modules/6.6.102-5.2.alnx4.aarch64/build -> ../../usr/src/kernels/...
```

### Step 4: 验证编译环境

```bash
# 检查 GCC 版本
gcc --version

# 检查 Make 版本
make --version

# 检查 kmod 版本
kmod --version

# 检查内核头文件
ls /usr/include/linux/*.h | head -5

# 测试编译简单模块
mkdir -p /tmp/kernel_test && cd /tmp/kernel_test

cat > test_module.c << 'EOF'
#include <linux/init.h>
#include <linux/module.h>
MODULE_LICENSE("GPL");
static int __init test_init(void) { return 0; }
static void __exit test_exit(void) {}
module_init(test_init);
module_exit(test_exit);
EOF

cat > Makefile << 'EOF'
obj-m += test_module.o
KERNEL_DIR := /lib/modules/$(shell uname -r)/build
PWD := $(shell pwd)
all:
	make -C $(KERNEL_DIR) M=$(PWD) modules
clean:
	make -C $(KERNEL_DIR) M=$(PWD) clean
EOF

make && ls -lh test_module.ko
```

---

## 内核 Module 开发

### 创建自定义 Module

以下脚本可创建一个完整的内核模块项目，包含源码和 Makefile：

```bash
#!/bin/bash
# 用法: bash create_module.sh <module_name>
MODULE_NAME="${1:?请提供模块名称}"

if [ -d "$MODULE_NAME" ]; then
    echo "[✗] 目录已存在：$MODULE_NAME"
    exit 1
fi

mkdir -p "$MODULE_NAME" && cd "$MODULE_NAME"

# 创建模块代码
cat > ${MODULE_NAME}.c << CEOF
/* ${MODULE_NAME} - Kernel Module for Alinux4 */
#include <linux/init.h>
#include <linux/module.h>
#include <linux/kernel.h>

MODULE_LICENSE("GPL");
MODULE_AUTHOR("Alinux4 Kernel Dev");
MODULE_DESCRIPTION("${MODULE_NAME}: A simple kernel module for alnx4");
MODULE_VERSION("0.1");

static int __init ${MODULE_NAME}_init(void)
{
    printk(KERN_INFO "${MODULE_NAME}: Module loaded (alnx4)\n");
    printk(KERN_INFO "${MODULE_NAME}: Hello, Alinux4 Kernel!\n");
    return 0;
}

static void __exit ${MODULE_NAME}_exit(void)
{
    printk(KERN_INFO "${MODULE_NAME}: Module unloaded\n");
}

module_init(${MODULE_NAME}_init);
module_exit(${MODULE_NAME}_exit);
CEOF

# 创建 Makefile
cat > Makefile << MEOF
obj-m += ${MODULE_NAME}.o

KERNEL_DIR := /lib/modules/\$(shell uname -r)/build
PWD := \$(shell pwd)

all:
	make -C \$(KERNEL_DIR) M=\$(PWD) modules

clean:
	make -C \$(KERNEL_DIR) M=\$(PWD) clean

install:
	sudo insmod ${MODULE_NAME}.ko

unload:
	sudo rmmod ${MODULE_NAME}

status:
	dmesg | tail -20 | grep ${MODULE_NAME}

.PHONY: all clean install unload status
MEOF

echo "[✓] Module 文件已创建:"
echo "  - ${MODULE_NAME}/${MODULE_NAME}.c"
echo "  - ${MODULE_NAME}/Makefile"
echo ""
echo "快速开始:"
echo "  cd ${MODULE_NAME} && make && sudo make install"
```

### 编译和测试 Module

```bash
cd my_module

# 编译
make

# 查看生成的文件
ls -lh *.ko

# 显示模块信息
modinfo my_module.ko

# 加载模块（需要 root）
sudo insmod my_module.ko

# 检查是否加载成功
lsmod | grep my_module

# 查看内核日志
dmesg | tail -10

# 卸载模块
sudo rmmod my_module

# 再次查看日志
dmesg | tail -10
```

---

## 故障排查

### 自动化诊断工具

**使用环境检查脚本：**

```bash
# 检查所有依赖
./scripts/check-env.sh

# 自动安装缺失的依赖
sudo ./scripts/install-deps.sh

# 一键搭建环境
sudo ./scripts/setup.sh
```

### 常见问题

**Q1: 缺少内核头文件**

```bash
# 检查 devel 包
rpm -qa | grep kernel-devel

# 安装匹配的 devel 包
sudo yum install kernel-devel-$(uname -r) kernel-headers-$(uname -r)

# 验证链接
ls -l /lib/modules/$(uname -r)/build
```

**Q2: GCC 版本不匹配**

```bash
# 查看 GCC 版本
gcc --version

# 重新安装 GCC
sudo yum reinstall gcc gcc-c++
```

**Q3: 编译时内存不足**

```bash
# 减少编译线程数
make -j2

# 或增加 swap
sudo fallocate -l 4G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile
```

**Q4: insmod 失败 - 无效格式**

```bash
# 检查内核版本匹配
uname -r
modinfo my_module.ko | grep vermagic

# 如果不匹配，重新编译
make clean && make
```

**Q5: Secure Boot 阻止模块加载**

```bash
# 临时禁用 Secure Boot（重启时按 F2/Del 进入 BIOS）
# 或为模块签名
sudo mokutil --disable-validation
```

**Q6: 缺少 kmod 工具**

```bash
# 安装 kmod
sudo yum install kmod

# 验证
kmod --version
```

**Q7: 编译警告 "compiler differs from the one used to build the kernel"**

这是正常警告，表示当前 GCC 版本与编译内核时的 GCC 版本略有不同。
只要主版本相同（如都是 GCC 12.3），通常可以安全忽略。

```bash
# 查看内核编译版本
cat /proc/version

# 查看当前 GCC 版本
gcc --version
```

**Q8: BTF generation skipped**

```bash
# 警告信息：Skipping BTF generation due to unavailability of vmlinux
# 这是正常现象，BTF 是可选功能，不影响模块基本功能
# 如需 BTF 支持，需要编译完整的内核
```

### 诊断命令

```bash
# 检查已安装的依赖
rpm -qa | grep -E 'gcc|make|flex|bison|kmod'

# 检查内核头文件
ls /usr/include/linux/*.h | wc -l

# 检查 Kbuild 系统
ls -l /lib/modules/$(uname -r)/build/Makefile

# 查看模块依赖
modinfo my_module.ko

# 查看内核日志
dmesg | tail -50

# 检查内核版本
cat /etc/system-release
uname -r
```

---

## 验收标准

### 环境搭建验收

| 步骤 | 检查命令 | 成功标志 |
|------|---------|---------|
| 1️⃣ 依赖安装 | `gcc --version` | GCC >= 10.0 |
| 2️⃣ 工具链 | `make --version` | Make >= 4.0 |
| 3️⃣ kmod 工具 | `kmod --version` | kmod 已安装 |
| 4️⃣ 内核头文件 | `ls /usr/include/linux` | 头文件存在 |
| 5️⃣ Kbuild | `ls /lib/modules/$(uname -r)/build` | build 链接存在 |
| 6️⃣ 编译测试 | `make` (测试模块) | .ko 文件生成 |
| 7️⃣ 加载测试 | `sudo insmod test.ko` | 无报错，dmesg 有输出 |

### Module 开发验收

```bash
# 1. 创建 module（使用上面的 create_module.sh 脚本）
bash create_module.sh test_mod

# 2. 编译 module
cd test_mod && make

# 3. 生成 .ko 文件
ls -lh test_mod.ko  # 应该有文件

# 4. 加载 module
sudo insmod test_mod.ko  # 无报错

# 5. 查看日志
dmesg | tail  # 应有 "test_mod: Module loaded"

# 6. 卸载 module
sudo rmmod test_mod  # 无报错
```

---

## 最佳实践

### 1. 使用 ccache 加速编译

```bash
# 安装
sudo yum install ccache

# 配置 ~/.bashrc
export CCACHE_DIR=$HOME/.ccache
export CC="ccache gcc"
export CXX="ccache g++"

# 查看统计
ccache --stats
```

**编译速度提升：**
- 首次编译：~45 分钟
- 二次编译：~8 分钟（提升 5.6 倍）

### 2. 保存内核配置

```bash
# 备份当前配置
cp /boot/config-$(uname -r) ~/kernel-configs/my-config

# 恢复配置
cp ~/kernel-configs/my-config /usr/src/kernels/linux-*/.config
make olddefconfig
```

### 3. 增量编译

```bash
# 只编译修改的部分
make -j8 && make modules -j8

# 清理特定模块
make M=drivers/net clean && make M=drivers/net
```

### 4. 版本控制

```bash
cd my_module
git init
git add .
git commit -m "Initial kernel module"
```

---

## Examples

- [编译新内核](examples/build-kernel.md) — **双方法编译指南**（SRPM/Upstream），包含配置优化、编译加速、安装流程
- [编译示例内核 Module](examples/build-module.md) — Hello World 内核模块的创建、编译、加载、卸载
- [示例模块集合](examples/README.md) — 包含 hello_module、param_module、proc_module、char_device

---

## Scripts

| 脚本 | 描述 |
|------|------|
| `scripts/build-kernel.sh` | **内核编译自动化**（支持 SRPM/Upstream 双方法） |
| `scripts/check-env.sh` | 检查环境依赖 |
| `scripts/install-deps.sh` | 自动安装依赖 |
| `scripts/setup.sh` | 一键搭建开发环境 |
| `scripts/verify-env.sh` | 全面验证环境（含测试编译） |
| `scripts/test-module.sh` | 自动化测试内核模块 |

**build-kernel.sh 用法：**
```bash
# Upstream 方法（推荐，快速）
./scripts/build-kernel.sh upstream latest 8 defconfig

# SRPM 方法（Alinux4 官方）
./scripts/build-kernel.sh srpm $(uname -r)

# 查看编译状态
./scripts/build-kernel.sh status

# 安装编译好的内核
./scripts/build-kernel.sh install upstream
```

---

## Documentation

| 文档 | 描述 |
|------|------|
| [docs/troubleshooting.md](docs/troubleshooting.md) | **内核编译故障排查指南**（10+ 常见问题及解决方案） |
| [docs/module-signing.md](docs/module-signing.md) | 内核模块签名指南（Secure Boot） |
| [docs/build-performance.md](docs/build-performance.md) | 编译性能优化指南（ccache、并行编译） |

---

## References

- [Alinux4 软件仓库](references/alinux4-repo.md) — 仓库地址、YUM 配置、常用内核包
- [内核开发资源](references/kernel-resources.md) — 内核源码、官方文档、驱动开发指南等

---

**适用版本**: Alinux4 (alnx4)
**支持架构**: x86_64, aarch64
**支持内核**:
  - Alinux4 官方内核（通过 SRPM 编译，如 6.6.102-5.2.alnx4）
  - 上游最新内核（通过 kernel.org，如 6.12.x、6.13.x）
**dist 字段**: alnx4
**软件仓库 (x86_64)**: https://mirrors.aliyun.com/alinux/4/updates/x86_64/os/Packages/
**软件仓库 (aarch64)**: https://mirrors.aliyun.com/alinux/4/updates/aarch64/os/Packages/
**最后更新**: 2026-03-17

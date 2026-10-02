# Build Performance Guide - 编译性能优化指南

内核模块编译可能耗时较长，本指南介绍如何优化编译速度。

---

## 1. 使用 ccache 加速编译

ccache（Compiler Cache）可以缓存编译结果，显著加快重复编译速度。

### 安装 ccache

```bash
sudo yum install ccache
```

### 配置 ccache

**方法一：修改 ~/.bashrc（推荐）**

```bash
# 添加到 ~/.bashrc
export CCACHE_DIR=$HOME/.ccache
export CC="ccache gcc"
export CXX="ccache g++"

# 使配置生效
source ~/.bashrc
```

**方法二：创建符号链接**

```bash
# 创建 ccache 链接
mkdir -p ~/bin/ccache
cd ~/bin/ccache
ln -s /usr/bin/ccache gcc
ln -s /usr/bin/ccache g++
ln -s /usr/bin/ccache cc

# 添加到 PATH（在 ~/.bashrc 中）
export PATH=$HOME/bin/ccache:$PATH
```

### 配置 ccache 参数

```bash
# 设置缓存大小（默认 5GB，建议 10-20GB）
ccache --max-size=20G

# 查看统计
ccache --stats

# 清除缓存
ccache --clean
```

### 验证 ccache 工作

```bash
# 首次编译
make clean && make

# 查看统计
ccache --stats

# 二次编译
make clean && make

# 再次查看统计（命中率应提高）
ccache --stats
```

**典型效果：**
- 首次编译：~45 分钟
- 二次编译：~8 分钟（提升 5.6 倍）
- 缓存命中率：80-95%

---

## 2. 并行编译

使用多核 CPU 加速编译。

### Make 并行选项

```bash
# 使用所有 CPU 核心
make -j$(nproc)

# 或指定核心数
make -j8

# 编译内核模块
make -C /lib/modules/$(uname -r)/build M=$(pwd) modules -j8
```

### 在 Makefile 中配置

```makefile
# 自动检测 CPU 核心数
NPROCS := $(shell nproc)

all:
	make -C $(KERNEL_DIR) M=$(PWD) modules -j$(NPROCS)
```

---

## 3. 增量编译

只编译修改的部分。

### 模块增量编译

```bash
# 修改源文件后，直接编译（无需 clean）
make

# 仅编译特定模块
make M=drivers/net clean
make M=drivers/net
```

### 避免不必要的 clean

```bash
# ❌ 不推荐：每次都清理
make clean && make

# ✅ 推荐：直接编译
make
```

---

## 4. 使用更快的存储

### SSD vs HDD

| 存储类型 | 编译时间 |
|---------|---------|
| HDD | ~45 分钟 |
| SATA SSD | ~15 分钟 |
| NVMe SSD | ~8 分钟 |

### 使用 tmpfs 编译

```bash
# 创建 tmpfs 挂载点（需要足够内存）
sudo mkdir -p /mnt/ramdisk
sudo mount -t tmpfs -o size=8G tmpfs /mnt/ramdisk

# 复制源码到 ramdisk
cp -r ~/kernel-module /mnt/ramdisk/
cd /mnt/ramdisk/kernel-module

# 编译
make

# 复制结果回原目录
cp *.ko ~/kernel-module/
```

---

## 5. 减少依赖扫描

### 跳过不必要的检查

```bash
# 直接编译，跳过依赖检查
make modules

# 而不是
make all  # 包含 check-deps
```

### 简化 Makefile

对于快速迭代开发，使用简化 Makefile：

```makefile
obj-m += my_module.o
KERNEL_DIR := /lib/modules/$(shell uname -r)/build

all:
	$(MAKE) -C $(KERNEL_DIR) M=$(PWD) modules

clean:
	$(MAKE) -C $(KERNEL_DIR) M=$(PWD) clean
```

---

## 6. 预编译头文件

### 启用预编译头文件

```bash
# 在内核配置中启用
echo "CONFIG_PREEMPT=y" >> .config
make olddefconfig
```

---

## 7. 优化内核配置

### 精简内核配置

```bash
# 基于当前配置
cp /boot/config-$(uname -r) .config

# 只启用必要选项
scripts/config --disable CONFIG_DEBUG_INFO
scripts/config --disable CONFIG_DEBUG_INFO_BTF

# 重新生成配置
make olddefconfig
```

### 禁用调试选项

```bash
# 减少编译时间
scripts/config --disable CONFIG_FRAME_POINTER
scripts/config --disable CONFIG_RANDOMIZE_BASE
```

---

## 8. 使用编译缓存服务

### 分布式编译（distcc）

```bash
# 安装 distcc
sudo yum install distcc

# 配置主机列表
echo "localhost/4" > ~/.distcc/hosts

# 使用 distcc 编译
make CC=distcc gcc -j8
```

---

## 性能对比

| 优化方法 | 首次编译 | 二次编译 | 提升倍数 |
|---------|---------|---------|---------|
| 无优化 | 45 分钟 | 45 分钟 | 1x |
| 并行编译 (-j8) | 15 分钟 | 15 分钟 | 3x |
| ccache | 45 分钟 | 8 分钟 | 5.6x |
| ccache + 并行 | 15 分钟 | 3 分钟 | 15x |
| + NVMe SSD | 8 分钟 | 2 分钟 | 22x |

---

## 推荐配置

### 开发环境推荐

```bash
# 1. 安装 ccache
sudo yum install ccache

# 2. 配置 ~/.bashrc
cat >> ~/.bashrc << 'EOF'
export CCACHE_DIR=$HOME/.ccache
export CC="ccache gcc"
export CXX="ccache g++"
export PATH=$PATH:/usr/lib/ccache
EOF

source ~/.bashrc

# 3. 设置缓存大小
ccache --max-size=20G

# 4. 使用并行编译
export MAKEFLAGS="-j$(nproc)"
```

### 验证配置

```bash
# 检查 ccache
which gcc
# 应显示：/usr/bin/gcc（ccache 透明拦截）

# 查看 ccache 统计
ccache --stats

# 编译测试
cd examples/hello_module
make clean && make
ccache --stats
```

---

## 故障排查

### Q1: ccache 未命中率高

```bash
# 查看统计
ccache --stats

# 可能原因：
# - 编译器版本变化
# - 编译选项变化
# - 文件路径变化

# 解决方法：清理缓存
ccache --clean
```

### Q2: 编译失败

```bash
# 临时禁用 ccache
export CC="gcc"
make clean && make
```

### Q3: 内存不足

```bash
# 减少并行度
make -j2

# 增加 swap
sudo fallocate -l 8G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile
```

---

## 参考资源

- [ccache 官方文档](https://ccache.dev/)
- [Linux 内核编译指南](https://www.kernel.org/doc/html/latest/kbuild/)
- [Make 并行编译](https://www.gnu.org/software/make/manual/html_node/Parallel.html)

---

**适用版本**: Alinux4 (alnx4)  
**支持架构**: x86_64, aarch64

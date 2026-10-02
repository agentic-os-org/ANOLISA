# Kernel Development Skill - Alinux4

阿里云 Alinux4 操作系统内核研发自动化 Skill。

## 功能特性

✅ **依赖检测与安装** - 自动检测并安装内核开发所需的软件包  
✅ **工具链安装** - 安装编译器（gcc）、make、kmod 等开发工具  
✅ **内核 devel 包安装** - 下载并安装当前系统内核的 devel 包  
✅ **Module 编译测试** - 测试编译示例内核 module  
✅ **示例代码生成** - 一键生成可编译的内核 module  

## 快速开始

### 1. 一键搭建开发环境

```bash
cd kernel-dev

# 完整环境搭建（推荐）
python scripts/kernel_dev.py --setup

# 分步执行
python scripts/kernel_dev.py --install-deps    # 安装依赖
python scripts/kernel_dev.py --install-devel   # 安装 devel 包
python scripts/kernel_dev.py --test-module     # 测试编译
```

### 2. 创建第一个内核模块

```bash
# 创建示例 module
python scripts/kernel_dev.py --create hello_module

# 编译和加载
cd hello_module
make
sudo make install

# 查看日志
dmesg | tail

# 卸载
sudo make unload
```

### 3. 手动搭建环境（学习用）

```bash
# 安装依赖包（包含 kmod）
sudo yum install -y gcc gcc-c++ make binutils \
  flex bison libelf-devel openssl-devel ncurses-devel \
  pahole perl python3 git ccache kmod

# 安装内核 devel 包
sudo yum install kernel-devel-$(uname -r) kernel-headers-$(uname -r)

# 验证安装
gcc --version && ls /lib/modules/$(uname -r)/build
```

## 主要脚本

| 脚本 | 功能 | 使用场景 |
|------|------|---------|
| `kernel_dev.py` | ⭐ 唯一主脚本 | 环境搭建、module 创建、编译测试 |

## 支持的 Module 类型

- **basic** - 基础模块（Hello World）
- 更多类型可通过修改模板实现

## 系统要求

- **操作系统**: Alinux4 (alnx4)
- **内核版本**: 6.6.102-5.2.alnx4.x86_64
- **内存**: 至少 2GB（推荐 4GB+ 用于内核编译）
- **磁盘**: 至少 5GB 可用空间
- **权限**: 需要 root 权限

## 依赖包

核心依赖会自动安装：

```bash
gcc, gcc-c++, make, binutils
flex, bison
libelf-devel, openssl-devel, ncurses-devel
pahole, perl, python3, kmod
```

可选工具：

```bash
git, ccache, dwarves, wget, curl
```

## 故障排查

### 常见问题

**Q: 缺少内核头文件**

```bash
# 从 Alinux4 仓库安装
sudo yum install kernel-devel-$(uname -r) kernel-headers-$(uname -r)

# 或访问仓库页面下载
# https://mirrors.aliyun.com/alinux/4/updates/x86_64/os/Packages/
```

**Q: GCC 版本不兼容**

```bash
sudo yum reinstall gcc gcc-c++
```

**Q: 编译时内存不足**

```bash
make -j2  # 减少线程数
```

**Q: insmod 失败 - 无效格式**

```bash
# 检查版本匹配
uname -r
modinfo hello_module.ko | grep vermagic

# 重新编译
make clean && make
```

**Q: 缺少 kmod 工具**

```bash
sudo yum install kmod
```

## 项目结构

```
kernel-dev/
├── SKILL.md                      # Skill 配置文件
├── README.md                     # 本文件
└── scripts/
    └── kernel_dev.py             # 主脚本
```

## 最佳实践

### 1. 使用 ccache 加速编译

```bash
sudo yum install ccache
export CC="ccache gcc"
export CXX="ccache g++"

# 编译速度提升 5-10 倍
```

### 2. 保存内核配置

```bash
cp /boot/config-$(uname -r) .config.backup
```

### 3. 版本控制

```bash
git init
git add .
git commit -m "Initial kernel module"
```

### 4. 配置 Alinux4 仓库

```bash
# 确保仓库配置正确
cat /etc/yum.repos.d/alinux4.repo

# 刷新缓存
yum makecache
```

## 参考资源

- [Skill 详细文档](SKILL.md)
- [Alinux4 官方文档](https://www.alibabacloud.com/help/zh/alinux)
- [Alinux4 软件仓库](https://mirrors.aliyun.com/alinux/4/updates/x86_64/os/Packages/)
- [内核编译指南](https://www.kernel.org/doc/html/latest/admin-guide/quickstart.html)
- [Linux 设备驱动](https://lwn.net/Kernel/LDD3/)

---

**适用版本**: Alinux4 (alnx4)  
**内核版本**: 6.6.102-5.2.alnx4.x86_64  
**dist 字段**: alnx4  
**最后更新**: 2026-03-16

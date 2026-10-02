# Alinux4 软件仓库

## 仓库地址

| 资源 | 地址 |
|------|------|
| 主仓库（x86_64） | https://mirrors.aliyun.com/alinux/4/updates/x86_64/os/Packages/ |
| 主仓库（aarch64） | https://mirrors.aliyun.com/alinux/4/updates/aarch64/os/Packages/ |
| Alinux4 官方文档 | https://www.alibabacloud.com/help/zh/alinux |

## 配置 YUM 仓库

以下配置会自动检测架构并使用对应的仓库 URL：

```bash
# 检测架构
ARCH=$(uname -m)
echo "当前架构: $ARCH"

# 添加仓库配置（自动适配架构）
sudo tee /etc/yum.repos.d/alinux4.repo << EOF
[alinux4-updates]
name=Alinux4 Updates - $ARCH
baseurl=https://mirrors.aliyun.com/alinux/4/updates/$ARCH/os/
enabled=1
gpgcheck=0
EOF

# 刷新缓存
sudo yum makecache

# 搜索内核相关包
yum search kernel-devel
```

## 常用内核包

- `kernel-devel-$(uname -r)` — 内核开发头文件
- `kernel-headers-$(uname -r)` — 内核头文件
- `kernel-6.6.102-*.src.rpm` — 内核源码 RPM 包

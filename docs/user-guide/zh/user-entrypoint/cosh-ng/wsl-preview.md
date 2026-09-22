# 在 Windows 上通过 WSL2 运行 cosh-ng（预览）

[English](../../../en/user-entrypoint/cosh-ng/wsl-preview.md)

Windows 用户可以通过社区预览 launcher 试用 cosh-ng。launcher 在 Git Bash
中运行，通过 `wsl.exe` 进入 WSL2，并在与当前 Windows 工作目录对应的目录
中启动已安装在 Linux 发行版内的 `cosh-shell`。这是一条预览路径，不是
Windows 原生移植：平台支持矩阵保持不变。

请牢记以下四点，launcher 每次启动时也会打印这些声明。

- cosh-ng 实际运行在 WSL2 中的 Linux 上。
- Git Bash 只提供 Windows 侧入口。
- `/mnt/c` 等挂载目录可能存在性能和权限差异。
- 配置与凭据保存在 WSL 用户环境中。

## 前置条件

- Windows 已启用 WSL2 并安装了一个 Linux 发行版（在管理员 PowerShell 中
  运行 `wsl --install` 即可完成；默认发行版为 Ubuntu）。
- 已在该发行版内安装 cosh-ng。在 WSL 终端中按照
  [快速开始](QUICKSTART.md) 的 Linux 步骤操作。
- 从本仓库获取 launcher 脚本 `cosh-wsl`。在 Git Bash 中执行：

```bash
mkdir -p ~/bin
curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/cosh-ng/scripts/cosh-wsl -o ~/bin/cosh-wsl
chmod +x ~/bin/cosh-wsl
export PATH="$HOME/bin:$PATH"
```

## 启动会话

在 Git Bash 中进入项目目录，然后运行 launcher：

```bash
cd /c/work/your-project
cosh-wsl
```

launcher 会用 `wslpath` 转换 Windows 工作目录，并在 WSL2 内对应的目录
启动 `cosh-shell`。如需使用其他发行版，设置 `COSH_WSL_DISTRO`：

```bash
COSH_WSL_DISTRO=Debian cosh-wsl
```

## 预览范围

预览覆盖交互式终端、cosh-core、认证和常用文件工具。`pkg`、`svc` 系统
操作命令、workspace checkpoint 和 Gateway 不在本次预览范围内，此路径
不予支持。

`/mnt/c` 等 Windows 盘符挂载目录上的文件访问可能明显变慢，且无法保留
Linux 权限位。建议把日常工作放在 Linux 文件系统中（例如 `~/` 下），把
`/mnt/c` 当作交换区使用。

## 反馈

本次预览用于在更大投入之前验证真实需求。如遇问题或确认该路径可用，
请在 [GitHub issue #2770](https://github.com/alibaba/anolisa/issues/2770)
上反馈。

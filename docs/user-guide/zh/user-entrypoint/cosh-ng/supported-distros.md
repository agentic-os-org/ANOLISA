# 支持的平台与Linux发行版

[English](../../../en/user-entrypoint/cosh-ng/supported-distros.md)

cosh-ng的交互式终端可在Linux和macOS上运行。软件包和服务命令使用主机原生的管理工具。

| 平台 | 交互式Shell | 软件包命令 | 服务命令 |
|---|---|---|---|
| Linux | Bash或zsh | dnf、apt或zypper | systemd |
| macOS | Bash或zsh | Homebrew | 不可用 |

## Linux发行版

以下`/etc/os-release` ID有内置路由：

| ID | 包管理器 |
|---|---|
| `alinux`、`centos`、`fedora` | dnf |
| `ubuntu`、`debian` | apt |
| `opensuse-leap`、`opensuse-tumbleweed`、`sles` | zypper |

未列出的发行版如果`ID_LIKE`包含以下值之一，可以复用对应的软件包家族：

| `ID_LIKE`家族 | 包管理器 |
|---|---|
| `alinux`、`centos`、`fedora`、`rhel` | dnf |
| `debian`、`ubuntu` | apt |
| `opensuse`、`suse` | zypper |

家族路由只表示软件包后端兼容，不代表对每个衍生版或版本做认证。未知软件包家族会返回结构化的`UnsupportedDistro`错误。

## 修改主机前

安装前运行`anolisa env`。在目标主机上，先使用`cosh-cli`只读命令和对应操作的`--dry-run`选项确认路由，再执行软件包或服务变更。服务命令需要Linux和systemd；macOS用户可以通过Homebrew使用软件包命令，但不能使用`cosh-cli svc`。

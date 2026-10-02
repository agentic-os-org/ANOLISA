# Supported Platforms and Linux Distributions

[中文版](../../../zh/user-entrypoint/cosh-ng/supported-distros.md)

cosh-ng can run the interactive terminal on Linux and macOS. Package and service commands use the host's native management tools.

| Platform | Interactive shell | Package commands | Service commands |
|---|---|---|---|
| Linux | Bash or zsh | dnf, apt, or zypper | systemd |
| macOS | Bash or zsh | Homebrew | Not available |

## Linux distributions

These `/etc/os-release` IDs have built-in routing:

| ID | Package manager |
|---|---|
| `alinux`, `centos`, `fedora` | dnf |
| `ubuntu`, `debian` | apt |
| `opensuse-leap`, `opensuse-tumbleweed`, `sles` | zypper |

An unlisted distribution can use a package family when its `ID_LIKE` contains one of these values:

| `ID_LIKE` family | Package manager |
|---|---|
| `alinux`, `centos`, `fedora`, `rhel` | dnf |
| `debian`, `ubuntu` | apt |
| `opensuse`, `suse` | zypper |

Family routing means the package backend is compatible; it is not certification of every derivative or release. An unknown package family returns a structured `UnsupportedDistro` error.

## Before changing the host

Run `anolisa env` before installation. On the target host, use read-only `cosh-cli` commands and the action's `--dry-run` option to verify routing before package or service mutations. Service commands require Linux with systemd; macOS users can use package commands through Homebrew but not `cosh-cli svc`.

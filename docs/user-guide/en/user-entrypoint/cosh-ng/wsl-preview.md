# Run cosh-ng on Windows with WSL2 (preview)

[中文版](../../../zh/user-entrypoint/cosh-ng/wsl-preview.md)

Windows users can try cosh-ng through a community preview launcher. The
launcher runs in Git Bash, enters WSL2 through `wsl.exe`, and starts the
`cosh-shell` already installed inside a Linux distribution, in the directory
that matches your current Windows working directory. This is a preview path,
not a native Windows port: the supported platform matrix is unchanged.

Keep these four points in mind. The launcher prints them on every start.

- cosh-ng actually runs on Linux inside WSL2.
- Git Bash is only the Windows-side entry point.
- Directories under `/mnt/c` and other mounts may have performance and
  permission differences.
- Configuration and credentials stay in the WSL user environment.

## Prerequisites

- Windows with WSL2 enabled and a Linux distribution installed
  (`wsl --install` from an elevated PowerShell sets up both; Ubuntu is the
  default distribution).
- cosh-ng installed inside that distribution. Follow the Linux steps in the
  [quick start](QUICKSTART.md) from a WSL terminal.
- The launcher script `cosh-wsl` from this repository. From Git Bash:

```bash
mkdir -p ~/bin
curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/cosh-ng/scripts/cosh-wsl -o ~/bin/cosh-wsl
chmod +x ~/bin/cosh-wsl
export PATH="$HOME/bin:$PATH"
```

## Start a session

From Git Bash, go to your project directory and run the launcher:

```bash
cd /c/work/your-project
cosh-wsl
```

The launcher converts the Windows working directory with `wslpath` and starts
`cosh-shell` in the matching directory inside WSL2. To use a different
distribution, set `COSH_WSL_DISTRO`:

```bash
COSH_WSL_DISTRO=Debian cosh-wsl
```

## Preview scope

The preview covers the interactive terminal, cosh-core, authentication, and
common file tools. The `pkg` and `svc` system-operation commands, workspace
checkpoints, and the Gateway are not part of this preview and are not
supported on this path.

Files under `/mnt/c` and other Windows drive mounts can be noticeably slower
and may not preserve Linux permission bits. For the best experience, keep
active work in the Linux filesystem (for example under `~/`) and treat
`/mnt/c` as an exchange area.

## Feedback

This preview exists to validate real demand before any larger investment.
Report problems or confirm that the path works for you on
[GitHub issue #2770](https://github.com/alibaba/anolisa/issues/2770).

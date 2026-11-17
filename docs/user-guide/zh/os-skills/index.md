# OS 技能库

OS Skills 是面向 AI Agent 的系统管理与 DevOps 技能库。它提供预构建的技能，使 Agent 能够执行常见的系统管理和自动化任务。

---

## 概述

OS Skills 覆盖三大领域：

- **系统管理** — 用户管理、服务控制、包管理、文件系统操作
- **云集成** — 云资源查询、实例管理、网络配置
- **DevOps 自动化** — CI/CD 流水线管理、容器操作、部署工作流

---

## 安装

```bash
anolisa install os-skills
```

源码安装需要 Python 3.6 或更新版本及 GNU Make。发布清单决定默认技能集合，
安装器只使用 Python 标准库。从源码检出目录安装供 cosh 使用的技能集合：

```bash
make -C src/os-skills install
```

如果 `python3` 不存在或版本过旧，为同一命令添加 `PYTHON3=/path/to/python3`。
安装器在修改文件前检查解释器版本。

普通用户继续使用原有的 `~/.copilot-shell/skills/` 目录；root 默认安装到
`/usr/local/share/anolisa/skills/`。系统资源的 `AWPRJ` 默认为 `anolisa`，可显式覆盖。
使用 `SKILLS_DIR=/absolute/path` 可选择其他技能集合目录；卸载时使用相同的值。
启动新的 cosh 会话后，运行 `/skills` 检查已安装的技能。

`INSTALL_PROFILE=user` 的默认路径由 `HOME` 推导；`HOME` 未设置、为空、为相对路径
或为 `/` 时安装直接失败，不会写到 `/` 下。没有家目录时，请显式传入 `PREFIX`、
`DATADIR` 和 `SKILLS_DIR`。

技能同名时，本地系统副本优先于 `/usr/share/anolisa/skills/` 中的 RPM 副本。
同一集合应选择一种交付方式；管理旧的源码安装时，将 `SKILLS_DIR` 指向原目录。

`make uninstall` 只删除 `make install` 记录在
`<datadir>/anolisa/components/os-skills/install-manifest` 中的技能目录。
缺少该记录时不会删除任何内容，并以非零状态退出，因此不会误删 os-skills
从未安装过的同名技能目录。`make install` 反向同理：遇到不是自己安装的同名目录
会直接拒绝，只有显式传入 `REPLACE_UNOWNED=1` 才会整体替换并记录，供后续卸载删除。
只有 `1` 会开启替换；`0` 或不设置时保持拒绝，其他取值会在改动任何内容前报错。
因此在此版本之前完成的安装，需要先执行一次 `REPLACE_UNOWNED=1 make install`，
之后才能正常卸载。被记录的目录整体归 os-skills 管理，每次安装都会整体替换，
请勿在这些目录中放置个人文件，自建技能请使用其他名称。安装失败或被中断时会放回原有技能，
记录保持不变；若记录已写入新技能后才被中断，则保留新安装的技能，并提示再次执行 `make install`
完成安装。若某一步无法撤销（例如集合目录变为只读），错误信息会指出遗留内容：
未能删除的新副本，或仍保存原有技能的 `.os-skills.*/.old` 目录。记录会保留历次安装的技能名，
新版本移除的技能在卸载时也会一并删除。
RPM 安装请通过包管理器卸载，raw `anolisa install` 部署请使用
`anolisa uninstall os-skills`，而不是 `make uninstall`。

一份记录只对应一个集合：`make install` 会拒绝记录中未登记的目录，
`make uninstall` 会连同技能一起删除记录，因此同时维护多个集合时请为每个
集合单独指定 `MANIFEST=`。卸载其中一个集合不会影响其他集合的记录与共享组件元数据。

---

## 快速开始

安装后，OS Skills 可供任何 ANOLISA 兼容的 Agent 运行时使用。Agent 可通过自然语言调用技能：

```
> "检查所有已挂载文件系统的磁盘使用情况"
> "重启 nginx 服务"
> "显示运行中的容器及其资源使用"
```

---

## 技能分类

### 系统管理

| 技能 | 说明 |
|------|------|
| `disk-usage` | 检查文件系统磁盘使用 |
| `service-ctl` | 启动/停止/重启系统服务 |
| `process-mgmt` | 列出和管理进程 |
| `user-mgmt` | 用户和组管理 |
| `package-ops` | 包安装/移除/查询 |

### DevOps 自动化

| 技能 | 说明 |
|------|------|
| `container-ops` | Docker/Podman 容器管理 |
| `log-analysis` | 搜索和分析系统日志 |
| `network-diag` | 网络诊断（ping、traceroute、端口检查） |
| `cron-mgmt` | Cron 任务管理 |

---

## 与 Agent 运行时集成

OS Skills 与 cosh 及其他 ANOLISA 兼容运行时自动集成。技能在启动时被发现并加入 Agent 的工具清单。

```bash
# 验证技能已加载
anolisa status os-skills
```

---

## 配置

配置文件：`~/.config/os-skills/config.toml`

```toml
[skills]
# 启用的技能类别
enabled = ["system", "devops"]

[safety]
# 对破坏性操作要求确认
confirm_destructive = true
```

---

## 参见

- [anolisa CLI](../getting-started/anolisa-cli.md)

# anolisa CLI

`anolisa` CLI 是 ANOLISA 组件的统一生命周期入口。它负责解析组件来源、
维护按 scope 隔离的安装记录、将 RPM transaction 委托给 native package
manager，并诊断或修复状态漂移。

---

## 安装

### 方式 A：安装脚本（推荐）

```bash
curl -fsSL https://get.agentic-os.sh | bash
```

### 方式 B：YUM（Alinux）

```bash
sudo yum install anolisa
```

验证安装：

```bash
anolisa --version
```

---

## Scope 与可见性

`--install-mode user` 写入当前用户目录，`--install-mode system` 写入
system state，通常需要 root。未指定时，root 默认使用 system mode，普通
用户默认使用 user mode。

只读命令使用 user-plus-system 视图。因此普通用户可以看到和诊断 system
安装，adapter discovery 也可以使用其发布的 contract。修改命令仍然只写入
显式选择的 scope；同一组件的 user 安装和 system 安装可以共存。

---

## 命令

### install

通过配置的 raw 或 RPM backend 安装单个组件，或规划 index 中的全部组件：

```bash
anolisa --install-mode user install <component>
sudo anolisa --install-mode system install <component>
anolisa install --all
```

另一个 scope 中已有安装，不会让当前 scope 变成“already installed”。已有
记录的重装或变更由 lifecycle planner 处理，不会被静默覆盖。

RPM 安装支持 Yum 3 和 DNF 4。ANOLISA 优先使用 yum，不存在时使用 dnf。
安装前可使用 `sudo` 检查依赖和冲突：

```bash
sudo anolisa --install-mode system --dry-run install cosh --backend rpm
```

此检查不会安装或更改软件包。请先解决报告的冲突再重试；检查通过不保证
后续安装一定成功。指定版本与系统版本锁等限制冲突时，命令会失败，不会
替换为其他版本。

如果 ANOLISA 无法识别系统包管理器，请按提示手工安装缺失依赖后重试。

### uninstall

从所选 scope 移除一个安装：

```bash
anolisa uninstall <component>
anolisa uninstall <component> --purge
sudo anolisa --install-mode system uninstall <component> --remove-system-package
```

ANOLISA-owned 文件和 managed RPM package 由各自的 owner backend 移除。
adopted 或 observed system RPM 默认保留；只有确实要移除 native package 时
才使用 `--remove-system-package`。

### update

更新一个组件、全部已记录组件、CLI 本身，或运行只读 RPM 更新检查：

```bash
anolisa update <component>
anolisa update all
anolisa update self
anolisa update --check
```

`update all` 不更新 CLI 本身。delegated 成员会在可行时合并成一次 native
transaction，但每个组件仍保留独立的 recovery journal 和 record。

### list 与 status

查看有效的 user-plus-system 视图：

```bash
anolisa list
anolisa list --installed
anolisa status
anolisa status <component>
```

user view 中两个 scope 都有同名组件时，user record 为 active，system record
仍作为 shadowed state 可见。system-mode view 只读取 system root，不枚举其他
用户的 state。

未跟踪的 RPM 观察使用与安装相同的包解析器。index 映射优先于历史包 alias
和 capability：提供 `anolisa-component(cosh)` 的旧 cosh-ng RPM 显示在
cosh-ng 下，cosh 仍解析到 copilot-shell。`action=install` 表示生命周期操作，
并不代表已检查 native 依赖；请通过 RPM install dry-run 检查这些包能否共存。

### doctor

运行只读的 health、dependency、service、state 与 recovery journal 检查：

```bash
anolisa doctor
anolisa doctor <component>
anolisa --dry-run doctor <component>
```

`doctor` 扫描当前 visibility view 中的全部 root：user mode 包含 user root 和
可读的 system root，system mode 只包含 system root。当前调用不能修改某个
system root 时，它会在修复建议中补全
`sudo anolisa --install-mode system`。`--fix` 在当前版本中仍为保留参数；请
显式执行输出的 `fix_plan`。

对于 raw 安装，`status` 和 `doctor` 允许修改声明为 `type = "config"` 的文件
内容。文件缺失、不安全路径、意外符号链接、权限或 capability 偏移仍会检查失败；
普通 data 和 executable 文件仍需通过 SHA-256 校验。旧安装记录会从保存的
component manifest 在内存中恢复无歧义的 config 声明。若重叠的目录声明混合了
config 和不可变类型，旧文件因缺少来源映射而继续进行摘要校验。
请先备份编辑过的配置，再重新安装组件以记录准确类型。诊断不会改写状态文件。

### restart

重启所选 scope 安装记录中的 service：

```bash
anolisa --dry-run --install-mode user restart <component>
anolisa --install-mode user restart <component>
anolisa --dry-run --install-mode system restart <component>
sudo anolisa --install-mode system restart <component>
```

`--dry-run` 只列出将要重启的 unit，不会执行 `systemctl daemon-reload`
或 `systemctl restart`。system 模式预览只读已记录状态，不获取排他安装锁，
因此不需要 state root 的写权限。

### upgrade

预览或应用 system/RPM image 升级。raw-managed 组件会报告为 skipped，不会被
迁移到其他 backend：

```bash
anolisa --install-mode system --dry-run upgrade
sudo anolisa --install-mode system upgrade
sudo anolisa --install-mode system upgrade --target <profile>
```

### adopt、repair 与 forget

在不混淆 package ownership 的前提下管理状态：

```bash
sudo anolisa --install-mode system adopt <component>
sudo anolisa --install-mode system repair <component>
anolisa --install-mode user forget <component>
sudo anolisa --install-mode system forget <component>
```

`adopt` 将已有 system RPM 记录为 delegated-adopted，不取得 native removal
authority。`repair` 协调指定 scope 的 record 与 rpmdb 或中断 journal。
`forget` 只删除所选 scope 的记录，绝不执行 package 或 owned-file 删除；
user scope 的 forget 不能删除只是在视图中可见的 system 记录。

有 pending operation 时，即使安装尚未创建安装记录就失败，`forget` 也会
引导先运行 `repair`。repair 会将 journal 与 rpmdb 协调；forget 不能丢弃
可能已经改变系统的 transaction 证据。

### adapter

管理组件 adapter：

```bash
anolisa adapter scan
anolisa adapter enable <component> [framework]
anolisa adapter disable <component> [framework]
anolisa adapter status [component]
```

对于 OpenClaw 插件，执行 `adapter enable` 即同意插件声明的能力。ANOLISA
仅在安装器 help 列出 `--accept-capabilities` 时添加该参数，dry-run 计划也
遵循相同规则。Capability consent 不授予 `--allow-unsafe-plugin-install`
权限；同意被拒绝时会单独诊断，不归为插件安全扫描拒绝。

对于 OpenCode，可用以下命令管理已安装的 Tokenless 插件：

```bash
anolisa adapter enable tokenless opencode
anolisa adapter status tokenless
anolisa adapter disable tokenless opencode
```

driver 从 PATH 查找 `opencode`，也可通过 `OPENCODE_BIN` 指定。它将 manifest 中的
`.js` 或 `.ts` 入口注册为 `plugins/<plugin_id>.<扩展名>`，配置目录依次取
`OPENCODE_CONFIG_DIR`、`XDG_CONFIG_HOME/opencode`、`~/.config/opencode`。
自定义目录必须是绝对路径。后续 status 和 disable 应使用相同的目录配置；若配置
发生变化，需恢复原环境后重试清理。本 driver 不管理项目级插件安装或 npm 插件。

enable 会接管指向同一插件源的已有符号链接，包括 Tokenless 独立安装脚本创建的
链接；相对链接展开后必须与记录的源路径一致。目录别名链接会被拒绝，以确保包卸载后
仍能清理。接管后 disable 可以删除该链接。同名文件、目录或指向不同目标的链接会被保留；
清理失败时保留 receipt，解决冲突后可以重试。同路径升级以原子操作替换入口。
若 enable 或 disable 中断，保持相同配置重试命令即可恢复未完成的变更。如果错误
提示保留了某个条目，请保留其恢复目录，解决公开路径上的冲突后重试；恢复也支持
被移走的目录。切回独立安装脚本前，请先完成 ANOLISA disable 和恢复操作；独立脚本不会
处理这些 journal。配置目录所在的文件系统必须支持原子交换和不覆盖的 rename，才能完成
替换和清理；不支持时操作会失败，仅解决路径冲突不能补足文件系统能力。如果原安装脚本使用了
`TOKENLESS_OPENCODE_CONFIG_DIR`，通过 ANOLISA 启用前需将
`OPENCODE_CONFIG_DIR` 设置为相同目录。

启用或禁用后需重启 OpenCode。status 检查链接和安装源；若启用中断或清理仍需重试，即使
当前链接匹配也会保留 `cleanup_failed`，需重试 enable 或完成 disable 才能解除。
运行时加载状态报告为
`unknown`；链接存在不代表运行中的 OpenCode 已加载插件。`--dry-run` 仅预览操作，
不修改插件文件或 receipt。

### logs 与 bug report

查看组件日志或生成诊断包：

```bash
anolisa logs <component>
anolisa logs <component> --limit 50
anolisa logs <component> --severity warn
anolisa bug
```

`--level` 是 `--severity` 的别名。

---

## 恢复行为

install、uninstall、update、adopt 和 repair 会先在所选 state root 写入
recovery intent，再执行 lifecycle 副作用。native package 操作采用
forward-only 策略：如果 dnf 可能已经提交而 ANOLISA record 尚未提交，journal
会保持 pending，`anolisa repair <component>` 会重新观察 rpmdb。owned-file
操作保留已校验的 backup，并在失败时按逆序补偿。`forget` 是原子的 record-only
状态更新，不执行 package/file 副作用，也不创建 recovery journal。

`upgrade` 仍是兼容性 orchestrator，不是 planner/journal consumer。它会拒绝
已有的 pending recovery，并在 transaction 失败后重新观察 rpmdb，但不会创建
per-component recovery journal。`upgrade` 若被进程中断，应先运行
`anolisa doctor`，处理报告的 component drift 后再执行其他 lifecycle 修改。

不要为了解除阻塞而直接删除 pending journal。先运行 `doctor` 确认其 scope
和 subject，再执行带完整 scope 的 `repair` 命令。malformed 或 ambiguous
journal 会有意保持 pending，等待人工检查。

---

## 全局选项

| 选项 | 说明 |
|------|------|
| `--install-mode user\|system` | 选择修改 scope |
| `--prefix <PATH>` | 覆盖所选 scope 的安装前缀 |
| `--dry-run` | 输出计划但不执行 |
| `--json` | 输出机器可读的 JSON |
| `-v, --verbose` | 增加详细程度 |
| `-q, --quiet` | 隐藏非错误输出 |
| `--no-color` | 禁用彩色输出 |
| `--version` | 显示 CLI 版本 |
| `--help` | 显示命令帮助 |

---

## 示例流程

```bash
curl -fsSL https://get.agentic-os.sh | bash
anolisa env
anolisa install cosh
anolisa install tokenless
anolisa adapter enable tokenless cosh
anolisa doctor
anolisa status
```

---

## 配置

system mode 从 `/etc/anolisa/repo.toml` 读取 backend 选择和 endpoint，
user mode 从 `~/.config/anolisa/repo.toml` 读取：

```toml
schema_version = 1
default_backend = "raw"

[backends.raw]
base_url = "https://repo.example.com/anolisa/v1/"
```

raw backend 每次执行都会重新拉取 distribution index。当前不会使用缓存的 index 作为回退，因此仓库不可达时命令会直接失败。
旧的 `cache_ttl_secs` 和 `offline_fallback` 字段仍可正常解析以保持向后兼容，但当前 raw backend 不会使用这些值。

RPM 仓库需要代理或私有 CA 时，可在 `/etc/yum.conf` 的 `[main]` 段直接配置
`proxy`、`proxy_username`、`proxy_password` 或 `sslcacert`；该文件不存在时
使用 `/etc/dnf/dnf.conf`。未明确配置时，ANOLISA 使用环境代理（`http_proxy`、
`https_proxy`、`all_proxy`、`no_proxy`）和系统信任的证书。

CLI 参数只影响当前执行的操作，不存在 `[install] mode` 配置。

---

## 参见

- [安装指南](../installation.md)
- [故障排查](../troubleshooting.md)

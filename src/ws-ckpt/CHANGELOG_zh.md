# 更新日志

[English](CHANGELOG.md)

## 0.5.0

### Added

- 统一 workspace 路径身份，init 时拒绝冲突的别名注册
- init/unregister 被中断的 workspace 在 daemon 重启后自动恢复
- 新增 cosh-ng 受管任务使用的 guarded rollback V2 协议与 evidence wire variants，cosh-ng 需要 ws-ckpt daemon 0.5.0 及以上

### Changed

- OpenClaw 工具白名单改由 OpenClaw config CLI 管理，要求 OpenClaw 2026.2.13 及以上 (#3221)
- `/etc/ws-ckpt/config.toml` 以 RPM `%config(noreplace)` 打包：升级保留管理员修改、新默认值落为 `.rpmnew`；卸载时修改过的配置保留为 `.rpmsave`，重装后需手动恢复 (#3070)

### Fixed

- 修复空 workspace 不生成快照的问题
- 修复 workspace 目录被外部替换后操作仍静默成功的问题，现在快速失败并提示 recover (#3059)
- 修复 `recover --all` 部分失败时仍返回成功的问题 (#3069)
- 修复 recover 失败时 `rpm -e` 连带清掉最后一份快照的问题 (#3069)
- 修复磁盘空间紧张时 checkpoint 失败的问题，自动回收已删除子卷 (#3053)
- 修复并发 init 同一 workspace 的竞态问题
- 修复 raw 安装布局下的 adapter 发现问题
- 修复 OpenClaw 插件安装时未校验 CLI 能力支持的问题 (#3116)

## 0.4.5

### Added

- 新增中英双语的 sidecar 部署指南 (#2965)

### Fixed

- 修复所有 loop 设备被占用时的间歇性 bootstrap 失败 (#2965)
- sidecar preStop 的 loop 清理改为仅处理 ws-ckpt 自身的设备 (#2965)

## 0.4.4

### Added

- 新增受保护的 checkpoint 协议，快照创建按存储身份围栏，崩溃后状态仍可校验

### Fixed

- 修复回滚被中断后重启丢失状态，无法判定的候选文件予以保留而非删除
- 修复 `config --global` 校验 daemon 是否实际加载所写配置 (#2813)
- 修复 sidecar 部署指南遗漏全局配置所需的 `/etc/ws-ckpt` 共享卷 (#2813)
- 修复错误提示覆盖容器化部署与 daemon 挂载命名空间场景 (#2807, #2814)

## 0.4.3

### Added

- 新增 k8s sidecar 容器化部署支持 (#2034)

### Fixed

- 修复 daemon 在持续 checkpoint 负载下内存无限增长，最终导致 OOM (#2554)
- 修复 loop device 后端在并发 IO 下 checkpoint 延迟，至多降低至原来的 1/5 (#2523)
- 修复 bootstrap 失败后遗留孤儿镜像和 loop device，并在启动失败时输出明确的错误信息而非静默退出 (#1956)
- 修复 workspace 路径已被挂载时 init 返回 EBUSY 无提示，改为拒绝并给出清晰的错误信息 (#1798)
- 补充 RPM 组件身份声明，使 anolisa-cli 可自动发现 ws-ckpt 适配器 (#2568)

## 0.4.2

### Added
- 新增 ops 日志写入的遥测门控 (#1509)

### Fixed
- 修复中断 init 后遗留的 `.pre-init-bak` 自动恢复 (#1601)

## 0.4.1

### Added

- 新增 rollback 后跳过自动 checkpoint (#1263)

### Fixed

- 修复 config 更新后的工作区同步 (#1263)
- 修复 crontab 条目中 ws-ckpt 的绝对路径处理 (#1263)
- 变更 rollback -n 偏移量，直接传递 numAncestors (#1263)

## 0.4.0

### Changed

- **不兼容** checkpoint `-i`/`--id` 参数更名为 `-s`/`--snapshot` 作为主参数；`-i` 保留为隐藏别名，未来版本可能移除 (#1064)

### Added

- 新增插件安装/卸载子命令 (#1005)
- 新增 component.toml 用于 anolisa-cli 适配器发现 (#1005)
- 新增 rollback 预览功能，支持 --preview 参数 (#1103)
- 新增每次 CLI 操作后的耗时显示 (#1075)
- 新增省略 --snapshot 时自动生成快照 ID (#1064)
- 新增 SLS 运维日志输出用于仪表盘指标 (#1059)
- 新增 diff 的可选 -t 参数，用于将快照与当前工作区对比 (#848)
- 新增按祖先数量 rollback 和快照 DAG 追踪 (#877)
- 新增基于 cron 的定时 checkpoint 快照 (#819)

### Fixed

- 修复 --snapshot/-s 作为主参数的处理及插件参数对齐 (#1103, #1064)
- 修复 SKILL.md 与实际 CLI/插件实现的同步 (#847)
- 修复 init 和 recover 对被替换的 workspace 符号链接的防护 (#860)
- 修复 init rsync 去除 --copy-unsafe-links (#873)

## 0.3.3

### Added

- 新增每工作区策略覆盖，支持 hermes/openclaw 插件 (#721)
- 新增 `/proc` cwd 占用者检测，用于 init 和 rollback (#684)
- 新增 Hermes 适配器运行脚本 (#617)

### Fixed

- 修复 rollback 中的写锁竞争和 cwd 检测死锁 (#721, #684)
- 修复非 UTF-8 路径和路径穿越快照 ID 的输入验证 (#695, #678)
- 修复 seccomp 架构选择、工作区注册并发和 RPM 打包问题 (#695, #684)

## 0.3.2

- 修复 openclaw 卸载时未从配置中移除工具白名单
- 修复父路径拒绝规则作为工作区级别规则应用于 skill 和 openclaw 插件

## 0.3.1

- 修复插件工作区配置注册和自动加载
- 拒绝将 hermes cwd 本身或其父路径作为工作区路径
- 修复插件工具优先使用显式 workspace 参数而非配置
- 修复 skill 删除需要 --force 参数
- 修复 daemon 工作区路径验证和 fswatch 文件描述符泄漏
- 移除未使用的 btrfs_ops.rs 模块

## 0.3.0

- 新增 openclaw 插件脚手架
- 新增 hermes 插件脚手架
- 将 ws-ckpt skill 改为 agent 无关，在调用时提示输入工作区
- 遵循 `make install` 契约用于 build-all 集成
- 修复 list 和 diff 子命令的缺陷
- 将 daemon 改为有状态

## 0.2.0

- 新增 auto_cleanup 功能及开关
- 统一通过 TOML 文件修改配置
- 新增全局 CLI 警告：当任意工作区快照数 >1000 或文件系统使用率 >90%
- 修复后端检测和 daemon 状态恢复逻辑
- 修复 daemon 重启后镜像大小配置不生效
- 移除过时的 fs_warn_threshold_percent 参数
- 修复 config.toml 作为示例文件分发

## 0.1.0

- 带 Unix Socket IPC 和 Bincode 二进制协议的 Daemon
- `init` / `checkpoint` / `rollback` / `delete` / `list` / `diff` / `cleanup` / `status` / `config` 命令
- 后台调度器：自动清理、健康检查、孤立恢复
- 多后端：btrfs-base / btrfs-loop / overlayfs 自动检测
- TOML 配置持久化及运行时热重载
- systemd 服务及 Alinux 4 RPM 打包

# 变更日志

[English](CHANGELOG.md)

本文件记录 ANOLISA Blaze 的所有重要变更。

格式基于 [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [未发布]

## [0.3.0] - 2026-07-22

### 新增

- 通用 `StorageProvider` trait，支持可插拔存储后端架构。
- `FileStorageProvider`：默认文件存储后端，适用于开发和标准部署。
- `[storage]` 配置段：`provider`、`pool_size`、`prefork`、`flush_interval` 字段，均有向后兼容的默认值。
- `GET /v1/health` 现返回 `storage_pool` 状态（ready/capacity/pending）。
- `BackendSpawner` trait 扩展 `restore`、`pause`、`resume`、`create_snapshot` 方法（默认返回不支持，为后续快照工作流预留接口）。

## [0.2.1] - 2026-07-21

### 变更

- **品牌重塑**：组件从 Anvil 更名为 Blaze。二进制：`blazed`，配置路径：`/etc/anolisa/blaze/`，状态目录：`/var/lib/blaze/`。
- Firecracker vCPU 配置现已校验上限（1–32）。

### 新增

- 组件已注册到项目清单（根 README、AGENTS.md、PR 模板）。
- VM 资源配置回退链已在 README 中说明。

## [0.2.0] - 2026-06-30

### 新增

- FirecrackerSpawner：支持 Firecracker microVM 后端，daemon 启动时自动探测并选择最强隔离。
- TCP 远程 API：可配置 `[listen].http_addr` 开启 TCP 监听（端口 14159），供平台远程调用。
- 优先级后端选择：`build_spawner()` 按 firecracker → linux-sandbox → mock 优先级自动选型。
- Storage section：`[storage].images_dir` 统一管理 vmlinux/rootfs 查找路径。
- 打包骨架：`dist/anvil.service`（systemd unit）+ `anvil.spec`（RPM）+ `tmpfiles-anvil.conf`。
- `[backends]` 配置段，直接映射后端二进制路径。

## [0.1.3] - 2026-06-24

### 变更

- sandbox 进程现在运行在完整 namespace 隔离中（PID、网络、文件系统）。

## [0.1.2] - 2026-06-22

### 新增

- daemon 现在管理 sandbox 进程生命周期：创建时自动启动，销毁时自动终止。
- backend 二进制不可用时优雅降级（便于开发环境使用）。

## [0.1.1] - 2026-06-20

### 新增

- Policy 校验在 sandbox 启动前拒绝不安全的配置。
- 与 `osbase sandbox uninstall` 安全协调（防止移除正在使用的 backend）。

## [0.1.0] - 2026-06-18

ANOLISA Anvil 首个骨架版本。

### 新增

- 通过 HTTP API 创建、列出、查看、checkpoint（仅状态转换）、reset、销毁 sandbox。
- 策略驱动的 backend 选型：指定 workload class 即可自动匹配合适的 sandbox 类型。
- Warm pool：预创建 sandbox 随时分配，可配置 min/target/max 容量。
- 模板共享：多个 sandbox 共用一份 base 内存镜像，降低单实例内存开销。
- Prometheus metrics 端点，供监控系统采集。

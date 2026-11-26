# 贡献指南

[English](CONTRIBUTING.md)

## 开发环境

| 要求 | 版本 |
|------|------|
| Rust toolchain | stable（`rust-toolchain.toml` 管理） |
| Rust 最低版本 | 1.88 |
| 组件 | rustfmt + clippy |
| 支持平台 | Linux（完整功能）；macOS（功能受限） |

```bash
cd src/cosh-ng
rustup show   # 确认工具链已就绪
```

## 构建

```bash
# 完整构建（所有 workspace crate）
cargo build --workspace

# 发布构建
cargo build --workspace --release

# 单独构建某个二进制
cargo build --bin cosh-cli
cargo build --bin cosh-core
cargo build --bin cosh-shell
```

### RPM 构建 Profile

在仓库根运行 `./scripts/rpm-build.sh cosh-ng` 使用默认 RPM profile。
`COSH_RPM_PROFILE=alinux2 ./scripts/rpm-build.sh cosh-ng` 显式选择
Alibaba Cloud Linux 2.1903 profile，发行后缀默认是 `.alinux2`；
`DIST_TAG` 可覆盖后缀，但不会改变 profile。旧系统 profile 不安装 Gateway
服务单元，也不声明经过 ALinux4 资格验证的登录能力。运行二进制和只读升级、
卸载守卫保留，但不授予受管登录 Shell 修改能力，也不代表 systemd 219 上的
Gateway 服务隔离已经验收。
已安装的 Gateway 服务单元仍存在时，不能直接替换为不含服务的 profile；
需要先显式卸载原包，由现有卸载守卫停止实例后移除单元。

两个 profile 共用 spec、源码归档和生命周期 backend。spec 在自身 `target`
目录构建，并从同一目录安装，即使调用者导出了其他 `CARGO_TARGET_DIR`。
构建环境仍需提供声明的依赖；脚本不会安装新 Rust 工具链。在 `src/cosh-ng`
运行 `make test-packaging`，在仓库根运行 `bash tests/test-cosh-rpm-profile.sh`
做 fixture 检查；宣称目标交付 profile 受支持之前，仍需验证干净 SRPM 重建和
真实 RPM 安装、升级、卸载事务。

此源码/RPM profile 不改变官方预编译归档的 glibc 兼容 profile。

## 验证

按改动范围选择检查。

```bash
# 普通代码改动，格式化并运行最接近的测试
cargo fmt --all -- --check
cargo test --locked -p cosh-platform test_detect  # 示例

# 修改 public API 或 rustdoc 时
cargo doc --workspace --no-deps
```

纯文档改动只检查链接、格式、命令和中英文一致性，无需运行 Rust 测试。
针对性 Clippy、integration test 或 Shell 布局审计仅在与改动行为相关时添加。

全量本地门禁和持久 ECS 验证仅用于较大或跨模块代码改动，并且当前任务需要明确要求。
其余广泛回归覆盖交给 CI。

```bash
scripts/run-test-gates.sh all    # 完整确定性门禁
cargo build --workspace --release --locked
crates/cosh-shell/scripts/check-layout.sh
```

代码所有权和测试目标选择见[开发者入门指南](../../contribute/zh/cosh-ng/getting-started.md)。

## 工作空间结构

```
cosh-ng/
├── Cargo.toml              # workspace 配置
├── rust-toolchain.toml     # stable + rustfmt + clippy
└── crates/
    ├── cosh-types/         # 纯类型，零副作用
    ├── cosh-platform/      # 平台抽象（发行版检测、后端路由）
    ├── cosh-cli/           # CLI 入口
    ├── cosh-core/          # Agent 核心
    ├── cosh-shell/         # 交互终端
    ├── cosh-gateway-contracts/ # 无副作用的 Gateway contract
    └── cosh-gateway/       # Gateway control plane library 基础
```

## 依赖管理

- 所有依赖版本在 `[workspace.dependencies]` 统一声明
- 子 crate 通过 `dep = { workspace = true }` 引用
- 添加新依赖前检查是否已存在等价 crate
- 不允许未经讨论升级主版本号

## 代码规范

### 模块组织

使用 Rust 2018+ 推荐的文件布局，**不使用 `mod.rs`**。

```
# 正确
src/extension.rs        # 父模块
src/extension/          # 子模块目录
    config.rs
    manager.rs

# 错误示例
src/extension/mod.rs
```

### 错误处理

| 场景 | 方式 |
|------|------|
| 库 crate | `thiserror` 枚举 |
| 二进制 | `anyhow::Result` |
| 不可达路径 | `unreachable!()` + 注释 |
| 禁止 | `unwrap()` / `expect()` / `panic!()` |

### 注释

- `///` 用于所有 pub 项
- `//` 仅解释 *为什么*，不重复类型签名
- 首行为独立摘要，祈使句或名词短语
- 不允许没有负责人和背景的 `TODO`，也不要保留注释掉的旧代码

### Clippy

- 默认 deny 所有 warnings
- 确需忽略时使用最窄范围的 `#[allow(clippy::xxx)]` + 注释说明

## 提交规范

提交格式为 `type(cosh-ng): [crate_scope] imperative description`。

- 类型可选 feat、fix、refactor、docs、test、ci 或 chore
- scope 使用 `cosh-ng`
- crate scope 使用 `[core]`、`[shell]`、`[cli,platform]` 或其他精确列表
- 50 字符内，英文，祈使语气，首字母小写，无句号
- 需要 `Signed-off-by` trailer

```bash
git commit -s -m 'feat(cosh-ng): [core] add hook registry list'
```

## PR 流程

1. 从最新 main 分支切出特性分支
2. 遵循 `feature/cosh-ng/<short-desc>` 分支命名
3. 确保所有适用检查通过后推送
4. PR 标题遵循 commit message 格式
5. 填写 PR 模板中的适用章节，包括风险、验证、文档和回滚说明

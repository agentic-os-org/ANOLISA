# 新增发行版适配

[English](../../en/cosh-ng/adding-distros.md)

## 概述

cosh-ng 通过 `Distro` 和 `PkgManager` 抽象操作系统差异。新增一级发行版支持前，
先检查它的 `ID_LIKE` 是否已可映射到 DNF、Apt 或 Zypper 家族。兼容派生版需要
补充测试和文档，但通常不需要新的枚举变体或后端。

## 步骤

### 1. 确定是否需要一级变体

Linux 检测读取 `/etc/os-release`；仅当该文件不存在时，才回退到
`/usr/lib/os-release`。检测会先匹配标准化后的 `ID`，再从左到右扫描以空白
分隔的 `ID_LIKE` 值。

例如 Rocky Linux（`ID=rocky ID_LIKE="rhel fedora"`）会识别为
`Distro::Compatible`。包管理家族为 DNF，但 `id_str()` 和 JSON 输出仍保留
`rocky`。只有发行版需要兼容家族无法表达的独立行为时，才添加一级变体。

### 2. 添加 Distro 枚举变体

在 `crates/cosh-platform/src/detect.rs` 中添加变体。

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Distro {
    // ...existing...
    MyDistro { version: String },   // 新增
}
```

### 3. 实现检测逻辑

在 `detect_from_content()` 的 match 分支中添加 ID 映射。

```rust
match id.as_deref() {
    // ...existing...
    Some("mydistro") => Distro::MyDistro { version },
    // ...
}
```

所有值都会标准化为小写。直接 `ID` 匹配应保持在 `ID_LIKE` 回退之前，使显式支持的
发行版继续使用自己的变体。

### 4. 实现辅助方法

```rust
impl Distro {
    pub fn id_str(&self) -> &str {
        match self {
            // ...existing...
            Distro::MyDistro { .. } => "mydistro",
        }
    }

    pub fn display_name(&self) -> String {
        match self {
            // ...existing...
            Distro::MyDistro { version } => format!("MyDistro {}", version),
        }
    }

    pub fn pkg_manager(&self) -> PkgManager {
        match self {
            // ...existing...
            Distro::MyDistro { .. } => PkgManager::Dnf, // 根据实际情况选择
        }
    }
}
```

如果新发行版使用的包管理器不在现有 `PkgManager` 枚举中，需先扩展该枚举。

### 5. 添加包管理器后端（如需新增）

如果需要新的 `PkgManager` 变体，在 `crates/cosh-platform/src/pkg.rs` 中添加对应的命令
构建函数。

```rust
// 新增 PkgManager 变体
pub enum PkgManager {
    // ...existing...
    Pacman,
}

// 在 pkg_install / pkg_remove / pkg_search / pkg_list 中添加路由分支
PkgManager::Pacman => ("pacman", vec!["-S", "--noconfirm", package]),
```

### 6. 添加单元测试

在 `detect.rs` 的 `#[cfg(test)]` 模块中添加测试。

```rust
#[test]
fn test_detect_mydistro() {
    let content = "NAME=\"My Distro\"\nVERSION_ID=\"1.0\"\nID=mydistro\n";
    let distro = Distro::detect_from_content(content);
    assert_eq!(distro, Distro::MyDistro { version: "1.0".into() });
    assert_eq!(distro.pkg_manager(), PkgManager::Dnf);
}
```

对兼容派生版，要覆盖它的真实 `ID`、带引号和不带引号的 `ID_LIKE`、第一个
可识别家族，以及 JSON 中保留的发行版标识。

### 7. 运行针对性测试

```bash
cd src/cosh-ng

# 运行检测相关测试
cargo test --locked -p cosh-platform test_detect

# 运行完整测试套件
cargo test --locked -p cosh-platform

# 运行 CLI 集成测试（确保新路由不破坏 JSON 信封）
cargo test --locked -p cosh-cli
```

## 当前支持矩阵

| 发行版 ID | Distro 变体 | PkgManager | 备注 |
|-----------|-------------|------------|------|
| `alinux` | `Alinux` | Dnf | 阿里云原生 Linux |
| `centos` | `CentOS` | Dnf | |
| `fedora` | `Fedora` | Dnf | |
| `ubuntu` | `Ubuntu` | Apt | |
| `debian` | `Debian` | Apt | |
| `opensuse-leap` / `opensuse-tumbleweed` / `sles` | `OpenSUSE` | Zypper | 三个 ID 映射到同一变体 |
| `ID_LIKE=alinux/centos/fedora/rhel` 的未列出 ID | `Compatible` | Dnf | 保留真实 `ID`；例如 `rocky` |
| `ID_LIKE=debian/ubuntu` 的未列出 ID | `Compatible` | Apt | 保留真实 `ID` |
| `ID_LIKE=opensuse/suse` 的未列出 ID | `Compatible` | Zypper | 保留真实 `ID` |

## 设计约束

| 规则 | 说明 |
|------|------|
| ID 小写 | `detect_from_content()` 对 ID 做 `to_lowercase()` |
| 兼容回退 | 以空白分隔的 `ID_LIKE` 中，第一个可识别家族决定包管理器 |
| Unknown 兜底 | 直接 ID 和兼容家族均未匹配时归入 `Unknown(String)`，包操作返回 `UnsupportedDistro` |
| 多 ID 合并 | 多个 ID 可映射同一 Distro 变体（如 opensuse 系列） |
| 包管理器解耦 | `PkgManager` 与 `Distro` 是独立枚举，通过 `pkg_manager()` 映射 |
| 文件优先级 | `/etc/os-release` 优先；仅当其不存在时才使用 `/usr/lib/os-release` |

## 完整检查清单

- [ ] 确定 `ID_LIKE` 兼容是否已足够
- [ ] 仅在需要独立行为时添加 `Distro` 变体和直接 ID 匹配
- [ ] `id_str()` 保留正确的发行版标识
- [ ] `display_name()` 返回可读名称
- [ ] `pkg_manager()` 映射到预期家族
- [ ] `Display` trait（通过 `display_name()`）正确格式化
- [ ] 按需覆盖直接 ID、`ID_LIKE`、引号、文件回退和未知输入
- [ ] 如需新 `PkgManager`，在 `pkg.rs` 所有操作中添加路由
- [ ] 更新[支持的发行版](../../../user-guide/zh/user-entrypoint/cosh-ng/supported-distros.md)

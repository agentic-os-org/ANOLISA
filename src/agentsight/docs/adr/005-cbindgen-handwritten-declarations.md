# ADR-005: cbindgen 手写声明 workaround

## 状态
已采纳（临时方案，待 cbindgen 修复后移除）

## 背景
AgentSight 使用 Rust 2024 edition，FFI 导出函数使用 `#[unsafe(no_mangle)]` 属性。cbindgen 0.27 无法识别这个新语法，会静默跳过所有导出函数，导致生成的 C header 中缺少函数声明。

## 决策
在 `cbindgen.toml` 的 `after_includes` 块中**手写所有 FFI 函数的 C 声明**，并在 `build.rs` 中实现 drift guard 脚本来检测手写声明与 Rust 代码之间的函数名不一致。

## 理由
- 等待 cbindgen 上游修复的周期不确定，项目需要立即可用的 C header
- 手写声明虽然有维护负担，但配合 drift guard 可以至少保证函数名不漂移
- 替代方案（降级到 Rust 2021 edition 或 fork cbindgen）的代价更高

## 已知局限
- drift guard **只检查函数名**，不检查参数类型、数量或返回值
- 修改 FFI 函数签名（不改名）时 drift guard 不会报错，必须手动同步 C 声明
- `cbindgen.toml` 中的 `item_types = ["structs"]` 是独立的 workaround，防止 cbindgen 把 `pub const` 生成为重复的 `#define`

## 移除条件
当 cbindgen 发布支持 `#[unsafe(no_mangle)]` 的版本后，应当：
1. 移除 `after_includes` 手写声明
2. 移除 `item_types = ["structs"]` 限制
3. 移除 `build.rs` 中的 `check_ffi_header_drift` 函数

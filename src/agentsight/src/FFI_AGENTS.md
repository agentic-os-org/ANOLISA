# FFI Layer Rules

> 适用于 `src/ffi.rs` 及所有 `extern "C"` 导出函数。

1. 新增 `extern "C"` 函数必须同时在 `cbindgen.toml` 的 `after_includes` 中添加 C 声明（cbindgen 0.27 不识别 `#[unsafe(no_mangle)]`）
2. FFI 类型必须标注 `#[repr(C)]`
3. 禁止 panic 穿越 FFI 边界 — 所有入口函数用 `std::panic::catch_unwind` 包裹
4. 错误通过 thread-local `LAST_ERROR` 返回，调用方用 `agentsight_last_error()` 读取
5. 指针参数必须在函数入口做 null check，返回错误码而非 UB
6. 修改函数签名后必须确认 `build.rs` drift guard 通过（注意：drift guard 只检查函数名，不检查签名）
7. `AgentsightLLMData` 字段变更（新增/删除/重排）会改变 C ABI layout — cbindgen 自动拾取 `#[repr(C)]` struct 的所有字段，但变更后必须确认下游 C 消费方已同步更新

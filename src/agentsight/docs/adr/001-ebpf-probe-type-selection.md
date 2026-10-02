# ADR-001: eBPF 探针类型选型

## 状态
已采纳

## 背景
AgentSight 需要在不修改 Agent 代码的前提下捕获 LLM API 调用。Linux eBPF 提供了三种主要的用户态追踪方式：uprobe、kprobe 和 tracepoint。需要选择最适合捕获 SSL/TLS 明文流量的方式。

## 决策
- SSL 流量捕获使用 **uprobe**，hook 用户态 `SSL_read`/`SSL_write` 函数
- 进程生命周期追踪使用 **tracepoint**（`sched_process_exec`/`sched_process_exit`）
- 文件写入捕获使用 **fentry**（`vfs_write`），性能优于 kprobe

## 理由
- uprobe 可以直接读取 SSL 解密后的明文 buffer，避免在内核层面处理密钥和解密逻辑
- kprobe hook 内核 SSL 实现不可行——Linux 内核没有统一的 SSL 层，加密在用户态库（OpenSSL/BoringSSL/GnuTLS）中完成
- tracepoint 比 kprobe 更稳定，不受内核函数重命名影响
- fentry（BPF trampoline）比 kprobe 性能更好，开销约为 kprobe 的 1/3

## 后果
- 需要为每种 SSL 库（OpenSSL、BoringSSL、GnuTLS）分别查找符号并 attach uprobe
- 新增 SSL 库支持（如 Go crypto/tls）需要额外的 uprobe 实现
- tracepoint 接口稳定但可获取的信息受限于内核暴露的字段

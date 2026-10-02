# ADR-003: C FFI eventfd + read 模型

## 状态
已采纳

## 背景
AgentSight 需要以 C 动态库（cdylib）形式被外部程序集成。外部调用方需要一种方式知道"有新事件了"并读取事件数据。备选方案包括：回调函数（callback）、channel 通信、eventfd + 主动 read。

## 决策
采用 **eventfd 通知 + 主动 read** 模型：
1. `agentsight_get_eventfd()` 返回一个文件描述符
2. 调用方 `epoll`/`poll` 等待 fd 可读
3. fd 就绪时调用 `agentsight_read()` 读取事件，通过回调函数逐个交付

## 理由
- eventfd 是 Linux 标准的跨线程/跨进程通知机制，C 调用方无需理解 Rust channel
- 调用方可以将 eventfd 集成到自己的事件循环（epoll/libuv/libevent），不需要额外线程
- callback 模式虽然简单，但从 BPF ring buffer 线程直接回调 C 代码会引入锁竞争和栈溢出风险
- 纯 callback 模式无法让调用方控制读取节奏（背压），eventfd + read 天然支持

## 后果
- 调用方需要理解 eventfd 的语义和 epoll 用法
- `agentsight_read()` 是同步阻塞的（可通过 `AGENTSIGHT_READ_BLOCK` flag 控制），调用方需要在合适的线程调用
- FFI 边界需要 `catch_unwind` 防止 panic 穿透

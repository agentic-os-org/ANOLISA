# ADR-002: 共享 Ring Buffer 架构

## 状态
已采纳

## 背景
AgentSight 有多个 eBPF 探针（sslsniff、proctrace、procmon、filewatch、filewrite、udpdns、tcpsniff），每个探针都需要将捕获的事件传递给用户态程序。需要决定是每个探针使用独立的 ring buffer，还是多个探针共享同一个。

## 决策
每个探针使用**独立的 ring buffer**，用户态通过 `ProbesPoller` 统一 poll 所有 ring buffer 的 fd。

## 理由
- 独立 ring buffer 避免了不同探针事件类型的序列化/反序列化冲突
- 每个 ring buffer 可以独立调整大小，高流量的 sslsniff 需要更大的 buffer，低流量的 procmon 可以更小
- `epoll` 统一 poll 多个 fd 的开销极低，不会成为瓶颈
- 共享 ring buffer 需要在 BPF 侧做类型标记和长度前缀，增加 BPF 程序复杂度

## 后果
- 新增探针时需要在 `Probes` 结构体中添加对应的 ring buffer 字段
- `ProbesPoller` 需要处理所有探针类型的事件分发
- 内存占用是所有 ring buffer 大小之和，但可以通过配置单独调整

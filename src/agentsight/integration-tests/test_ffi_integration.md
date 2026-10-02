# C FFI API 集成测试

> 前置条件见 [RULES.md](RULES.md)（环境变量、部署流程、通用规则）

## 测试目标

通过 C FFI API 启动 agentsight，能采集到 LLM/HTTPS 事件数据。

1. FFI 全流程（config → new → start → read → stop → free）不 crash
2. `agentsight_read()` LLM 回调触发且字段非空（provider、model、request_url）
3. `agentsight_read()` 能抓取 Hermes 和 OpenClaw 的 LLM 请求，字段非空且可按 comm 区分来源进程

## 运行条件

- root 权限（eBPF）
- Linux kernel >= 5.8 with BTF
- gcc 可用
- 网络可达外部域名

## 测试步骤

1. 编译 example：
   ```bash
   cargo build --release
   gcc -o /tmp/agentsight_example examples/agentsight_example.c \
       -I./include -L./target/release -lagentsight -lpthread -ldl -lm
   ```
2. 运行：
   ```bash
   sudo LD_LIBRARY_PATH=./target/release /tmp/agentsight_example
   ```
3. 运行期间分别启动 Hermes 和 OpenClaw agent 进程，使其各自向 `dashscope.aliyuncs.com` 发起 LLM API 调用
4. 等待 30s 或 Ctrl+C 停止

## 判定

- **PASS**: stdout 出现 `[LLM]` 行，字段非空（provider/model 等），可通过 comm 区分 Hermes 与 OpenClaw
- **SKIP**: 无事件输出（无匹配 agent 进程）
- **FAIL**: 全流程 crash 或回调字段为空
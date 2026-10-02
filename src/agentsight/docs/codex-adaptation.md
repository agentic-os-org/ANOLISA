# Codex CLI 适配指南

AgentSight 通过 eBPF uprobe 捕获 Codex CLI 的 TLS 明文流量。Codex 通过 reqwest → native-tls → openssl-sys 静态链接 OpenSSL 3.x，并使用 `SSL_write_ex` / `SSL_read_ex` 作为加密入口（rustls + aws-lc-rs 仅用于 WebSocket TLS，不走 `SSL_write_ex`）。当二进制保留符号时按符号挂载即可；剥离了符号时需要通过内置 offset 表查表。

## 三级回退

| Tier | 方式 | 适用场景 |
|------|------|---------|
| 1 | 按符号名挂 uprobe | 二进制保留 `.dynsym` / `.symtab` 时（自编译默认） |
| 2 | 字节码 pattern 扫描 | 已收录指纹的常见 BoringSSL / OpenSSL 构建 |
| 3 | 内置 offset 表查表 | 已收录版本的 stripped binary |

未命中三级则放弃挂载并打印 warn 日志。

## 内置版本

写入 `src/agentsight/agentsight.json -> codex_offsets.entries` 即可。当前内置：

| codex 版本 | 状态 |
|------------|------|
| 0.141.0 | Tier 3 `SSL_*_ex` |
| 0.137.0 | Tier 3 `SSL_*_ex` |

## 为新版本提取偏移

### 前置：拿到一份**带符号**的同版本二进制

线上 codex release 是 stripped binary，nm 拿不到符号。你需要先准备一份**带符号**副本，再用它跑提取脚本。三种方式按优先级：

**方式 A：官方 symbols 包（0.140 及以上推荐）**

0.140 起 codex GitHub release 附带 `codex-symbols-x86_64-unknown-linux-musl.tar.gz`。下载并解压即可：

```bash
VERSION=0.141.0  # 替换成你的版本
curl -L -o codex-symbols.tar.gz \
  "https://github.com/openai/codex/releases/download/rust-v${VERSION}/codex-symbols-x86_64-unknown-linux-musl.tar.gz"
tar xzf codex-symbols.tar.gz
# 解压出 codex-symbols-x86_64-unknown-linux-musl/codex.debug，nm 可识别符号
```

**方式 B：自行编译，不要 strip（0.139 及以下，或希望可复现）**

0.139 及以下 release 没有 symbols 包；此时直接 clone 源码、checkout 对应 tag、编译时不要 strip：

```bash
git clone https://github.com/openai/codex.git
cd codex
git checkout rust-v0.137.0       # 替换成你的版本

# 编译前确认 codex-rs/Cargo.toml 里 [profile.release] 没有以下任一行：
#   strip = true
#   strip = "symbols"
# 若有，注释掉或改成 strip = "none"
cargo build --release -p codex --target x86_64-unknown-linux-musl
# 产物：target/x86_64-unknown-linux-musl/release/codex
nm --defined-only target/x86_64-unknown-linux-musl/release/codex | grep SSL_  # 验证有符号
```

> Tip：debug build (`cargo build` 不加 `--release`) 也带全套符号，但函数地址 / 二进制布局跟 release 不一致，不能直接用于线上 stripped binary 的偏移提取。请用 release 构建。

**方式 C：本机已经在跑带符号的版本**

如果是从源码 `cargo install --path ...` 安装的，本机 codex 一般就带符号，直接用即可。

### 另外还需要

- 一份**目标**（运行中那份） stripped binary —— 用于取指纹
- Python 3 + `nm`、`readelf`、`sha256sum`

### 步骤

1. 取 stripped binary 的指纹：

```bash
stat --printf='%s\n' /path/to/codex-stripped
head -c 65536 /path/to/codex-stripped | sha256sum
readelf -n /path/to/codex-stripped | grep "Build ID"   # 如有
```

2. 在**带符号**那份上跑提取脚本：

```bash
python3 src/agentsight/scripts/extract-codex-offsets.py /path/to/codex-with-symbols
```

脚本会优先匹配 `SSL_write_ex` / `SSL_read_ex` / `SSL_do_handshake`，若 `_ex` 变体不存在则退到 `SSL_write` / `SSL_read`，并据此生成 `write_is_ex` / `read_is_ex` 标志。输出形如：

```json
{
  "codex_version": "0.141.0",
  "fingerprint": {
    "file_size": 276579568,
    "head_64k_sha256": "f015ddd2a687c1fc0b3ce70d898c0a68eeab88ad0040e79b0fe49a8545ff52a9"
  },
  "offsets": {
    "ssl_write": 210691872,
    "ssl_read": 210691280,
    "ssl_do_handshake": 210689600,
    "write_is_ex": true,
    "read_is_ex": true
  }
}
```

3. 用**步骤 1** 拿到的 stripped binary 指纹覆盖 `fingerprint`，然后把整条 entry 追加到 `agentsight.json` 的 `codex_offsets.entries` 数组里。如果有 BuildID，也加进 `fingerprint`，AgentSight 会优先按 BuildID 匹配。

4. 验证：重新启动 agentsight 后日志会出现：

```
[attach_process] pid=<PID>: codex offset table matched for /proc/<PID>/root/.../codex
  (write=0x<...>, read=0x<...>, handshake=0x<...>)
```

## 手动提取（无脚本时）

```bash
# 优先用 _ex 变体（OpenSSL 3.x 默认导出）
nm --defined-only /path/to/codex-with-symbols | grep -E ' (SSL_write_ex|SSL_read_ex|SSL_do_handshake)$'

# 如果没有 _ex，退到普通 SSL_write / SSL_read
nm --defined-only /path/to/codex-with-symbols | grep -E ' (SSL_write|SSL_read|SSL_do_handshake)$'
```

`nm` 输出第一列就是文件偏移（十六进制），直接转十进制填入 `offsets`。

## 完全没有带符号副本时的后备路线

极端情况下既拿不到 symbols 包也无法重新编译（比如目标版本源码已不可获取），可以用 `objdump` 加 `bpftrace` / tracefs uprobe 的命中数比对来人工锁定地址。已收录的 0.137 偏移就是这样拿到的。大致思路：

1. `objdump -d` 找具备 OpenSSL `SSL_write_ex` 序言特征的候选地址，可借助 `strings -t x` 反查 OpenSSL 内部字符串引用收窄范围。
2. 用 `bpftrace -e 'uprobe:/path/to/codex:0x<CANDIDATE> { @ = count(); }'` 挂到候选 offset，同时跑一次真实 codex 请求；命中数应满足 `SSL_do_handshake = 1`、`SSL_write_ex ≈ 1`、`SSL_read_ex` 与 SSE chunk 数同阶。
3. 用 uretprobe 抓 `$retval` 判断是否 `_ex` 变体（返回值仅 0/1，真实长度由第 4 参数 `*written` 回写），据此设置 `write_is_ex` / `read_is_ex`。

这条路线工作量大且容易出错，建议优先走前面三种带符号副本的方式。

## 误判记录：为什么曾误认为 aws-lc

早期分析将 codex 的 TLS 库误判为 aws-lc（BoringSSL 兼容），原因如下：

1. **依赖树误读**：Cargo.toml 声明了 `rustls` + `aws_lc_rs` feature，分析者假设所有 TLS 流量都走 rustls/aws-lc-rs。实际上 reqwest 用的是 `default-tls`（→ native-tls → openssl-sys），rustls 仅用于 tokio-tungstenite 的 WebSocket TLS。
2. **API 名称歧义**：`SSL_write_ex` 在 OpenSSL 和 aws-lc 中都存在（aws-lc 是 BoringSSL fork，实现了相同的 SSL C API），stripped binary 无法用 `nm` 区分。
3. **缺少 symbols 包**：0.139 及以下没有官方 symbols 包，无法做符号级验证。直到下载了 0.141.0 的 symbols 包，`nm` 才看到 `SSL_write_ex` 周围全是 `ossl_*` 前缀的函数（2705 个），而 `aws_lc_*` 前缀的函数（5 个）全是 crypto 原语（AES、HKDF 等），没有 SSL API。
4. **Runloop 场景误导**：Runloop 场景下 codex 连本地代理走明文，Node.js 代理走 OpenSSL，这让分析者更确信 codex 不走 OpenSSL——但实际原因是 Runloop 把 codex 配置为连本地代理，不是 codex 本身不支持 OpenSSL。

验证结论：0.50.0 / 0.137 / 0.141 三个版本均通过 `strings` 确认内嵌 `OpenSSL 3.5.x`，REST API 全程走 OpenSSL，AgentSight 挂 `SSL_write_ex` 可抓到全部 LLM 流量。

## 符号与 ABI

### 为什么 codex 用 `_ex` 变体

Codex 静态链接 OpenSSL 3.x（通过 reqwest → native-tls → openssl-sys）。OpenSSL 3.0+ 引入了 `SSL_write_ex` / `SSL_read_ex` 作为推荐 API，返回 0/1 表示成功/失败，字节数通过出参回写。虽然 OpenSSL 仍导出传统 `SSL_write` / `SSL_read`，但 reqwest / native-tls 默认调用 `_ex` 变体。经实测（带符号二进制 `nm` 验证），`SSL_write_ex` 地址与 offset 表完全吻合，且 `SSL_write` / `SSL_read` 也存在于同一段地址空间。

### `_ex` 与普通 `SSL_write` 的 ABI 差异

| | `SSL_write` (OpenSSL ≤2.x) | `SSL_write_ex` (OpenSSL 3.x) |
|---|---|---|
| 返回值 | 写入字节数（≥0 成功，<0 失败） | 0/1（1=成功，0=失败） |
| 字节数来源 | 返回值 `rax` | 第 4 参数出参 `*written`（`rcx` 指向的内存） |
| BPF 读取方式 | `kretprobe` 读 `rax` | `kretprobe` 读 `*written` 指针指向的内存 |

`SSL_read_ex` 同理：返回 0/1，真实字节数在 `*readbytes` 出参里。

### BPF 程序路由

offset 表的 `write_is_ex` / `read_is_ex` 标志决定 `attach_static_ssl_by_offset` 挂载哪组 BPF 程序：

- `write_is_ex = true` → 挂 `probe_SSL_write_ex_enter`（entry 保存 `rcx` = `*written` 指针）+ `probe_SSL_write_ex_exit`（return 时读 `*written` 得到字节数）
- `write_is_ex = false` → 挂 `probe_SSL_rw_enter`（entry 保存 `rsi` = buf 指针、`rdx` = len）+ `SSL_exit`（return 时读 `rax` 得到字节数）

如果 `write_is_ex` 标志设错（比如把 `_ex` 函数当成普通 `SSL_write` 挂），BPF 会从返回值 `rax` 读字节数——但 `_ex` 的返回值只有 0/1，导致每次事件只上报 1 字节，HTTP parser 拿到的是垃圾数据。

### 符号总表

| 符号 | 角色 | 说明 |
|------|------|------|
| `SSL_write` / `SSL_write_ex` | 写端 | 应用明文进入加密管线入口 |
| `SSL_read` / `SSL_read_ex` | 读端 | 解密明文交给应用 |
| `SSL_do_handshake` | 握手 | 标记 TLS 握手完成 |

offset 表里 `write_is_ex` / `read_is_ex` 不能省——它决定了 BPF 从哪个位置读取真实字节数。

## PR 模板

新增版本偏移请附：

- [ ] codex 版本号
- [ ] stripped binary 的 `file_size` + `head_64k_sha256`（必须）+ BuildID（如有）
- [ ] 提取方式（脚本 / 手动）
- [ ] 验证截图（agentsight 启动日志 `codex offset table matched`）

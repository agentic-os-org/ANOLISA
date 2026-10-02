# 资产验证

[English](../../../en/agent-security/agent-sec-core/asset-verification.md)

资产验证用于检查 Skill 目录的分发完整性。它会验证 GPG 签名的 manifest，检查每个
manifest 条目的 SHA-256 摘要，并拒绝 manifest 未覆盖的额外非隐藏普通文件。

该命令验证发布或部署签名。它不同于 [Skill Ledger](skill-ledger.md)：后者使用 Ed25519
签名维护本地运行时历史。

## 安装

```bash
# 首选：以 system mode 安装标准 ANOLISA raw 组件
sudo anolisa --install-mode system install sec-core

# 备选：已配置 YUM 源的 Alinux 系统
sudo yum install anolisa agent-sec-core
sudo anolisa --install-mode system adopt sec-core

# 源码构建（仅开发者）
./scripts/build-all.sh --component sec-core
```

资产验证要求 GnuPG 2.0 或更高版本。组件包中包含 verifier 配置和受信公钥。

## 验证 Skill

不带参数时执行批量发现；也可以绕过发现逻辑，显式验证单个 Skill。

```bash
# 扫描默认安装根目录
agent-sec-cli verify

# 只验证一个 Skill 目录
agent-sec-cli verify --skill /path/to/skill
```

每个候选 Skill 必须包含：

| 路径 | 用途 |
|------|------|
| `.skill-meta/Manifest.json` | 记录每个已签名文件的预期 SHA-256 摘要 |
| `.skill-meta/.skill.sig` | `Manifest.json` 的 GPG 分离签名 |

verifier 从包内 `agent_sec_cli/asset_verify/trusted-keys/` 目录加载受信 `.asc` 公钥。
manifest 或签名缺失、签名不受信或无效、manifest 条目缺失或被修改，以及出现额外未签名的
普通文件，都会使该候选验证失败。

## 默认发现根目录

`agent-sec-cli verify` 读取包内 `asset_verify/config.conf`。默认配置包含两个可选发现根目录：

| 安装拓扑 | 发现根目录 |
|----------|------------|
| RPM | `/usr/share/anolisa/skills` |
| 标准 ANOLISA raw 包 | `/usr/local/share/anolisa/skills` |

每个直接、非隐藏的子目录都是候选 Skill。发现过程遵循以下规则：

- 缺失的根目录会被静默跳过。
- 空根目录，或不含直接、可见子目录的根目录，不会贡献候选。
- 解析到同一 canonical path 的重复根目录只扫描一次。
- 已存在但不是目录的根路径，或无法枚举的根目录，属于操作错误。
- 无法读取的候选 Skill 与签名或哈希无效的候选一样，属于验证失败。

两个默认根目录是固定的包数据，不会从任意安装 prefix 动态渲染。对于重定位或自定义
Skill，请使用 `agent-sec-cli verify --skill /path/to/skill`。

## Outcome 与退出码

每次正常完成的运行都会先输出 `CHECKED`、`PASSED` 和 `FAILED` 计数，再输出一行精确的
最终状态：

| Outcome | 含义 | 最终状态行 | 退出码 |
|---------|------|------------|--------|
| `verified` | 至少检查了一个候选，且所有候选均通过 | `VERIFICATION PASSED` | `0` |
| `failed` | 至少一个候选验证失败 | `VERIFICATION FAILED` | `1` |
| `no_candidates` | 发现流程正常完成，但没有找到候选 | `VERIFICATION SKIPPED: NO CANDIDATE SKILLS` | `0` |

`no_candidates` 是成功的 best-effort 发现结果，不代表已验证任何资产。因此，默认根目录
缺失或为空，不会让未安装 Skill 的环境产生验证失败。

`--skill` 只指定一个候选。路径不存在、路径不是目录、Skill 不可读或内容无效时，结果
始终为 `failed`，退出码为 `1`；显式验证不会把这种输入映射为 `no_candidates`。

配置解析、受信密钥加载、canonicalization 或根目录枚举失败属于操作错误，退出码为 `1`。
此时不存在稳定验证结果，telemetry 可以省略资产 outcome；CLI 会把操作错误写入标准错误。

对于正常完成的运行，telemetry 会记录 `seccore.asset_outcome`，值为 `verified`、`failed`
或 `no_candidates`，并同时记录通过与失败计数。发现根目录和 Skill 路径不会上传。

## 自管理部署中的 Skill 签名

发布包应保留其发布签名。自行管理部署时，可以使用源码树中的签名工具生成本地签名密钥、
把公钥导出到 verifier 信任目录，并为 Skill 签名：

```bash
cd src/agent-sec-core
tools/sign-skill.sh --check
tools/sign-skill.sh --init
tools/sign-skill.sh /path/to/skill --force
```

修改任何受覆盖文件后都要重新签名。密钥管理、批量签名和 CI/CD 用法见
[Skill 签名指南](../../../../../src/agent-sec-core/tools/SIGNING_GUIDE_zh.md)。

## 故障排查

- `no_candidates`：检查 Skill 是否安装在两个默认根目录中；自定义位置请传入 `--skill`。
- `ERR_MANIFEST_MISSING` 或 `ERR_SIG_MISSING`：被发现的目录是候选，但未使用资产验证格式签名。
- `ERR_SIG_INVALID`：确认签名公钥已安装到包内 `trusted-keys/` 目录；manifest 变化后需重新签名。
- `ERR_HASH_MISMATCH` 或 `ERR_UNEXPECTED_FILE`：检查 Skill 内容，然后恢复已签名发布版本，
  或对已审查的内容有意重新签名。
- outcome 产生前的操作错误：检查 `config.conf`、受信密钥目录，以及每个现存发现根路径的
  类型和权限。

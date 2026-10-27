# 包安装与恢复参考

[English](../../../en/aw/reference/packages.md)

首次使用按[安装指南](../../../../user-guide/zh/aw/installation.md)操作。本文供打包、部署和自动化维护使用。

## 安装本地包

`aw install` 接受解压后的 Preview bundle 或包含 `package.json` 与 `payload/` 的独立包，`--bundle` 必须为绝对路径。
先将 `AW_PREFIX` 设为自己的绝对安装路径，再预览并执行。以下示例使用 user scope。

```bash
aw install --bundle /absolute/unpacked-bundle --prefix "$AW_PREFIX" --scope user --dry-run
aw install --bundle /absolute/unpacked-bundle --prefix "$AW_PREFIX" --scope user
aw list --installed --prefix "$AW_PREFIX" --json
aw doctor --prefix "$AW_PREFIX" --json
aw uninstall --prefix "$AW_PREFIX" --scope user --yes
```

root 管理的安装使用 `sudo` 和显式 `--scope system`，core、扩展和卸载保持相同 scope 与 prefix。
Preview Provider 要求同一次构建的 core；普通工具可独立安装。精确版本依赖须已在同一 prefix 中，或由同一 Preview 组合包提供。
已有组件升级使用新 prefix；安装器拒绝原地替换。

计划列出文件、依赖和阻碍；受阻计划返回 1。执行时重新校验输入。每文件最多 1 GiB，每 prefix 已登记内容最多 4 GiB、32767 个文件；恢复记录也受容量限制。
文件系统需支持 `O_TMPFILE`、硬链接、原子重命名和目录持久同步。

## 数据库与旧安装

| 范围 | 数据库 |
| --- | --- |
| user | `$XDG_STATE_HOME/aw/packages`，默认 `$HOME/.local/state/aw/packages` |
| system | `/var/lib/aw/packages` |

`sudo` 不自动选择 system scope。指定 `--prefix` 的查询使用该安装记录的数据库位置。
`installed.json` 保存包及文件归属，`transactions/` 保存未完成操作；`<prefix>/.aw-packages/aw-core.json` 保存数据库位置。
这些记录与 AW 运行时 `state_dir` 分开。

旧 Preview 的组件记录位于 `<prefix>/.aw-packages/*.json`。查询需显式 prefix；首次写入迁移时存档旧 JSON。
出现 `runtime_unknown` 时保留旧安装，改用新 prefix。旧安装器拒绝新的数据库位置格式。

## 故障恢复

遇到 `needs_recovery` 时，暂停包写入，选择原 scope 执行恢复。

```bash
aw recover --json
sudo aw recover --scope system --json
```

数据库已提交时完成清理，未提交时恢复旧文件和记录。冲突文件与备份会保留并报告路径，处理前不要删除它们。
`doctor` 检查归属文件，失败时 JSON 的 `result.report.issues` 列出问题；它不自动修复。
卸载拒绝被修改、缺失、有反向依赖或仍被 AW 使用的文件。最后一个包移除后，空登记、位置文件和锁继续保留。

报告格式见[CLI 自动化](cli.md)。事务和锁顺序见[包管理设计](../../../../../src/aw/docs/design/zh/packaging/package-management.md)，构建与发行验收见[贡献指南](../../../../../src/aw/CONTRIBUTING_zh.md)。

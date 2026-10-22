# 离线配置编辑

[English](../../en/configuration/configuration-editing.md)

`aw_package::config_edit::ConfigurationEditor` 编辑已有的期望 YAML 或 JSON
配置。统一 CLI 通过 `aw config show` 和编辑命令开放查看与修改。它不安装 Agent、不执行命令、
不发现 Provider 能力，也不证明原生效果支持。

## 编辑合同

`open` 使用随包提供的 `aw-config` 校验器验证完整原文件。`as_value`、`provider`
和 `hook` 分别提供工作文档、指定名称的 Provider，以及指定事件下的步骤。这些值
可能包含 Provider 私有配置，调用方负责决定哪些内容可以展示。

`add_provider` 插入显式完整的 Provider 定义。`add_hook` 向已有事件末尾追加完整的
结构化或原生步骤。事件不存在时，先通过 `add_event` 显式提供事件选项和步骤，
编辑器不会隐式启用事件。原有步骤顺序、事件选项和无关配置值都会保留；省略的
可选字段仍然省略，Provider 私有 `config` 中的未知键保持不变。未知公共字段、
重复输入键及不匹配的 Provider/步骤形态由现有 Schema 和静态校验器拒绝。

同名对象或同一事件下的同 ID 步骤已有相同值时，新增操作幂等；已有不同值时报告
冲突，不直接覆盖。删除不存在的对象或步骤同样幂等。`remove_hook` 删除最后一个
步骤后仍保留事件及其选项。`remove_provider` 拒绝仍有引用的 Provider，包括禁用
步骤的引用，不会隐式删除其他 Hook。

每次修改先校验候选文档，成功后才替换工作文档。非法输入和冲突都保留工作文档及
原文件。修改在内存中暂存，直到调用 `save`；`validate` 仅执行离线校验。

```rust
use aw_package::config_edit::ConfigurationEditor;
use serde_json::json;
use std::path::Path;

let mut editor = ConfigurationEditor::open(Path::new("/home/user/aw.yaml"))?;
editor.add_provider("native", json!({
    "protocol": "native-hook/v1alpha1",
    "transport": {
        "type": "stdio", "location": "agent", "argv": ["/opt/company/hook"]
    },
    "timeout_ms": 1000, "max_output_bytes": 4096, "config": {}
}))?;
// Use add_event only if the event is absent; preserve an existing event's options.
if editor.as_value()["spec"]["events"].get("tool.before").is_none() {
    editor.add_event("tool.before", json!({"enabled": true, "steps": []}))?;
}
editor.add_hook("tool.before", json!({
    "id": "company-before", "provider": "native", "native": {},
    "on_error": "report"
}))?;
editor.validate()?;
editor.save()?;
```

## 发布边界

文件必须是当前用户拥有的普通文件，不能有硬链接、特殊权限位、扩展访问 ACL 或
组/其他用户写权限。ACL mask 不能当作所属组的权限；快照检查也拒绝打开后新增的
ACL。目标文件、所持父目录及新建暂存目录均拒绝原生 NFSv4 ACL/DACL、rich ACL 和 CIFS/SMB 安全描述符
属性，包括等价于普通权限位的 ACL；
编辑器不转换这些 ACL 模型。暂存文件继承的原生 ACL 会在写入内容前被拒绝。
CIFS 的 DACL、owner+DACL 及完整安全描述符同时检查 `system.cifs_*` 和等价的
`system.smb3_*` 属性名称。
绝对路径不能包含符号链接或父目录跳转；父目录必须由当前用户拥有且不可
被组/其他用户写入。输入、展开数据和深度仍受现有校验器限制。

打开时逐级遍历目录组件，不跟随符号链接，并持有已校验的父目录描述符。读取、
快照检查、暂存和替换均通过 Linux `/proc/self/fd` 使用该描述符；必须挂载 procfs。
祖先目录改名不能把已打开的编辑器重定向到其他目录。快照读取时会重新检查所持
父目录的所有者及权限。

`save` 对当前文件取得非阻塞锁：实际修改要求文件可写并使用排他锁，语义无变化时
使用只读描述符和共享锁。这也满足 Linux NFS 的锁要求。比较设备/inode、权限、
所有者、所属组、硬链接数、大小、修改时间及完整
字节是否与原始快照一致，并在发布前立即重复检查。并发编辑器会遇到锁竞争或拒绝
过期快照，包括另一个编辑器已经原子替换原 inode 的情况。处理新版本时需重新打开
文件，编辑器不会自动重试或合并修改。其他写入方必须遵守同一文件锁，才能避免
最后一次检查与 rename 之间的竞争。每次快照都会重新校验当前用户拥有的普通文件、
权限及无硬链接约束。

语义无变化的保存，包括先修改后撤回，保留原始字节和 inode。有变化时，文档序列化
为 YAML，在目标父目录下的私有临时目录中保留权限位及所属组，并同步后原子替换。
暂存内容在替换前不可被其他用户读取。校验、锁竞争、快照检查及暂存写入失败均保留
原文件，并删除本操作创建的临时文件和目录。提交点之后不执行
可能失败的操作。实际修改会重写注释、格式和键顺序，不保留扩展属性或 ACL；恢复
原权限位前会移除继承的访问 ACL。这是
原子发布，不是断电持久性保证。配置字节变化也会改变运行时配置 revision。

## CLI 边界

`aw config` 提供编辑器的 `show`、`add-provider`、`remove-provider`、
`add-event`、`add-hook` 和 `remove-hook`。每次修改命令只暂存一个操作并保存一次，
多次调用不是批量事务；library 调用方可先暂存多个已校验操作再保存。必填、未知、
重复及互相冲突的选择器选项，都在打开配置或定义文件前检查。

添加操作通过 `--definition FILE` 接收完整 YAML/JSON 对象。定义文件可使用相对
路径，但须为普通文件。有界读取和 `aw_config::parse_value` 复用现有 4 MiB、
深度 32 的解析器，保留重复键、alias 展开及 JSON 兼容类型检查，再由编辑器校验
修改后的完整配置。不接收 stdin，不执行命令。`show` 打印格式化 JSON，包括
Provider 私有值；选择器可选 Provider、事件或事件下的步骤，指定对象不存在时
报错。`show` 不发布文件。

修改操作打印 `Updated configuration` 或 `Configuration unchanged`。
`aw config validate` 是单独的静态验证，接受相对路径和 `--against`，普通校验成功时
打印 `configuration valid`，不执行编辑器的所有权检查。独立 `aw-package` 可执行文件
及其编辑器专用验证已删除，Rust 的 `ConfigurationEditor::validate` API 保留。
CLI 编辑不修改 Schema、Registry 或 Service 合同，
也不重载当前服务。选项、回滚及使用新 revision 重启的说明见
[CLI 参考](../../../../../../docs/developer-guide/zh/aw/packages.md#编辑已有-provider-与-hook-配置)。

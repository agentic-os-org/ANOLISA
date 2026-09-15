# Qoder 独立插件打包

[English](README.md)

构建包含 Tokenless 运行时和共享 Hooks 的 Qoder 插件。
这个开发者打包入口不会安装软件或发布插件。普通组件安装仍优先使用
`anolisa install tokenless`，再执行 `anolisa adapter enable tokenless qoder`。

## 构建

使用与源码版本一致、来源可信且已校验 SHA-256 的预编译发行二进制。
每个平台目录包含 `tokenless`、`rtk` 和记录 Tokenless 发行版本的 `version.txt`。
打包器检查二进制架构；`version.txt` 是调用方提供的来源信息，不能证明二进制身份。
发布方必须锁定源码提交和发行包校验和。不要在 macOS 上编译。

```text
prebuilt/
  linux-x64/{tokenless,rtk,version.txt}
  linux-arm64/{tokenless,rtk,version.txt}
  darwin-arm64/{tokenless,rtk,version.txt}
```

```bash
python3 packaging/qoder/package.py --prebuilt-root /path/to/prebuilt \
  --target linux-x64 --target linux-arm64 --target darwin-arm64 \
  --output /path/to/new-plugin-directory
qodercli plugins validate /path/to/new-plugin-directory
qodercli plugins install /path/to/new-plugin-directory --scope user
```

输出目录必须不存在。仅选择已取得可信二进制的平台。可用独立构建的产物选择
`darwin-x64`，这不代表已有 Intel Mac 正式发行包。产物包含写入版本的原生
manifest、共享 Hooks、二进制、平台启动器、许可证和 SHA-256 清单。
不依赖 npm 安装生命周期脚本，也不需要模型调用 Skill。

## 运行与限制

安装后重启 Qoder CLI 或执行 `/plugins reload`。需要 Bash 和 Python 3.10+。
缺少依赖时向 stderr 输出诊断并保留原始工具结果。插件不会在工具调用期间下载
代码，也不会修改用户 Shell 配置。即使 PATH 上存在其他 Tokenless，也使用包内
版本。旧 Tool Ready Hook 保持关闭。

独立包仅为自己的 Hooks 设置 `TOKENLESS_DISABLE_SHELL_RECOVERY=1`。Hook 私有
PATH 不会传给 Qoder 的 Shell，因此关闭依赖 `tokenless retrieve` 的 Marker
压缩，包括需要原文恢复的表格采样。命令重写和不依赖恢复的压缩继续可用。
ANOLISA 安装的现有 adapter 保留原文恢复能力。独立包不包含依赖全局命令的
`tokenless-stats` 斜杠命令；可直接运行
`/path/to/new-plugin-directory/bin/tokenless stats`。

回退时使用 Qoder 原生插件管理器禁用或卸载插件。不要同时启用独立包和
ANOLISA 安装的副本。需要另行验证实际进入模型上下文的输出；结构校验和
模拟协议测试无法单独证明端到端节省效果。

# ANOLISA Omarchy 插件

[English](README.md)

插件在 Omarchy 顶栏提供一个下拉窗口。用户从这里打开 cosh 或 Qoder，
查看 sec-core 的安全检查记录和 Tokenless 的词元处理统计。
插件复用 ANOLISA 组件，不修改这些组件的核心代码。

## 当前状态

0.1.3 和 `omarchy/v0.1.3` 是待 review 的版本方案。
本次改动尚未在官方发布 Omarchy 安装包。
发布前请使用已验证的 Preview 包或本地候选包，步骤见[使用说明](USER_GUIDE_zh.md)。

## 安装和使用

维护者发布对应的官方 Release 后，在 Omarchy Linux x86_64 的桌面终端运行：

```bash
git clone --depth 1 https://github.com/agentic-os-org/ANOLISA.git
cd ANOLISA/distribution/omarchy
bash ./install
~/.local/bin/anolisa-omarchy status
```

请用当前桌面用户运行，不要给整个安装命令加 sudo。
脚本下载预编译包，校验后安装，不在用户电脑上编译。
系统需要 Python 3.11 或更新版本，以及可用的 Omarchy 桌面、sudo 和 GitHub 网络。
安装安全服务和补装已确认的系统依赖时会使用 sudo。

安装后点顶栏 **ANOLISA → Start setup**。
cosh 用 `/auth` 配置模型；Qoder 单独配置账户信息。
两项都能跳过，之后从 **Settings** 重新进入。
插件默认英文，可在 **Settings → Language** 切换简体中文。

```bash
anolisa-omarchy help
anolisa-omarchy status
anolisa-omarchy setup
anolisa-omarchy doctor
```

安装包包含 cosh、AW、两个 Provider、sec-core 的 Bash 扫描程序和 Tokenless。
Qoder CLI 从 Qoder 官方按需下载，默认推荐固定版本 1.1.84，也可选择 Latest。
插件不会静默更新 Qoder。未验收的新版本会显示提示。

## 移除

```bash
omarchy plugin remove community.anolisa
sudo systemctl disable --now anolisa-omarchy-security.service
```

以上操作移除顶栏插件、停用安全服务，并保留程序、账户配置和统计数据。
这不是彻底清理用户数据。安装路径和限制见[使用说明](USER_GUIDE_zh.md)。

- [使用说明和验收](USER_GUIDE_zh.md)
- [开发和发布流程](CONTRIBUTING_zh.md)
- [更新记录](CHANGELOG_zh.md)

# ws-ckpt 打包维护说明

[English](../../en/ws-ckpt/packaging.md)

## 本地打包与测试

在 Linux 上，从仓库根目录执行：

```bash
make -C src/ws-ckpt build
make -C src/ws-ckpt package-raw
make -C src/ws-ckpt test-package-raw
```

## Preview

指定待验证的分支及其 workspace 版本：

```bash
gh workflow run release-preview.yaml --repo agentic-os-org/ANOLISA \
  --ref <branch> -f component=ws-ckpt -f version=<workspace-version>
```

从该 workflow run 下载 Linux x86_64 和 aarch64 的构建产物。

## 正式发布标签

更新发布版本后，推送 `ckpt/v<version>` 标签触发 Release workflow。
标签版本须与 workspace 版本一致。workflow 会将两个 Linux 架构的预构建产物
附加到 GitHub Release。每次发布使用新的版本标签。

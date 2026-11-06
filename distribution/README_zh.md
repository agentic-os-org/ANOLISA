# 发行工具

[English](README.md)

本目录包含 ANOLISA 交付工具，以及构建 AW core 和独立 Provider 归档的脚本。
组件和 Provider owner 保留各自构建与发布责任；产品安装和升级归发行方或原生
包管理器负责。

| 入口 | 职责 |
| --- | --- |
| [packaging/aw](packaging/aw/README_zh.md) | 构建和组装独立的 AW core 与 Provider 归档 |
| `Makefile` | `aw-packages` 生成归档，`aw-integration-backends` 单独构建后端验收程序 |
| [anolisa](anolisa/README_zh.md) | ANOLISA CLI 源码、文档与交付入口 |

AW Provider 扩展管理使用 `aw-package` 内部的私有文件事务引擎；运行时文件读锁
归 `aw-exec`。这些模块不安装 core 或后端软件，也不组装产品发行。

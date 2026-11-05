# 镜像与云服务入口

[English](../../en/getting-started/images.md)

ANOLISA 是由龙蜥社区（OpenAnolis）发起的开源项目，目前在 GitHub 的
[agentic-os-org](https://github.com/agentic-os-org) 下开发和维护。

本目录收录预装系统镜像、容器基础镜像和开箱即用的云服务入口。欢迎社区与厂商
补充 ISO、VM 镜像下载地址、容器镜像拉取地址，以及云服务使用入口。

## 完整系统镜像

本节收录可独立下载并部署的 ISO、VM 等完整系统镜像。目前尚未收录独立下载链接，
欢迎社区与厂商补充。

## 容器基础镜像

构建 Agent 运行环境或平台镜像时，将基础镜像用于 `FROM`，在构建阶段安装环境
所需的组件软件包。

| 镜像 / 维护者 | 系统 / 架构 | 预装内容 | 拉取地址与使用说明 |
|---|---|---|---|
| Agentic OS 基础镜像 / Alibaba Cloud | Alibaba Cloud Linux 4 / linux/amd64 | ANOLISA CLI 与组件仓库配置 | `ac2-registry.cn-hangzhou.cr.aliyuncs.com/ac2/agentic-os:1.3` · [基础镜像与派生镜像构建指南](https://help.aliyun.com/zh/alinux/agentic-os-base-container-image) |

该基础镜像不含组件二进制。你需要在构建阶段安装所需组件的软件包。
各组件指南说明了运行依赖，上面的镜像指南说明了启动方式。

## 云服务使用入口

你可以直接使用云服务提供的预装环境，体验 ANOLISA，无需自行下载系统镜像。

| 服务 / 提供方 | 入口与使用说明 |
|---|---|
| 阿里云云服务器（ECS） / Alibaba Cloud | 按照 [ECS 公共镜像与首次使用指南](https://help.aliyun.com/zh/alinux/agentic-os-getting-started)，在创建实例或更换操作系统时选择 Alibaba Cloud Linux 4 Agentic 版，再完成环境准备与模型授权。 |
| 阿里云轻量应用服务器（SWAS）智能体专用实例 / Alibaba Cloud | 打开[实例创建入口](https://swasnext.console.aliyun.com/buy)，选择 **Alibaba Cloud Linux 4 Agentic 版**。 |
| 阿里云无影云电脑（AI PC） / Alibaba Cloud | 打开无影个人版的[镜像使用入口](https://wuying-personal.aliyun.com/platform#/imageZone?defaultImageCode=93316207)，按平台指引使用。 |

## 贡献镜像与云服务入口

欢迎通过 PR 修改本页及对应英文页，新增或更新条目。请提供以下信息：

- 镜像：名称、维护者、系统与架构、预装内容、下载或拉取地址，以及使用说明。
- 云服务：服务名称、提供方、使用入口，以及所需镜像或实例的选择说明。

镜像维护者可以将镜像文件发布到 Releases、镜像站或 OCI registry。
本目录页集中收录链接，并标明各条目的维护者或服务提供方；镜像与云服务由对应
厂商或社区维护者发布和运营。

PR 流程见[贡献指南](../../../../CONTRIBUTING_zh.md)。

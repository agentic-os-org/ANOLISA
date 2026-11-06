# docs 目录说明

[English](README.md)

按要完成的任务选择指南。安装、使用和运维 ANOLISA，请阅读
[用户指南](user-guide/zh/index.md)；在其他项目中接入或扩展 ANOLISA，请阅读
[开发者指南](developer-guide/zh/index.md)；修改、测试或发布 ANOLISA 本身，请从
[贡献指南](../CONTRIBUTING_zh.md)开始。

## 目录结构

```text
ANOLISA/
├── CONTRIBUTING.md              # 通用贡献指南
├── CONTRIBUTING_zh.md
├── contribute/
│   └── {en,zh}/
│       ├── aw/                  # 构建和发行验证
│       ├── cosh-ng/             # 组件开发和测试
│       └── ws-ckpt/packaging.md
└── docs/
    ├── README.md                # 文档目录的组织与维护说明
    ├── README_zh.md
    ├── navigation.yaml          # 网站导航、中英文标签和简介
    ├── user-guide/
    │   └── {en,zh}/
    │       ├── index.md
    │       ├── getting-started/
    │       │   ├── quickstart.md
    │       │   ├── installation.md
    │       │   ├── building.md  # 当前位置，见下文
    │       │   ├── testing.md   # 当前位置，见下文
    │       │   └── …
    │       └── {component}/index.md
    └── developer-guide/
        └── {en,zh}/
            ├── index.md
            ├── sdk/tokenless/   # SDK 总览和 AgentScope 集成
            └── aw/              # 框架、Provider 和观测接入
                └── providers/
```

用户手册中的组件目录平铺存放。每个组件的 `index.md` 介绍功能、包含首次使用步骤，
并在需要时链接到其他章节。共用的用户任务放在 `getting-started/`。项目级
Quickstart 带读者用简短流程获得第一个结果；`getting-started/` 无需再放索引页。
现有 `building.md` 和 `testing.md` 暂保留原路径，后续按任务拆分内容。

对于项目支持的源码安装方式，用户指南可以链接到贡献指南中独立可执行的构建章节。
用户页面说明适用的平台和版本，以及如何安装和验证产物；构建章节维护工具链要求、
构建命令和产物路径。开发环境准备和 PR 检查放在贡献指南。

网站的阅读分组在 [`navigation.yaml`](navigation.yaml) 中定义，不要求文档源码
目录也按同样的层级存放。调整分组或名称时，无需移动组件手册。组件 URL 同样
平铺：入口为 `/docs/user-guide/aw/`，章节为 `/docs/user-guide/aw/installation/`
这样的路径。共用页面放在 `/docs/user-guide/getting-started/<page>/`。旧地址直接
跳转到当前页面，保留语言和传入的章节锚点。中英文用户手册
索引中的导航区块由这份 YAML 生成：

```bash
node website/scripts/navigation.mjs --write-indexes
```

组件条目的 `description.en` 和 `description.zh` 是手册索引中固定的一句话简介。
在 YAML 中维护这些简介，并与组件文档保持一致，不从 README 段落中自动抽取。
网站使用同一份简介，侧栏仍只显示组件名称。简介只说明组件做什么，不放版本号
或安装细节。

修改导航后，在仓库根目录执行此命令。请修改 YAML，不要直接修改手册索引中的
生成区块。网站也从这些 Markdown 源文件生成页面，生成的网站文件不作为另一份
文档源码维护。

## 其他文档放在哪里

通用贡献流程放在仓库根目录的[贡献指南](../CONTRIBUTING_zh.md)中，组件开发章节
放在根目录 [`contribute/`](../contribute/zh/)。公开 SDK、API 和集成文档集中在
[`developer-guide/`](developer-guide/zh/index.md)。组件的设计理由、不变量和内部接口
随实现放在 `src/<component>/docs/design/` 或组件登记的源码目录下的对应位置。
项目级或跨组件 ADR 仍遵循现有的责任归属、存放位置和评审流程。

同一份导航 YAML 也定义 SDK、AW 接入下拉菜单和贡献者指南侧栏。网站将现有根目录及
组件内的 `CONTRIBUTING*.md` 与 `contribute/` 章节一起展示，不另写重复的贡献总览。
SDK 目录页由网站根据配置的条目生成。

已弃用组件的手册随组件保留在 `deprecated/<component>/docs/` 中，不进入当前用户
手册和网站导航。[文档规范](../specs/documentation-standard.md)规定命名、中英文
对应和维护要求。

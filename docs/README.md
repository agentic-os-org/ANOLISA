# About this documentation directory

[中文版](README_zh.md)

This directory separates the user manual from documentation about developing
ANOLISA. Start with the [user guide](user-guide/en/index.md) to install or use
components; this page explains how documentation sources are organized.

## Directory layout

```text
docs/
├── README.md                    # Documentation organization and maintenance
├── README_zh.md
├── navigation.yaml              # Website groups, order, bilingual labels and descriptions
├── user-guide/
│   ├── en/
│   │   ├── index.md             # User manual entry point
│   │   ├── getting-started/     # Shared installation, build, test, and troubleshooting
│   │   │   └── quickstart.md    # One short project-level first-use path
│   │   ├── aw/index.md
│   │   ├── cosh-ng/index.md
│   │   ├── skillfs/index.md
│   │   └── …/index.md           # One directory per component
│   └── zh/                     # Matching Chinese manual
└── developer-guide/
    ├── en/                     # Existing developer documentation
    └── zh/
```

The user manual keeps component directories side by side. Each component's
`index.md` introduces its functionality, includes its first-use steps, and links
to further chapters when needed. Shared tasks live in `getting-started/`, so
users and contributors can refer to the same installation, build, test, and
troubleshooting instructions. The project Quickstart stays a separate short
journey with links to the next task; `getting-started/` does not need another
index page.

Website navigation groups are defined in [`navigation.yaml`](navigation.yaml).
They organize reading paths without imposing the same hierarchy on source
directories. Changing a group or its label does not require moving a component
manual. Component URLs are flat too: `/docs/user-guide/aw/` is the entry point,
and chapters use paths such as `/docs/user-guide/aw/installation/`. Shared pages
use `/docs/user-guide/getting-started/<page>/`. Old addresses redirect directly
to their current pages, preserving the language and incoming section fragment.
The navigation blocks in the English and Chinese manual indexes are
generated from this file:

```bash
node website/scripts/navigation.mjs --write-indexes
```

Each component's `description.en` and `description.zh` define its fixed one-sentence
introduction in the manual index. Maintain these descriptions in the YAML,
consistent with the component documentation; they are not extracted from README
paragraphs. The website uses the same descriptions, while sidebar entries show
only the component labels. Keep descriptions focused on what the component does,
without release numbers or installation details.

Run this command from the repository root after changing the navigation. Edit
the YAML rather than the generated blocks in the manual indexes. The website
also consumes these canonical Markdown sources; generated site files are not
separate documentation sources.

## Other documentation locations

General contribution instructions live in the repository's
[contribution guide](../CONTRIBUTING.md). Component development and integration
documentation is collected in
[`developer-guide/`](developer-guide/en/README.md). Component technical designs
stay with their source under `src/<component>/docs/` or the component's
registered source root.

Manuals for deprecated components are kept with the archived component under
`deprecated/<component>/docs/` and are excluded from the current manual and
website navigation. The [documentation standard](../specs/documentation-standard.md)
defines naming, bilingual parity, and maintenance requirements.

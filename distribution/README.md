# Distribution tooling

[中文版](README_zh.md)

This directory contains ANOLISA delivery tooling and scripts assembling AW core
and independent Provider archives. Component and Provider owners retain their
own builds and releases; the distributor or native package manager owns product
installation and upgrades.

| Entry | Responsibility |
| --- | --- |
| [packaging/aw](packaging/aw/README.md) | Build and assemble independent AW core and Provider archives |
| `Makefile` | `aw-packages` for archives and `aw-integration-backends` for separate backend acceptance builds |
| [anolisa](anolisa/README.md) | ANOLISA CLI source, documentation and delivery entrypoints |

AW Provider extension management uses the private file transaction engine inside
`aw-package`; runtime file leases belong to `aw-exec`. These modules do not install
core or backend software or assemble product releases.

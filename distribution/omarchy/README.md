# ANOLISA Omarchy integration

[简体中文](README_zh.md)

A native Omarchy bar dropdown for opening cosh and Qoder, with local sec-core
and Tokenless views. It reuses ANOLISA components without changing their core
source. It does not replace Omarchy's desktop or terminal emulator.

## Release status

Version 0.1.3 and tag `omarchy/v0.1.3` are proposed for review. No official
Omarchy release has been published by this change. Until publication, use a
verified Preview or local candidate archive as described in the [user guide](USER_GUIDE.md).

## Install

After the maintainers publish the matching official Release, run these commands
as the logged-in desktop user on Omarchy Linux x86_64:

```bash
git clone --depth 1 https://github.com/agentic-os-org/ANOLISA.git
cd ANOLISA/distribution/omarchy
bash ./install
~/.local/bin/anolisa-omarchy status
```

The script downloads the versioned prebuilt archive and SHA-256 sidecar from the
official ANOLISA Release. It checks the archive and every file before installation.
No compilation takes place on the laptop. Python 3.11 or later, a working
Omarchy desktop session, sudo and access to GitHub are required. Runtime libraries
are checked before activation. The installer uses sudo for the system safety
service and any confirmed missing system packages.

Open **ANOLISA** in the bar, then **Start setup**. Configure cosh's model service
with `/auth`. Configure Qoder separately through Qoder CLI or a PAT in the wizard.
Either step can be deferred and reopened from **Settings**.

The archive contains cosh, AW, two Providers, the sec-core Bash-scanning runtime
and Tokenless. Qoder is optional and downloaded from its own official service.
The default choice is the fixed, tested version 1.1.84. Users can choose Latest;
untested versions are marked as such. Downloads do not happen silently.

## Use and remove

```bash
anolisa-omarchy help
anolisa-omarchy status
anolisa-omarchy setup
anolisa-omarchy doctor
```

The panel defaults to English. **Settings → Language** also offers Simplified
Chinese. Qoder keeps its own interface settings. **Help** explains the commands
and components. The sec-core view reports recent Bash scans and risk findings;
Tokenless reports estimated reductions in processed tool content, not billing savings.

To remove the bar widget and stop its safety service:

```bash
omarchy plugin remove community.anolisa
sudo systemctl disable --now anolisa-omarchy-security.service
```

This preserves installed programs, credentials and statistics. It is not a full
data purge. See the [user guide](USER_GUIDE.md) for paths, upgrades and limitations.

- [User guide and acceptance checks](USER_GUIDE.md)
- [Development, Preview and release procedure](CONTRIBUTING.md)
- [Build entry point](BUILDING.md)
- [Changelog](CHANGELOG.md)

License: [Apache-2.0](../../LICENSE). Adapted Omarchy popup code retains its MIT
notice in `plugin/OMARCHY-LICENSE`. Bundled components retain their own licenses.

# Changelog

[中文版](CHANGELOG_zh.md)

All notable changes to OS Skills are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.3] - 2026-08-21

### Fixed

- The RPM now provides `anolisa-component(os-skills)`, allowing
  `anolisa upgrade` to resolve the OS Skills package from RPM metadata for an
  existing component when the repository-side component index is unavailable
  ([#2576](https://github.com/alibaba/anolisa/pull/2576)).

## [0.6.2] - 2026-08-07

- Added the `ktuner` skill for deterministic kernel diagnosis, tuning, and rollback. (#1278)
- Removed legacy OpenClaw and Hermes adapter scripts from source and RPM installs. (#1172)
- Updated `anolisa-guide` with authenticated Skill Ledger recovery and tamper detection. (#2185)

## [0.6.1] - 2026-07-03

- Rewrote `sysom-diagnosis` skill and removed legacy CLI. (#1241)
- Fixed OpenClaw gateway write scope verification in `install-openclaw` skill. (#1205)

## [0.6.0] - 2026-06-29

- Added anolisa component contract (component.toml, Makefile, RPM spec). (#1159)
- Added OpenClaw bootstrap guidance to `install-openclaw` skill. (#1051)
- Added model endpoint preflight before gateway startup in `install-openclaw` skill. (#1031)
- Added static knowledge base update script for `anolisa-guide` skill. (#1010)
- Added `anolisa-guide` skill. (#849)
- Fixed Aliyun mirror fallback for uv and qwenpaw install. (#968)
- Fixed dashscope proxy URL to new Anthropic endpoint in `install-claude-code` skill. (#858)
- Renamed `copaw` to `qwenpaw` across os-skills. (#968)

## [0.5.0] - 2026-06-11

- Added `anolisa-register` skill. (#829)

## [0.4.0] - 2026-06-08

- Added auto-install tokenless plugin support for agent install skills. (#731)
- Added OpenClaw dependency precheck. (#719)
- Improved OpenClaw non-interactive setup. (#687)
- Added Hermes adapter runner. (#617)
- Added standalone ANOLISA adapter entry. (#549)
- Fixed OpenClaw state dir handling normalization. (#641)
- Improved Makefile install paths and contract. (#541)

## [0.3.0] - 2026-04-26

- Added `hermes-agent-install` skill. (#353)
- Added `clawhub-skill-mng` skill with npm install support and YAML description matching. (#315)
- Fixed AgentSight custom db path issue, using default paths instead. (#366)
- Fixed AgentSight token savings query support. (#355)
- Fixed AgentSight interruption CLI and aligned `conversation_id` naming. (#334)

## [0.2.2] - 2026-04-15

- Support enable AgentSight dashboard in `agentsight` skill. (#222)

## [0.2.1] - 2026-04-14

- Upgraded `xlsx` skill with MiniMax open-source implementation. (#218)
- Updated skill descriptions from "suitable for alinux4" to "rpm-base linux". (#182)

## [0.2] - 2026-04-12

- Added `humanizer`, `image-gen`, `pdf-reader`, and `xlsx` skills. (#178)
- Added `cosh-guide` skill. (#23)
- Support net/io/load diagnostic capabilities to `sysom-diagnosis` skill. (#163)

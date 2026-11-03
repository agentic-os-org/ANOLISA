# SkillFS component build and file installation entry point.
.DEFAULT_GOAL := build

.PHONY: build build-core build-debug
build: build-core
build-core:
	cargo build --release --workspace --locked
build-debug:
	cargo build --workspace

# Shared file-only installers also supply the legacy raw payload.
SOURCE_DIR ?= $(CURDIR)
PREFIX ?= /usr
BINARY ?= target/release/skillfs
BINDIR ?= /usr/local/bin
DOCDIR ?= $(PREFIX)/share/doc/skillfs
SKILLDIR ?= $(PREFIX)/share/anolisa/skills/skillfs-mount

.PHONY: install-core-files install-skills-files
install-core-files:
	install -d -m 0755 "$(DESTDIR)$(BINDIR)" "$(DESTDIR)$(DOCDIR)"
	install -m 0755 "$(BINARY)" "$(DESTDIR)$(BINDIR)/skillfs"
	install -m 0644 "$(SOURCE_DIR)/LICENSE" "$(SOURCE_DIR)/README.md" "$(DESTDIR)$(DOCDIR)/"

install-skills-files:
	@test -f "$(SOURCE_DIR)/docs/skills/skillfs-mount/SKILL.md" || { echo 'Missing bundled skillfs-mount skill' >&2; exit 1; }
	install -d -m 0755 "$(DESTDIR)$(SKILLDIR)"
	cp -pr "$(SOURCE_DIR)/docs/skills/skillfs-mount/." "$(DESTDIR)$(SKILLDIR)/"

include packaging/delivery.mk

.PHONY: help
help:
	@printf '%s\n' 'Targets: build install install-core install-skills build-core build-debug layout test-packaging'

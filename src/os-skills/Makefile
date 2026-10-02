# Makefile for os-skills

INSTALL_PROFILE ?= system
DESTDIR ?=
ifeq ($(INSTALL_PROFILE),user)
PREFIX ?= $(HOME)/.local
SKILLS_DIR ?= $(HOME)/.copilot-shell/skills
else
PREFIX ?= /usr
endif

DATADIR ?= $(PREFIX)/share
ifeq ($(INSTALL_PROFILE),system)
SKILLS_DIR ?= $(DATADIR)/anolisa/skills
endif
COMPONENT_DIR ?= $(DATADIR)/anolisa/components/os-skills

.PHONY: build install uninstall clean help

build:
	@echo "os-skills has no compile step"

install:
	@echo "==> Installing os-skills to $(DESTDIR)$(SKILLS_DIR)"
	@find . -name SKILL.md -print | while read -r skill_file; do \
		skill_dir=$$(dirname "$$skill_file"); \
		skill_name=$$(basename "$$skill_dir"); \
		install -d -m 0755 "$(DESTDIR)$(SKILLS_DIR)/$$skill_name"; \
		cp -pr "$$skill_dir/." "$(DESTDIR)$(SKILLS_DIR)/$$skill_name/"; \
	done
	install -d -m 0755 "$(DESTDIR)$(COMPONENT_DIR)"
	install -p -m 0644 component.toml "$(DESTDIR)$(COMPONENT_DIR)/component.toml"

uninstall:
	@echo "==> Removing os-skills from $(DESTDIR)$(SKILLS_DIR)"
	@find . -name SKILL.md -print | while read -r skill_file; do \
		skill_dir=$$(dirname "$$skill_file"); \
		skill_name=$$(basename "$$skill_dir"); \
		rm -rf "$(DESTDIR)$(SKILLS_DIR)/$$skill_name"; \
	done
	rm -rf "$(DESTDIR)$(COMPONENT_DIR)"

clean:
	@true

help:
	@echo "Targets: build install uninstall clean"
	@echo "Variables:"
	@echo "  INSTALL_PROFILE=user|system"
	@echo "  DESTDIR= staging root for packages"
	@echo "  PREFIX= install prefix"

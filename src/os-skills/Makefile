# Makefile for os-skills
.DEFAULT_GOAL := build

INSTALL_PROFILE ?= $(if $(filter 0,$(shell id -u)),system,user)
AWPRJ ?= anolisa
DESTDIR ?=
ifeq ($(INSTALL_PROFILE),user)
PREFIX ?= $(HOME)/.local
DATADIR ?= $(if $(XDG_DATA_HOME),$(XDG_DATA_HOME),$(HOME)/.local/share)
SKILLS_DIR ?= $(HOME)/.copilot-shell/skills
else
PREFIX ?= /usr/local
DATADIR ?= $(PREFIX)/share
# Standalone skills use the consumer's data root so cosh can discover them.
SKILLS_DIR ?= $(DATADIR)/$(if $(strip $(AWPRJ)),$(AWPRJ),cosh-ng)/skills
endif
COMPONENT_DIR ?= $(DATADIR)/anolisa/components/os-skills

.PHONY: build install uninstall check-layout clean help

check-layout:
	@case "$(INSTALL_PROFILE)" in system|user) ;; *) echo 'INSTALL_PROFILE must be system or user' >&2; exit 1 ;; esac
	@case "$(AWPRJ)" in ''|[a-zA-Z0-9]*) ;; *) echo 'AWPRJ must start with a letter or digit' >&2; exit 1 ;; esac
	@case "$(AWPRJ)" in *[!a-zA-Z0-9_.-]*) echo 'AWPRJ must be a single namespace segment' >&2; exit 1 ;; esac
	@for path in "$(if $(PREFIX),$(PREFIX),/)" "$(DATADIR)" "$(SKILLS_DIR)" "$(COMPONENT_DIR)" "$(if $(DESTDIR),$(DESTDIR),/)"; do \
		case "$$path" in /*) ;; *) echo "Installation path must be absolute: $$path" >&2; exit 1 ;; esac; \
		case "/$$path/" in */../*|*/./*) echo "Installation path contains a dot segment: $$path" >&2; exit 1 ;; esac; \
	done

build:
	@echo "os-skills has no compile step"

install: check-layout
	@echo "==> Installing os-skills to $(DESTDIR)$(SKILLS_DIR)"
	@find . -name SKILL.md -print | while read -r skill_file; do \
		skill_dir=$$(dirname "$$skill_file"); \
		skill_name=$$(basename "$$skill_dir"); \
		install -d -m 0755 "$(DESTDIR)$(SKILLS_DIR)/$$skill_name"; \
		cp -pr "$$skill_dir/." "$(DESTDIR)$(SKILLS_DIR)/$$skill_name/"; \
	done
	install -d -m 0755 "$(DESTDIR)$(COMPONENT_DIR)"
	install -p -m 0644 component.toml "$(DESTDIR)$(COMPONENT_DIR)/component.toml"

uninstall: check-layout
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
	@echo "  PREFIX= system install prefix"
	@echo "  AWPRJ= system resource namespace (default: anolisa)"
	@echo "  SKILLS_DIR= explicit skill collection directory"

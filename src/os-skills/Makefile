# Makefile for os-skills
.DEFAULT_GOAL := build

INSTALL_PROFILE ?= $(if $(filter 0,$(shell id -u)),system,user)
AWPRJ ?= anolisa
DESTDIR ?=
# Unset or 0: install refuses a same-name directory it did not create; 1 takes it over.
REPLACE_UNOWNED ?=
empty :=
space := $(empty) $(empty)
masked_space := |os-skills-space|
trim_slashes = $(if $(filter-out /,$(filter %/,$(1))),$(call trim_slashes,$(patsubst %/,%,$(1))),$(1))
# Drops trailing slashes but keeps /. Spaces are masked first because patsubst
# works on words and would otherwise rewrite or collapse paths with spaces.
strip_slash = $(subst $(masked_space),$(space),$(call trim_slashes,$(subst $(space),$(masked_space),$(1))))
ifeq ($(INSTALL_PROFILE),user)
# A trailing slash is cosmetic; dropping it keeps one collection on one receipt.
HOME := $(call strip_slash,$(HOME))
PREFIX ?= $(HOME)/.local
DATADIR ?= $(if $(XDG_DATA_HOME),$(call strip_slash,$(XDG_DATA_HOME)),$(HOME)/.local/share)
SKILLS_DIR ?= $(HOME)/.copilot-shell/skills
else
PREFIX ?= /usr/local
DATADIR ?= $(PREFIX)/share
# Standalone skills use the consumer's data root so cosh can discover them.
SKILLS_DIR ?= $(DATADIR)/$(if $(strip $(AWPRJ)),$(AWPRJ),cosh-ng)/skills
endif
# The receipt records this directory, so /a and /a/ must name one collection.
override SKILLS_DIR := $(call strip_slash,$(SKILLS_DIR))
COMPONENT_DIR ?= $(DATADIR)/anolisa/components/os-skills
# Install receipt: one line per skill name written by install, read by uninstall.
MANIFEST ?= $(COMPONENT_DIR)/install-manifest

.PHONY: build install uninstall check-layout clean help

check-layout:
	@case "$(INSTALL_PROFILE)" in system|user) ;; *) echo 'INSTALL_PROFILE must be system or user' >&2; exit 1 ;; esac
	@case "$(REPLACE_UNOWNED)" in ''|0|1) ;; *) echo 'REPLACE_UNOWNED must be 1 to take over unowned directories, or 0 or unset' >&2; exit 1 ;; esac
	@case "$(AWPRJ)" in ''|[a-zA-Z0-9]*) ;; *) echo 'AWPRJ must start with a letter or digit' >&2; exit 1 ;; esac
	@case "$(AWPRJ)" in *[!a-zA-Z0-9_.-]*) echo 'AWPRJ must be a single namespace segment' >&2; exit 1 ;; esac
	@for path in "$(if $(PREFIX),$(PREFIX),/)" "$(DATADIR)" "$(SKILLS_DIR)" "$(COMPONENT_DIR)" "$(MANIFEST)" "$(if $(DESTDIR),$(DESTDIR),/)"; do \
		case "$$path" in /*) ;; *) echo "Installation path must be absolute: $$path" >&2; exit 1 ;; esac; \
		case "/$$path/" in */../*|*/./*) echo "Installation path contains a dot segment: $$path" >&2; exit 1 ;; esac; \
	done

build:
	@echo "os-skills has no compile step"

install: check-layout
	@if [ -e "$(DESTDIR)$(MANIFEST)" ] && [ ! -f "$(DESTDIR)$(MANIFEST)" ]; then \
		echo "error: $(DESTDIR)$(MANIFEST) exists but is not a regular file" >&2; \
		exit 1; \
	fi
# Installing a skill replaces its whole directory, which must not hold the receipt.
	@find . -name SKILL.md -print | while IFS= read -r skill_file; do \
		skill_name=$$(basename "$$(dirname "$$skill_file")"); \
		for path in "$(COMPONENT_DIR)" "$(MANIFEST)"; do \
			case "$$path/" in "$(SKILLS_DIR)/$$skill_name"/*) \
				echo "error: $$path is inside the skill directory $(SKILLS_DIR)/$$skill_name" >&2; exit 1 ;; \
			esac; \
		done; \
	done
	@if [ -f "$(DESTDIR)$(MANIFEST)" ] && ! grep -qxF -- "# skills_dir=$(SKILLS_DIR)" "$(DESTDIR)$(MANIFEST)"; then \
		echo "error: $(DESTDIR)$(MANIFEST) records a different skill collection; pass MANIFEST= to keep a separate receipt" >&2; \
		exit 1; \
	fi
# Renaming the receipt into place commits the install: restore() rolls back only
# while the new receipt is still .tmp, so payloads and receipt always agree.
# After that it only finishes cleanup, so the trap stays until the recipe exits.
	@echo "==> Installing os-skills to $(DESTDIR)$(SKILLS_DIR)"; \
	manifest_dir=$$(dirname "$(DESTDIR)$(MANIFEST)") || exit 1; \
	install -d -m 0755 "$(DESTDIR)$(COMPONENT_DIR)" "$$manifest_dir" "$(DESTDIR)$(SKILLS_DIR)" || exit 1; \
	stage=$$(mktemp -d "$(DESTDIR)$(SKILLS_DIR)/.os-skills.XXXXXX") || exit 1; \
	mkdir "$$stage/.old" || { rm -rf "$$stage"; exit 1; }; \
	: > "$$stage/.published"; \
	receipt=pending; \
	restore() { \
		trap '' HUP INT TERM; \
		if [ "$$receipt" = committing ] && [ ! -e "$(DESTDIR)$(MANIFEST).tmp" ]; then \
			rm -f "$(DESTDIR)$(MANIFEST).new"; \
			rm -rf "$$stage"; \
			echo "error: install stopped after $(DESTDIR)$(MANIFEST) recorded the new skills; run make install again to finish" >&2; \
			exit 1; \
		fi; \
		intact=yes; \
		while IFS= read -r done_name; do \
			rm -rf "$(DESTDIR)$(SKILLS_DIR)/$$done_name"; \
			if [ -e "$(DESTDIR)$(SKILLS_DIR)/$$done_name" ] || [ -L "$(DESTDIR)$(SKILLS_DIR)/$$done_name" ]; then \
				echo "error: cannot remove the new copy at $(DESTDIR)$(SKILLS_DIR)/$$done_name" >&2; \
				intact=no; \
			fi; \
		done < "$$stage/.published"; \
		lost=no; \
		for backup in "$$stage/.old"/*; do \
			[ -e "$$backup" ] || [ -L "$$backup" ] || continue; \
			target="$(DESTDIR)$(SKILLS_DIR)/$${backup##*/}"; \
			if [ -e "$$target" ] || [ -L "$$target" ] || ! mv "$$backup" "$$target"; then lost=yes; fi; \
		done; \
		rm -f "$(DESTDIR)$(MANIFEST).new" "$(DESTDIR)$(MANIFEST).tmp"; \
		if [ "$$lost" = yes ]; then \
			find "$$stage" -mindepth 1 -maxdepth 1 ! -name .old -exec rm -rf {} +; \
			echo "error: install failed and some previous skills could not be restored; they are kept in $$stage/.old" >&2; \
			exit 1; \
		fi; \
		rm -rf "$$stage"; \
		if [ "$$intact" = no ]; then \
			echo "error: install failed and could not remove every new copy; the receipt is unchanged" >&2; \
			exit 1; \
		fi; \
		echo "error: install failed; the previously installed skills are unchanged" >&2; \
		exit 1; \
	}; \
	trap restore HUP INT TERM; \
	find . -name SKILL.md -print > "$$stage/.sources" || restore; \
	: > "$$stage/.names"; \
	while IFS= read -r skill_file; do \
		skill_dir=$$(dirname "$$skill_file"); \
		skill_name=$$(basename "$$skill_dir"); \
		case "$$skill_name" in \
		''|.*|*[!a-zA-Z0-9_.-]*) echo "error: unsupported skill directory name: $$skill_name" >&2; restore ;; \
		esac; \
		if grep -qxF -- "$$skill_name" "$$stage/.names"; then \
			echo "error: duplicate skill directory name: $$skill_name" >&2; restore; \
		fi; \
		cp -pr "$$skill_dir" "$$stage/$$skill_name" || restore; \
		printf '%s\n' "$$skill_name" >> "$$stage/.names"; \
	done < "$$stage/.sources"; \
	printf '# skills_dir=%s\n' "$(SKILLS_DIR)" > "$(DESTDIR)$(MANIFEST).new" || restore; \
	while IFS= read -r skill_name; do \
		target="$(DESTDIR)$(SKILLS_DIR)/$$skill_name"; \
		owned=no; \
		if grep -qxF -- "$$skill_name" "$(DESTDIR)$(MANIFEST)" 2>/dev/null; then owned=yes; fi; \
		if [ -e "$$target" ] || [ -L "$$target" ]; then \
			if [ "$$owned" = no ] && [ "$(REPLACE_UNOWNED)" != 1 ]; then \
				echo "error: $$target was not installed by os-skills; move it away or pass REPLACE_UNOWNED=1" >&2; \
				restore; \
			fi; \
			if [ "$$owned" = no ]; then \
				echo "warning: replacing $$target, which os-skills did not install" >&2; \
			fi; \
			mv "$$target" "$$stage/.old/$$skill_name" || restore; \
		fi; \
		printf '%s\n' "$$skill_name" >> "$$stage/.published"; \
		mv "$$stage/$$skill_name" "$$target" || restore; \
		printf '%s\n' "$$skill_name" >> "$(DESTDIR)$(MANIFEST).new" || restore; \
	done < "$$stage/.names"; \
	if [ -f "$(DESTDIR)$(MANIFEST)" ]; then \
		cat "$(DESTDIR)$(MANIFEST).new" "$(DESTDIR)$(MANIFEST)" | LC_ALL=C sort -u > "$(DESTDIR)$(MANIFEST).tmp" || restore; \
	else \
		LC_ALL=C sort -u "$(DESTDIR)$(MANIFEST).new" > "$(DESTDIR)$(MANIFEST).tmp" || restore; \
	fi; \
	receipt=committing; \
	mv -f "$(DESTDIR)$(MANIFEST).tmp" "$(DESTDIR)$(MANIFEST)" || restore; \
	rm -f "$(DESTDIR)$(MANIFEST).new"; \
	install -p -m 0644 component.toml "$(DESTDIR)$(COMPONENT_DIR)/component.toml" || restore; \
	rm -rf "$$stage"

uninstall: check-layout
	@if [ ! -f "$(DESTDIR)$(MANIFEST)" ]; then \
		echo "error: no os-skills install manifest at $(DESTDIR)$(MANIFEST); nothing removed" >&2; \
		echo "error: os-skills removes only the skills recorded by its own install" >&2; \
		exit 1; \
	fi
	@if ! grep -qxF -- "# skills_dir=$(SKILLS_DIR)" "$(DESTDIR)$(MANIFEST)"; then \
		echo "error: $(DESTDIR)$(MANIFEST) records a different skill collection; use the same SKILLS_DIR as install" >&2; \
		exit 1; \
	fi
	@echo "==> Removing os-skills from $(DESTDIR)$(SKILLS_DIR)"; \
	status=0; \
	while IFS= read -r skill_name; do \
		case "$$skill_name" in ''|'#'*) continue ;; esac; \
		case "$$skill_name" in \
		'.'|'..'|.*|*[!a-zA-Z0-9_.-]*) \
			echo "error: unreadable receipt entry: $$skill_name" >&2; status=1 ;; \
		esac; \
	done < "$(DESTDIR)$(MANIFEST)"; \
	if [ $$status -ne 0 ]; then \
		echo "error: fix $(DESTDIR)$(MANIFEST) before uninstalling; nothing removed" >&2; exit 1; \
	fi; \
	while IFS= read -r skill_name; do \
		case "$$skill_name" in ''|'#'*) continue ;; esac; \
		if ! rm -rf "$(DESTDIR)$(SKILLS_DIR)/$$skill_name"; then \
			echo "error: cannot remove $(DESTDIR)$(SKILLS_DIR)/$$skill_name" >&2; status=1; \
		fi; \
	done < "$(DESTDIR)$(MANIFEST)"; \
	exit $$status
	@rm -f "$(DESTDIR)$(MANIFEST)"
	@case "$(DESTDIR)$(MANIFEST)" in \
		"$(DESTDIR)$(COMPONENT_DIR)"/*) \
			if [ -z "$$(ls -A "$(DESTDIR)$(COMPONENT_DIR)" 2>/dev/null | grep -vxF component.toml)" ]; then \
				rm -rf "$(DESTDIR)$(COMPONENT_DIR)"; \
			fi ;; \
	esac

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
	@echo "  MANIFEST= install receipt read by uninstall (default: COMPONENT_DIR/install-manifest)"
	@echo "  REPLACE_UNOWNED=1 to replace a same-name directory os-skills did not install (0 or unset refuses)"

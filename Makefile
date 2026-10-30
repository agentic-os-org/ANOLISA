# Repository orchestration; component roots own their build and delivery rules.
.DEFAULT_GOAL := help

ANOLISA_ROOT := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
ANOLISA_COMPONENTS := cosh-ng tokenless agent-sec-core agentsight skillfs ws-ckpt agent-memory
COMPONENTS ?=
AWPRJ ?= anolisa

selected_components = $(if $(strip $(COMPONENTS)),$(strip $(COMPONENTS)),$(ANOLISA_COMPONENTS))
make_quote = '$(subst ','"'"',$(1))'
component_arguments = $(call make_quote,AWPRJ=$(AWPRJ)) \
	$(if $(filter-out undefined,$(origin PREFIX)),$(call make_quote,PREFIX=$(PREFIX))) \
	$(if $(filter-out undefined,$(origin INSTALL_PROFILE)),$(call make_quote,INSTALL_PROFILE=$(INSTALL_PROFILE)))
component_names = $(foreach component,$(selected_components),$(call make_quote,$(component)))
test_layout_inputs := AWPRJ PREFIX INSTALL_PROFILE PROFILE DESTDIR BINDIR LIBDIR LIBEXECDIR DATADIR \
	ADAPTERDIR SKILLDIR UNITDIR DOCDIR EXTENSIONDIR BIN_DIR RTK_BIN BUILD_DIR BINARY ENFORCER_BIN \
	AGENTSIGHT_BIN SYSCONFDIR CONFIGDIR MCPDIR SHARE_DIR COSH_EXTENSION_DIR ADAPTER_DIR PLUGINS_DIR \
	RUNTIME_DIR RUNTIME_SKILLS_DIR COSH_SKILLS_DIR COMPONENT_DIR SYSTEMD_SYSTEM_DIR CONFIG_DIR \
	SECOND_SKILLDIR PLUGINDIR COMPONENTDIR SOURCE_DIR XDG_DATA_HOME

.PHONY: help list-components layout build install install-core install-plugins install-skills test-packaging check-components check-selection check-install-root

help:
	@printf '%s\n' \
		'ANOLISA repository orchestration' \
		'  make list-components' \
		'  make layout [COMPONENTS="cosh-ng tokenless"] [AWPRJ=anolisa] [PREFIX=/usr/local]' \
		'  make build COMPONENTS="cosh-ng tokenless"' \
		'  make install COMPONENTS="cosh-ng tokenless" [DESTDIR=/absolute/pkgroot]' \
		'  make install-core COMPONENTS="tokenless" [DESTDIR=/absolute/pkgroot]' \
		'  make install-plugins COMPONENTS="tokenless" [DESTDIR=/absolute/pkgroot]' \
		'  make install-skills COMPONENTS="agent-sec-core" [DESTDIR=/absolute/pkgroot]' \
		'  make test-packaging [COMPONENTS="cosh-ng tokenless"]' \
		'Component Makefiles remain directly callable with make -C src/<component>.' \
		'Build requires provisioned dependencies and a supported build host.' \
		'Components select their installation defaults; explicit PREFIX and INSTALL_PROFILE override them.' \
		'PREFIX selects executable locations; DESTDIR optionally adds a package root.' \
		'Install does not build artifacts, activate services or register host plugins.'

list-components:
	@printf '%s\n' $(ANOLISA_COMPONENTS)

check-components:
	@for component in $(component_names); do \
		case "$$component" in cosh-ng|tokenless|agent-sec-core|agentsight|skillfs|ws-ckpt|agent-memory) ;; \
			*) printf 'Unknown component: %s\n' "$$component" >&2; exit 1;; \
		esac; \
	done

check-selection: check-components
	@test -n $(call make_quote,$(strip $(COMPONENTS))) || { echo 'Set COMPONENTS explicitly for build or install' >&2; exit 1; }

check-install-root:
	@if [ -n $(call make_quote,$(DESTDIR)) ]; then \
		case $(call make_quote,$(DESTDIR)) in /*) ;; *) echo 'DESTDIR must be an absolute package root' >&2; exit 1;; esac; \
		test -n "$$(printf '%s' $(call make_quote,$(DESTDIR)) | tr -d /)" || { echo 'Use an empty DESTDIR for a direct installation' >&2; exit 1; }; \
		case $(call make_quote,/$(DESTDIR)/) in */../*|*/./*) echo 'DESTDIR must not contain dot segments' >&2; exit 1;; esac; \
	fi

layout: check-components
	@set -eu; for component in $(component_names); do \
		printf '[%s]\n' "$$component"; \
		$(MAKE) --no-print-directory -C "$(ANOLISA_ROOT)src/$$component" layout $(component_arguments); \
	done

# Fixture suites verify component defaults rather than repository composition.
test-packaging: MAKEOVERRIDES =
test-packaging: check-components
	@set -eu; for component in $(component_names); do \
		printf '[%s]\n' "$$component"; \
		env $(foreach name,$(test_layout_inputs),-u $(name)) -u MAKEOVERRIDES \
			$(MAKE) --no-print-directory -C "$(ANOLISA_ROOT)src/$$component" test-packaging; \
	done

build: check-selection
	@set -eu; for component in $(component_names); do \
		$(MAKE) --no-print-directory -C "$(ANOLISA_ROOT)src/$$component" build $(component_arguments); \
	done

install install-core install-plugins install-skills: check-selection check-install-root
	@set -eu; for component in $(component_names); do \
		$(MAKE) --no-print-directory -C "$(ANOLISA_ROOT)src/$$component" $@ \
			$(component_arguments) $(call make_quote,DESTDIR=$(DESTDIR)); \
	done

# Included by the component root Makefile; no separate public build entrypoint.
# Legacy file defaults must not leak into the public file installation.
delivery_input = $(if $(filter command line environment,$(origin $(1))),$($(1)),$(2))
DELIVERY_ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/..)
DELIVERY_AWPRJ := $(call delivery_input,AWPRJ,)
DELIVERY_INSTALL_PROFILE := $(call delivery_input,INSTALL_PROFILE,system)
DELIVERY_PREFIX := $(call delivery_input,PREFIX,/usr/local)
delivery_trim_slashes = $(if $(filter %/,$(1)),$(call delivery_trim_slashes,$(patsubst %/,%,$(1))),$(1))
DELIVERY_PREFIX_BASE := $(call delivery_trim_slashes,$(DELIVERY_PREFIX))
DELIVERY_DESTDIR := $(call delivery_input,DESTDIR,)
DELIVERY_NAMESPACE := $(if $(strip $(DELIVERY_AWPRJ)),$(DELIVERY_AWPRJ)/,)
DELIVERY_BINDIR := $(call delivery_input,BINDIR,$(DELIVERY_PREFIX_BASE)/bin)
DELIVERY_LIBDIR := $(call delivery_input,LIBDIR,$(DELIVERY_PREFIX_BASE)/lib/$(DELIVERY_NAMESPACE)agent-sec-core)
DELIVERY_LIBEXECDIR := $(call delivery_input,LIBEXECDIR,$(DELIVERY_PREFIX_BASE)/libexec/$(DELIVERY_NAMESPACE)agent-sec-core)
DELIVERY_DATADIR := $(call delivery_input,DATADIR,$(DELIVERY_PREFIX_BASE)/share/$(DELIVERY_NAMESPACE)agent-sec-core)
DELIVERY_ADAPTERDIR := $(call delivery_input,ADAPTERDIR,$(DELIVERY_DATADIR)/adapters)
DELIVERY_SKILLDIR_DEFAULT := $(if $(filter command line environment,$(origin DATADIR)),$(DELIVERY_DATADIR)/skills,$(if $(strip $(DELIVERY_AWPRJ)),$(DELIVERY_PREFIX_BASE)/share/$(DELIVERY_NAMESPACE)skills,$(DELIVERY_DATADIR)/skills))
DELIVERY_SKILLDIR := $(call delivery_input,SKILLDIR,$(DELIVERY_SKILLDIR_DEFAULT))
DELIVERY_SYSCONFDIR := $(call delivery_input,SYSCONFDIR,$(if $(filter /usr,$(DELIVERY_PREFIX_BASE)),/etc,$(DELIVERY_PREFIX_BASE)/etc))
DELIVERY_CONFIGDIR := $(call delivery_input,CONFIGDIR,$(DELIVERY_SYSCONFDIR)/agent-sec)
DELIVERY_UNITDIR := $(call delivery_input,UNITDIR,$(DELIVERY_PREFIX_BASE)/lib/systemd/system)
DELIVERY_BUILD_DIR := $(call delivery_input,BUILD_DIR,$(DELIVERY_ROOT)/target)
DELIVERY_BUILD_OUTPUT := $(abspath $(DELIVERY_BUILD_DIR))

DELIVERY_INSTALL_ARGS = INSTALL_PROFILE=system V2_BIN_BUILD_DIR="$(DELIVERY_BUILD_OUTPUT)/v2/bin" BUILD_DIR="$(DELIVERY_BUILD_OUTPUT)" DESTDIR="$(DELIVERY_DESTDIR)" \
	PREFIX="$(DELIVERY_PREFIX)" BINDIR="$(DELIVERY_BINDIR)" LIBDIR="$(DELIVERY_LIBDIR)" \
	LIBEXECDIR="$(DELIVERY_LIBEXECDIR)" SKILLDIR="$(DELIVERY_SKILLDIR)" \
	EXTENSIONDIR="$(DELIVERY_ADAPTERDIR)/cosh" \
	OPENCLAW_PLUGIN_DIR="$(DELIVERY_ADAPTERDIR)/openclaw" \
	HERMES_PLUGIN_DIR="$(DELIVERY_ADAPTERDIR)/hermes" \
	CODEX_PLUGIN_DIR="$(DELIVERY_ADAPTERDIR)/codex" \
	QODER_PLUGIN_DIR="$(DELIVERY_ADAPTERDIR)/qoder" \
	QWEN_CODE_EXTENSION_DIR="$(DELIVERY_ADAPTERDIR)/qwencode" \
	SYSTEMD_SYSTEM_UNIT_DIR="$(DELIVERY_UNITDIR)" SYSTEMD_SERVICE_BINDIR="$(DELIVERY_BINDIR)" \
	SYSTEM_SEC_CORE_CONFIG_DIR="$(DELIVERY_CONFIGDIR)" SYSTEMD_SKILLSEC_CONFIG="$(DELIVERY_CONFIGDIR)/skillsec.json"

.PHONY: delivery-validate-layout delivery-validate-install layout install test-packaging

# The public uninstall must remove the skills selected by the public installer.
uninstall: SKILLDIR := $(DELIVERY_SKILLDIR)

delivery-validate-layout:
	@test "$(DELIVERY_INSTALL_PROFILE)" = system || { echo 'Public install supports INSTALL_PROFILE=system only; use an explicit legacy installer for other profiles' >&2; exit 1; }
	@case "$(DELIVERY_AWPRJ)" in *[!a-zA-Z0-9._-]*|.|..) echo 'AWPRJ must be empty or a single namespace name' >&2; exit 1;; esac
	@for path in "$(DELIVERY_PREFIX)" "$(DELIVERY_BINDIR)" "$(DELIVERY_LIBDIR)" "$(DELIVERY_LIBEXECDIR)" "$(DELIVERY_DATADIR)" "$(DELIVERY_ADAPTERDIR)" "$(DELIVERY_SKILLDIR)" "$(DELIVERY_UNITDIR)" "$(DELIVERY_SYSCONFDIR)" "$(DELIVERY_CONFIGDIR)"; do \
		case "$$path" in /*) ;; *) echo "Expected an absolute system path: $$path" >&2; exit 1;; esac; \
		case "$$path" in *[!a-zA-Z0-9_./-]*|*/../*|*/./*|*/..|*/.) echo "Expected a normalized system path using letters, digits, _, ., / or -: $$path" >&2; exit 1;; esac; \
	done

layout: delivery-validate-layout ## Show the public build/install layout
	@printf '%s\n' 'INSTALL_PROFILE=system' 'AWPRJ=$(DELIVERY_AWPRJ)' 'PREFIX=$(DELIVERY_PREFIX)' 'DESTDIR=$(DELIVERY_DESTDIR)' 'BINDIR=$(DELIVERY_BINDIR)' \
		'LIBDIR=$(DELIVERY_LIBDIR)' 'LIBEXECDIR=$(DELIVERY_LIBEXECDIR)' 'DATADIR=$(DELIVERY_DATADIR)' \
		'ADAPTERDIR=$(DELIVERY_ADAPTERDIR)' 'SKILLDIR=$(DELIVERY_SKILLDIR)' 'UNITDIR=$(DELIVERY_UNITDIR)' \
		'SYSCONFDIR=$(DELIVERY_SYSCONFDIR)' 'CONFIGDIR=$(DELIVERY_CONFIGDIR)'

# Keep each role's complete preflight separate from its file-only copier.
DELIVERY_CORE_INPUTS = \
	$(foreach binary,$(V2_RUNTIME_BINARIES),--executable "$(DELIVERY_BUILD_OUTPUT)/v2/bin/$(binary)" --target-file "$(DELIVERY_BINDIR)/$(binary)") \
	--executable "$(DELIVERY_BUILD_OUTPUT)/linux-sandbox" \
	--file "$(DELIVERY_ROOT)/LICENSE" \
	--file "$(DELIVERY_ROOT)/packaging/systemd/agent-sec-core-v2.service.in" \
	--file "$(DELIVERY_ROOT)/packaging/systemd/skillsec.json" \
	--target-file "$(DELIVERY_BINDIR)/linux-sandbox" \
	--target-file "$(DELIVERY_UNITDIR)/agent-sec-core.service" \
	--target-file "$(DELIVERY_CONFIGDIR)/skillsec.json" \
	--target-file "$(DELIVERY_DATADIR)/LICENSE"
DELIVERY_PLUGIN_INPUTS = \
	--directory "$(DELIVERY_BUILD_OUTPUT)/cosh-extension" \
	--directory "$(DELIVERY_BUILD_OUTPUT)/codex-plugin" \
	--directory "$(DELIVERY_BUILD_OUTPUT)/qoder-plugin" \
	--directory "$(DELIVERY_BUILD_OUTPUT)/openclaw-plugin" \
	--directory "$(DELIVERY_BUILD_OUTPUT)/hermes-plugin" \
	--directory "$(DELIVERY_BUILD_OUTPUT)/qwen-code-extension" \
	--file "$(DELIVERY_BUILD_OUTPUT)/cosh-extension/cosh-extension.json" \
	--file "$(DELIVERY_BUILD_OUTPUT)/codex-plugin/install.sh" \
	--file "$(DELIVERY_BUILD_OUTPUT)/qoder-plugin/install.sh" \
	--glob "$(DELIVERY_BUILD_OUTPUT)/qoder-plugin/hooks/*.py" \
	--file "$(DELIVERY_BUILD_OUTPUT)/openclaw-plugin/openclaw.plugin.json" \
	--file "$(DELIVERY_BUILD_OUTPUT)/openclaw-plugin/package.json" \
	--file "$(DELIVERY_BUILD_OUTPUT)/openclaw-plugin/dist/index.js" \
	--file "$(DELIVERY_BUILD_OUTPUT)/openclaw-plugin/scripts/deploy.sh" \
	--file "$(DELIVERY_BUILD_OUTPUT)/hermes-plugin/src/plugin.yaml" \
	--file "$(DELIVERY_BUILD_OUTPUT)/hermes-plugin/scripts/deploy.sh" \
	--file "$(DELIVERY_BUILD_OUTPUT)/qwen-code-extension/scripts/deploy.sh" \
	--file "$(DELIVERY_BUILD_OUTPUT)/qwen-code-extension/hooks/observability_hook.py" \
	--file "$(DELIVERY_BUILD_OUTPUT)/qwen-code-extension/hooks/skill_ledger_hook.py" \
	--tree "$(DELIVERY_BUILD_OUTPUT)/cosh-extension" "$(DELIVERY_ADAPTERDIR)/cosh" \
	--tree "$(DELIVERY_BUILD_OUTPUT)/codex-plugin" "$(DELIVERY_ADAPTERDIR)/codex" \
	--tree "$(DELIVERY_BUILD_OUTPUT)/qoder-plugin" "$(DELIVERY_ADAPTERDIR)/qoder" \
	--tree "$(DELIVERY_BUILD_OUTPUT)/openclaw-plugin" "$(DELIVERY_ADAPTERDIR)/openclaw" \
	--tree "$(DELIVERY_BUILD_OUTPUT)/hermes-plugin" "$(DELIVERY_ADAPTERDIR)/hermes" \
	--tree "$(DELIVERY_BUILD_OUTPUT)/qwen-code-extension" "$(DELIVERY_ADAPTERDIR)/qwencode"
DELIVERY_SKILL_INPUTS = \
	--directory "$(DELIVERY_BUILD_OUTPUT)/skills" \
	--glob "$(DELIVERY_BUILD_OUTPUT)/skills/*/SKILL.md" \
	--tree "$(DELIVERY_BUILD_OUTPUT)/skills" "$(DELIVERY_SKILLDIR)"

.PHONY: delivery-validate-core delivery-validate-plugins delivery-validate-skills \
	delivery-install-core-files delivery-install-plugins-files delivery-install-skills-files \
	install-core install-plugins install-skills

delivery-validate-core: delivery-validate-layout
	@python3 "$(DELIVERY_ROOT)/packaging/validate-stage.py" --destdir "$(DELIVERY_DESTDIR)" $(DELIVERY_CORE_INPUTS)

delivery-validate-plugins: delivery-validate-layout
	@python3 "$(DELIVERY_ROOT)/packaging/validate-stage.py" --destdir "$(DELIVERY_DESTDIR)" $(DELIVERY_PLUGIN_INPUTS)

delivery-validate-skills: delivery-validate-layout
	@python3 "$(DELIVERY_ROOT)/packaging/validate-stage.py" --destdir "$(DELIVERY_DESTDIR)" $(DELIVERY_SKILL_INPUTS)

delivery-validate-install: delivery-validate-core delivery-validate-plugins delivery-validate-skills

delivery-install-core-files:
	$(MAKE) -C "$(DELIVERY_ROOT)" install-cli-v2-source install-sandbox install-systemd-system $(DELIVERY_INSTALL_ARGS)
	install -d -m 0755 "$(DELIVERY_DESTDIR)$(DELIVERY_DATADIR)"
	install -p -m 0644 "$(DELIVERY_ROOT)/LICENSE" "$(DELIVERY_DESTDIR)$(DELIVERY_DATADIR)/LICENSE"

delivery-install-plugins-files:
	$(MAKE) -C "$(DELIVERY_ROOT)" install-source-v2-cosh-extension install-codex-plugin install-qoder-plugin \
		install-openclaw-plugin install-hermes-plugin install-qwen-code-extension $(DELIVERY_INSTALL_ARGS)

delivery-install-skills-files:
	$(MAKE) -C "$(DELIVERY_ROOT)" install-skills-files $(DELIVERY_INSTALL_ARGS)

install-core: delivery-validate-core ## Install prebuilt V2 binaries, services, configuration and license
	$(MAKE) -C "$(DELIVERY_ROOT)" delivery-install-core-files

install-plugins: delivery-validate-plugins ## Install prebuilt host integrations without registration
	$(MAKE) -C "$(DELIVERY_ROOT)" delivery-install-plugins-files

install-skills: delivery-validate-skills ## Install built skills without modifying host configuration
	$(MAKE) -C "$(DELIVERY_ROOT)" delivery-install-skills-files

# All applicable inputs must pass before any role begins writing files.
install: delivery-validate-install ## Install all prebuilt V2 files; DESTDIR optionally adds a temporary root
	$(MAKE) -C "$(DELIVERY_ROOT)" delivery-install-core-files delivery-install-plugins-files delivery-install-skills-files

test-packaging: ## Verify public installation and RPM ownership with fixtures only
	python3 "$(DELIVERY_ROOT)/tests/v2/packaging/test_delivery.py"

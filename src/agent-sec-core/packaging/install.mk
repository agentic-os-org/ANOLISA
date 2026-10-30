# =============================================================================
# INSTALL
# =============================================================================

# --- Install profile: 'system' (RPM) or 'user' (source build) ----------------
INSTALL_PROFILE ?= system

ifeq ($(INSTALL_PROFILE),user)
  PREFIX          ?= $(HOME)/.local
  ANOLISA_DATADIR ?= $(PREFIX)/share/anolisa
  EXTENSIONDIR    ?= $(HOME)/.copilot-shell/extensions/agent-sec-core
  SKILLDIR        ?= $(HOME)/.copilot-shell/skills
  OPENCLAW_PLUGIN_DIR ?= $(LIBDIR)/openclaw-plugin
  HERMES_PLUGIN_DIR   ?= $(LIBDIR)/hermes-plugin
  QWEN_CODE_EXTENSION_DIR ?= $(LIBDIR)/qwen-code-extension
  CODEX_PLUGIN_DIR    ?= $(LIBDIR)/codex-plugin
  QODER_PLUGIN_DIR    ?= $(LIBDIR)/qoder-plugin
else
  PREFIX          ?= /usr/local
  ANOLISA_DATADIR ?= /usr/share/anolisa
  EXTENSIONDIR    ?= $(ANOLISA_DATADIR)/extensions/agent-sec-core
  SKILLDIR        ?= $(ANOLISA_DATADIR)/skills
  OPENCLAW_PLUGIN_DIR ?= $(LIBDIR)/openclaw-plugin
  HERMES_PLUGIN_DIR   ?= $(LIBDIR)/hermes-plugin
  QWEN_CODE_EXTENSION_DIR ?= $(LIBDIR)/qwen-code-extension
  CODEX_PLUGIN_DIR    ?= $(LIBDIR)/codex-plugin
  QODER_PLUGIN_DIR    ?= $(LIBDIR)/qoder-plugin
endif

COMPONENT_DIR       ?= $(ANOLISA_DATADIR)/components/sec-core

BINDIR              ?= $(PREFIX)/bin
LIBDIR              ?= $(PREFIX)/lib/anolisa/sec-core
LIBEXECDIR          ?= $(PREFIX)/libexec/anolisa/sec-core
VENV_DIR            ?= $(LIBDIR)/venv
SYSTEM_SEC_CORE_UV_PYTHON_INSTALL_DIR ?= $(LIBDIR)/uv-python
WHEEL_DIR           ?= $(LIBDIR)/wheels
CLI_STAGED_SITE     ?= $(BUILD_DIR)/site-packages
CLI_PRIVATE_SITE    ?= /opt/agent-sec/lib/python3.11/site-packages
RPM_OPENCLAW_PLUGIN_DIR ?= /opt/agent-sec/openclaw-plugin
RPM_CODEX_PLUGIN_DIR  ?= /opt/agent-sec/codex-plugin
RPM_HERMES_PLUGIN_DIR ?= /opt/agent-sec/hermes-plugin
RPM_QODER_PLUGIN_DIR  ?= /opt/agent-sec/qoder-plugin
RPM_QWEN_CODE_EXTENSION_DIR ?= /opt/agent-sec/qwen-code-extension
SYSTEMD_USER_UNIT_DIR ?= /usr/lib/systemd/user
SYSTEMD_USER_UNIT_TEMPLATE ?= packaging/systemd/agent-sec-core.service.in
SYSTEMD_SERVICE_BINDIR ?= /usr/bin
SYSTEMD_SERVICE_DATADIR ?= /usr/share/anolisa
SYSTEMD_SYSTEM_UNIT_DIR ?= /usr/lib/systemd/system
SYSTEMD_SYSTEM_UNIT_TEMPLATE ?= packaging/systemd/agent-sec-core-v2.service.in
SOURCE_SYSTEMD_SYSTEM_UNIT_DIR ?= /etc/systemd/system
SYSTEM_SEC_CORE_CONFIG_DIR ?= /etc/agent-sec
SYSTEMD_SKILLSEC_CONFIG ?=
SYSTEMD_SKILLSEC_CONFIG_ARGS = $(if $(SYSTEMD_SKILLSEC_CONFIG), --skillsec-config "$(SYSTEMD_SKILLSEC_CONFIG)")

.PHONY: install-sandbox
install-sandbox: ## Install linux-sandbox binary only
	install -d -m 0755 $(DESTDIR)$(BINDIR)
	install -p -m 0755 $(BUILD_DIR)/linux-sandbox $(DESTDIR)$(BINDIR)/

.PHONY: install-tool
install-tool: ## Install sign-skill.sh to LIBEXECDIR
	install -d -m 0755 $(DESTDIR)$(LIBEXECDIR)
	install -p -m 0755 $(BUILD_DIR)/tools/sign-skill.sh $(DESTDIR)$(LIBEXECDIR)/

.PHONY: install-legacy
install-legacy: INSTALL_PROFILE := system
install-legacy: install-all-source-v2 ## Install V2 artifacts with the legacy source layout

.PHONY: install-cli
install-cli: ## Install agent-sec-cli wheel (for dev/debug)
	@$(call require_single_cli_wheel,$(BUILD_DIR)/wheels) \
		pip3 install "$$1"

.PHONY: install-cli-site
install-cli-site: ## RPM: Copy staged site-packages to private dir + wrappers
	# 1. Copy all Python packages to private directory
	install -d -m 0755 $(DESTDIR)$(CLI_PRIVATE_SITE)
	cp -rp $(CLI_STAGED_SITE)/. $(DESTDIR)$(CLI_PRIVATE_SITE)/
	# Remove uv-generated bin/ and .lock from private site-packages
	rm -rf $(DESTDIR)$(CLI_PRIVATE_SITE)/bin
	rm -f  $(DESTDIR)$(CLI_PRIVATE_SITE)/.lock
	# 2. Install wrapper scripts as /usr/bin/agent-sec-cli and /usr/bin/agent-sec-daemon
	install -d -m 0755 $(DESTDIR)/usr/bin
	install -p -m 0755 scripts/agent-sec-cli-wrapper.sh $(DESTDIR)/usr/bin/agent-sec-cli
	install -p -m 0755 scripts/agent-sec-daemon-wrapper.sh $(DESTDIR)/usr/bin/agent-sec-daemon

V2_RUNTIME_BINARIES := agent-sec-cli agent-sec-daemon agent-sec-daemon-health

define install_v2_binaries
	install -d -m 0755 "$(DESTDIR)$(1)"
	install -p -m 0755 $(foreach binary,$(V2_RUNTIME_BINARIES),"$(V2_BIN_BUILD_DIR)/$(binary)") "$(DESTDIR)$(1)/"
endef

.PHONY: install-cli-v2
install-cli-v2: ## RPM: Install V2 Rust CLI and daemon binaries
	$(call install_v2_binaries,/usr/bin)

.PHONY: install-cli-v2-source
install-cli-v2-source: ## Install V2 Rust CLI and daemon binaries to BINDIR
	$(call install_v2_binaries,$(BINDIR))

.PHONY: install-cli-venv
install-cli-venv: ## Create venv, install deps from uv.lock, install wheel, symlink
	@echo "Creating Python venv at $(VENV_DIR) ..."
	install -d -m 0755 $(VENV_DIR)
ifeq ($(INSTALL_PROFILE),system)
	install -d -m 0755 $(SYSTEM_SEC_CORE_UV_PYTHON_INSTALL_DIR)
	UV_PYTHON_INSTALL_DIR=$(SYSTEM_SEC_CORE_UV_PYTHON_INSTALL_DIR) uv venv --python 3.11.6 --allow-existing $(VENV_DIR)
else
	uv venv --python 3.11.6 --allow-existing $(VENV_DIR)
endif
	@echo "Installing dependencies from uv.lock ..."
	cd agent-sec-cli && UV_PROJECT_ENVIRONMENT=$(VENV_DIR) uv sync --frozen --no-dev --no-install-project
	@echo "Installing agent-sec-cli wheel ..."
	@$(call require_single_cli_wheel,$(BUILD_DIR)/wheels) \
		uv pip install --python $(VENV_DIR)/bin/python --no-deps "$$1"
	@echo "Creating symlink ..."
	install -d -m 0755 $(BINDIR)
	ln -sf $(VENV_DIR)/bin/agent-sec-cli $(BINDIR)/agent-sec-cli
	ln -sf $(VENV_DIR)/bin/agent-sec-daemon $(BINDIR)/agent-sec-daemon

.PHONY: install-skills-files
install-skills-files: ## Install skill files to SKILLDIR
	install -d -m 0755 $(DESTDIR)$(SKILLDIR)
	cp -rp $(BUILD_DIR)/skills/. $(DESTDIR)$(SKILLDIR)/
	find "$(BUILD_DIR)/skills" -type f \( -name '*.sh' -o -name '*.py' \) \
		-exec sh -c 'source_root=$$1; target_root=$$2; shift 2; \
			for source do relative=$${source#"$$source_root"/}; chmod 0755 "$$target_root/$$relative" || exit; done' \
			sh "$(BUILD_DIR)/skills" "$(DESTDIR)$(SKILLDIR)" {} +

.PHONY: install-openclaw-plugin
install-openclaw-plugin: ## Install openclaw-plugin to target directory
	install -d -m 0755 $(DESTDIR)$(OPENCLAW_PLUGIN_DIR)
	install -d -m 0755 $(DESTDIR)$(OPENCLAW_PLUGIN_DIR)/dist
	install -d -m 0755 $(DESTDIR)$(OPENCLAW_PLUGIN_DIR)/scripts
	cp $(BUILD_DIR)/openclaw-plugin/openclaw.plugin.json $(DESTDIR)$(OPENCLAW_PLUGIN_DIR)/
	cp $(BUILD_DIR)/openclaw-plugin/package.json $(DESTDIR)$(OPENCLAW_PLUGIN_DIR)/
	cp -r $(BUILD_DIR)/openclaw-plugin/dist/* $(DESTDIR)$(OPENCLAW_PLUGIN_DIR)/dist/
	cp -r $(BUILD_DIR)/openclaw-plugin/scripts/* $(DESTDIR)$(OPENCLAW_PLUGIN_DIR)/scripts/
	chmod 0755 $(DESTDIR)$(OPENCLAW_PLUGIN_DIR)/scripts/*.sh

.PHONY: install-hermes-plugin
install-hermes-plugin: ## Install hermes-plugin to target directory
	install -d -m 0755 $(DESTDIR)$(HERMES_PLUGIN_DIR)/src
	install -d -m 0755 $(DESTDIR)$(HERMES_PLUGIN_DIR)/scripts
	cp -rp $(BUILD_DIR)/hermes-plugin/src/. $(DESTDIR)$(HERMES_PLUGIN_DIR)/src/
	cp -rp $(BUILD_DIR)/hermes-plugin/scripts/. $(DESTDIR)$(HERMES_PLUGIN_DIR)/scripts/
	chmod 0755 $(DESTDIR)$(HERMES_PLUGIN_DIR)/scripts/*.sh

.PHONY: install-qwen-code-extension
install-qwen-code-extension: ## Install Qwen Code extension to target directory
	install -d -m 0755 $(DESTDIR)$(QWEN_CODE_EXTENSION_DIR)
	cp -rp $(BUILD_DIR)/qwen-code-extension/. $(DESTDIR)$(QWEN_CODE_EXTENSION_DIR)/
	chmod 0755 $(DESTDIR)$(QWEN_CODE_EXTENSION_DIR)/scripts/*.sh
	chmod 0755 $(DESTDIR)$(QWEN_CODE_EXTENSION_DIR)/hooks/observability_hook.py
	chmod 0755 $(DESTDIR)$(QWEN_CODE_EXTENSION_DIR)/hooks/skill_ledger_hook.py

.PHONY: install-codex-plugin
install-codex-plugin: ## Install codex-plugin hooks and install script
	install -d -m 0755 $(DESTDIR)$(CODEX_PLUGIN_DIR)
	cp -rp $(BUILD_DIR)/codex-plugin/. $(DESTDIR)$(CODEX_PLUGIN_DIR)/
	chmod 0755 $(DESTDIR)$(CODEX_PLUGIN_DIR)/install.sh

.PHONY: install-qoder-plugin
install-qoder-plugin: ## Install qoder-plugin hooks and install script
	install -d -m 0755 $(DESTDIR)$(QODER_PLUGIN_DIR)
	cp -rp $(BUILD_DIR)/qoder-plugin/. $(DESTDIR)$(QODER_PLUGIN_DIR)/
	chmod 0755 $(DESTDIR)$(QODER_PLUGIN_DIR)/install.sh
	chmod 0755 $(DESTDIR)$(QODER_PLUGIN_DIR)/hooks/*.py

.PHONY: install-cosh-hook
install-cosh-hook: ## Install cosh hooks (linux-sandbox + extension)
	install -d -m 0755 $(DESTDIR)$(BINDIR)
	install -p -m 0755 $(BUILD_DIR)/linux-sandbox $(DESTDIR)$(BINDIR)/
	install -d -m 0755 $(DESTDIR)$(EXTENSIONDIR)
	cp -rp $(BUILD_DIR)/cosh-extension/. $(DESTDIR)$(EXTENSIONDIR)/

.PHONY: install-component-manifest
install-component-manifest: ## Install the selected component contract to DESTDIR
	install -d -m 0755 $(DESTDIR)$(COMPONENT_DIR)
	install -m 0644 $(COMPONENT_CONTRACT) \
		$(DESTDIR)$(COMPONENT_DIR)/component.toml

.PHONY: install-systemd-user
install-systemd-user: ## Install agent-sec-core systemd user service
	install -d -m 0755 $(DESTDIR)$(SYSTEMD_USER_UNIT_DIR)
	sed \
		-e 's|{bindir}|$(SYSTEMD_SERVICE_BINDIR)|g' \
		-e 's|{datadir}|$(SYSTEMD_SERVICE_DATADIR)|g' \
		$(SYSTEMD_USER_UNIT_TEMPLATE) \
		> $(DESTDIR)$(SYSTEMD_USER_UNIT_DIR)/agent-sec-core.service
	chmod 0644 $(DESTDIR)$(SYSTEMD_USER_UNIT_DIR)/agent-sec-core.service

.PHONY: install-core-v2
install-core-v2: install-cli-v2 install-systemd-system ## Install V2 core binaries and system unit; start explicitly

.PHONY: install-all
install-all: install-cli-venv install-cosh-hook install-codex-plugin install-qoder-plugin install-openclaw-plugin install-hermes-plugin install-qwen-code-extension install-skills-files ## Install all (user source build)

.PHONY: require-source-v2-artifacts
require-source-v2-artifacts: ## Fail unless every V2 source-install artifact has been built
	@for artifact in \
		"$(V2_BIN_BUILD_DIR)/agent-sec-cli" \
		"$(V2_BIN_BUILD_DIR)/agent-sec-daemon" \
		"$(V2_BIN_BUILD_DIR)/agent-sec-daemon-health" \
		"$(BUILD_DIR)/linux-sandbox"; do \
		if [ ! -f "$$artifact" ]; then \
			echo "ERROR: refusing to remove the V1 source runtime; missing V2 artifact: $$artifact" >&2; \
			echo "Run 'make build-all' first." >&2; \
			exit 1; \
		fi; \
	done

.PHONY: remove-legacy-source-v1-runtime
remove-legacy-source-v1-runtime: require-source-v2-artifacts ## Remove the V1 source-build CLI links, venv, and user sandbox
	@for name in agent-sec-cli agent-sec-daemon; do \
		path="$(DESTDIR)$(BINDIR)/$$name"; \
		if [ -L "$$path" ] && [ "$$(readlink "$$path")" = "$(VENV_DIR)/bin/$$name" ]; then \
			rm -f "$$path"; \
		fi; \
	done
	rm -f $(DESTDIR)$(BINDIR)/linux-sandbox
	rm -rf $(DESTDIR)$(VENV_DIR)

.PHONY: install-source-v2-host
install-source-v2-host: ## Install V2 host runtime for source builds
	@$(MAKE) remove-legacy-source-v1-runtime
	@$(MAKE) install-cli-v2-source install-sandbox install-systemd-system-source

.PHONY: install-source-v2-cosh-extension
install-source-v2-cosh-extension: ## Install the cosh extension for a V2 source build
	install -d -m 0755 $(DESTDIR)$(EXTENSIONDIR)
	cp -rp $(BUILD_DIR)/cosh-extension/. $(DESTDIR)$(EXTENSIONDIR)/

.PHONY: install-source-v2-assets
install-source-v2-assets: install-source-v2-cosh-extension install-codex-plugin install-qoder-plugin install-openclaw-plugin install-hermes-plugin install-qwen-code-extension install-skills-files ## Install source-build integration assets

.PHONY: install-all-source-v2
install-all-source-v2: install-source-v2-host install-source-v2-assets ## Install all V2 source-build artifacts

.PHONY: install-all-for-rpmbuild
install-all-for-rpmbuild: OPENCLAW_PLUGIN_DIR := $(RPM_OPENCLAW_PLUGIN_DIR)
install-all-for-rpmbuild: HERMES_PLUGIN_DIR := $(RPM_HERMES_PLUGIN_DIR)
install-all-for-rpmbuild: QWEN_CODE_EXTENSION_DIR := $(RPM_QWEN_CODE_EXTENSION_DIR)
install-all-for-rpmbuild: CODEX_PLUGIN_DIR := $(RPM_CODEX_PLUGIN_DIR)
install-all-for-rpmbuild: QODER_PLUGIN_DIR := $(RPM_QODER_PLUGIN_DIR)
install-all-for-rpmbuild: install-cli-site $(RPM_INSTALL_COMMON_TARGETS) install-systemd-user ## Install all (RPM build)

# V2 installs a system-scope unit; the V1 raw/RPM user unit remains separate.
.PHONY: install-all-for-rpmbuild-v2
install-all-for-rpmbuild-v2: BINDIR := /usr/bin
install-all-for-rpmbuild-v2: OPENCLAW_PLUGIN_DIR := $(RPM_OPENCLAW_PLUGIN_DIR)
install-all-for-rpmbuild-v2: HERMES_PLUGIN_DIR := $(RPM_HERMES_PLUGIN_DIR)
install-all-for-rpmbuild-v2: QWEN_CODE_EXTENSION_DIR := $(RPM_QWEN_CODE_EXTENSION_DIR)
install-all-for-rpmbuild-v2: CODEX_PLUGIN_DIR := $(RPM_CODEX_PLUGIN_DIR)
install-all-for-rpmbuild-v2: QODER_PLUGIN_DIR := $(RPM_QODER_PLUGIN_DIR)
install-all-for-rpmbuild-v2: install-cli-v2 $(filter-out install-component-manifest,$(RPM_INSTALL_COMMON_TARGETS)) install-systemd-system ## Install all (V2 RPM build)

.PHONY: install-systemd-system
install-systemd-system: ## Stage V2 system unit and preserve existing settings
	install -d -m 0755 $(DESTDIR)$(SYSTEMD_SYSTEM_UNIT_DIR)
	sed -e 's|{bindir}|$(SYSTEMD_SERVICE_BINDIR)|g' \
		-e 's|^\(ExecStart=.* serve\)$$|\1$(SYSTEMD_SKILLSEC_CONFIG_ARGS)|' $(SYSTEMD_SYSTEM_UNIT_TEMPLATE) \
		> $(DESTDIR)$(SYSTEMD_SYSTEM_UNIT_DIR)/agent-sec-core.service
	chmod 0644 $(DESTDIR)$(SYSTEMD_SYSTEM_UNIT_DIR)/agent-sec-core.service
	install -d -m 0755 $(DESTDIR)$(SYSTEM_SEC_CORE_CONFIG_DIR)
	@if [ ! -e "$(DESTDIR)$(SYSTEM_SEC_CORE_CONFIG_DIR)/skillsec.json" ] && \
		[ ! -L "$(DESTDIR)$(SYSTEM_SEC_CORE_CONFIG_DIR)/skillsec.json" ]; then \
		install -m 0600 packaging/systemd/skillsec.json \
			"$(DESTDIR)$(SYSTEM_SEC_CORE_CONFIG_DIR)/skillsec.json"; \
	fi

.PHONY: install-systemd-system-source
install-systemd-system-source: ## Install V2 system unit for source builds
	install -d -m 0755 $(DESTDIR)$(SOURCE_SYSTEMD_SYSTEM_UNIT_DIR)
	sed -e 's|{bindir}|$(BINDIR)|g' $(SYSTEMD_SYSTEM_UNIT_TEMPLATE) \
		> $(DESTDIR)$(SOURCE_SYSTEMD_SYSTEM_UNIT_DIR)/agent-sec-core.service
	chmod 0644 $(DESTDIR)$(SOURCE_SYSTEMD_SYSTEM_UNIT_DIR)/agent-sec-core.service

# =============================================================================
# UNINSTALL
# =============================================================================
# Mirrors install-all-source-v2 (source build). RPM uninstall is handled by the
# package manager via the .spec %files manifests, not by `make uninstall`.

.PHONY: uninstall-cli-venv
uninstall-cli-venv: ## Remove venv and agent-sec-cli symlink
	rm -f $(DESTDIR)$(BINDIR)/agent-sec-cli
	rm -f $(DESTDIR)$(BINDIR)/agent-sec-daemon
	rm -rf $(DESTDIR)$(VENV_DIR)
ifeq ($(INSTALL_PROFILE),system)
	rm -rf $(DESTDIR)$(SYSTEM_SEC_CORE_UV_PYTHON_INSTALL_DIR)
endif

.PHONY: uninstall-cli-site
uninstall-cli-site: ## Remove RPM-style private site-packages and wrappers
	rm -f $(DESTDIR)$(BINDIR)/agent-sec-cli
	rm -f $(DESTDIR)$(BINDIR)/agent-sec-daemon
	rm -rf $(DESTDIR)$(CLI_PRIVATE_SITE)

.PHONY: uninstall-cosh-hook
uninstall-cosh-hook: ## Remove linux-sandbox and cosh extension
	rm -f $(DESTDIR)$(BINDIR)/linux-sandbox
	rm -rf $(DESTDIR)$(EXTENSIONDIR)

.PHONY: uninstall-openclaw-plugin
uninstall-openclaw-plugin: ## Remove openclaw plugin
	rm -rf $(DESTDIR)$(OPENCLAW_PLUGIN_DIR)

.PHONY: uninstall-hermes-plugin
uninstall-hermes-plugin: ## Remove hermes plugin
	rm -rf $(DESTDIR)$(HERMES_PLUGIN_DIR)

.PHONY: uninstall-qwen-code-extension
uninstall-qwen-code-extension: ## Remove Qwen Code extension
	rm -rf $(DESTDIR)$(QWEN_CODE_EXTENSION_DIR)

.PHONY: uninstall-codex-plugin
uninstall-codex-plugin: ## Remove codex plugin
	rm -rf $(DESTDIR)$(CODEX_PLUGIN_DIR)

.PHONY: uninstall-qoder-plugin
uninstall-qoder-plugin: ## Remove qoder plugin
	rm -rf $(DESTDIR)$(QODER_PLUGIN_DIR)

.PHONY: uninstall-skills
uninstall-skills: ## Remove sec-core's own skill subdirs from SKILLDIR
	@for d in skills/*/; do \
		[ -d "$$d" ] || continue; \
		name=$$(basename "$$d"); \
		rm -rf "$(DESTDIR)$(SKILLDIR)/$$name"; \
	done

.PHONY: uninstall-component-manifest
uninstall-component-manifest: ## Remove component manifest
	rm -rf $(DESTDIR)$(COMPONENT_DIR)

.PHONY: uninstall
uninstall: INSTALL_PROFILE := system
uninstall: uninstall-all-source-v2 ## Uninstall all V2 source-build artifacts

.PHONY: uninstall-source-v2-host
uninstall-source-v2-host: uninstall-cli-v2-source uninstall-source-v2-sandbox uninstall-systemd-system-source ## Remove V2 source-build host runtime

.PHONY: uninstall-source-v2-sandbox
uninstall-source-v2-sandbox: ## Remove the V2 source-build sandbox binary
	rm -f $(DESTDIR)$(BINDIR)/linux-sandbox

.PHONY: uninstall-cli-v2-source
uninstall-cli-v2-source: ## Remove V2 source-build CLI and daemon binaries
	rm -f $(DESTDIR)$(BINDIR)/agent-sec-cli
	rm -f $(DESTDIR)$(BINDIR)/agent-sec-daemon
	rm -f $(DESTDIR)$(BINDIR)/agent-sec-daemon-health

.PHONY: uninstall-systemd-system-source
uninstall-systemd-system-source: ## Remove V2 source-build system unit
	rm -f $(DESTDIR)$(SOURCE_SYSTEMD_SYSTEM_UNIT_DIR)/agent-sec-core.service

.PHONY: uninstall-source-v2-cosh-extension
uninstall-source-v2-cosh-extension: ## Remove the V2 source-build cosh extension
	rm -rf $(DESTDIR)$(EXTENSIONDIR)

.PHONY: uninstall-source-v2-assets
uninstall-source-v2-assets: uninstall-source-v2-cosh-extension uninstall-codex-plugin uninstall-qoder-plugin uninstall-openclaw-plugin uninstall-hermes-plugin uninstall-qwen-code-extension uninstall-skills ## Remove source-build integration assets

.PHONY: uninstall-all-source-v2
uninstall-all-source-v2: uninstall-source-v2-host uninstall-source-v2-assets ## Remove all V2 source-build artifacts

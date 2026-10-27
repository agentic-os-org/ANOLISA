.PHONY: install-legacy
# Existing installation profiles and shared file-only installers.
# Install paths
# INSTALL_PROFILE selects the install layout, matching the contract used by
# the other Rust components (tokenless, ws-ckpt): in user mode every writable
# path is rooted at $(HOME)/.local so `make install-legacy` runs without root; in
# system mode paths go under /usr. DATADIR/SHARE_DIR derive from $(PREFIX) so
# they follow the profile instead of being hardcoded to /usr/share/anolisa
# (which caused Permission denied under INSTALL_PROFILE=user).
INSTALL_PROFILE ?= user

ifeq ($(INSTALL_PROFILE),user)
PREFIX ?= $(HOME)/.local
else
PREFIX ?= /usr
endif

BINDIR ?= $(PREFIX)/bin
DATADIR ?= $(PREFIX)/share/anolisa
DOCDIR ?= $(PREFIX)/share/doc/$(NAME)
SHARE_DIR ?= $(DATADIR)/adapters/agent-memory
CONFIGDIR ?= $(DATADIR)/agent-memory
MCPDIR ?= $(DATADIR)/mcp-servers
UNITDIR ?= /usr/lib/systemd/user
BINARY ?= target/release/$(NAME)
PLUGIN_PAYLOAD ?= source

# Adapter directories
ADAPTER_SRC_DIR := adapters/agent-memory
OPENCLAW_PLUGIN_SRC_DIR := $(ADAPTER_SRC_DIR)/openclaw
# Component contract (publishing/plugin metadata) — kept separate from the
# adapter payload tree so the contract is not mixed with adapter resources.
COMPONENT_CONTRACT := target/packaging/anolisa/rpm/component.toml
COMPONENT_CONTRACT_TEMPLATE := packaging/anolisa/rpm/component.toml.in


# =============================================================================
# INSTALL (source build)
# =============================================================================

# install-legacy: Install binary, config, and adapter. If release binary + plugin
# dist already exist (e.g., pre-built by CI/test fixture), skip the
# expensive build step to avoid timeout on clean ECS (cargo+npm takes
# >60s; pytest-timeout default is 60s). Set FORCE_BUILD=1 to override.
install-legacy: ## Preserve the historical build-and-install profile behavior
	@echo "==> Installing to $(DESTDIR)$(BINDIR)..."
	@if [ ! -f target/release/$(NAME) ] || [ "$(FORCE_BUILD)" = "1" ]; then \
		echo "==> Binary not found, building..."; \
		$(MAKE) build; \
	else \
		echo "==> Binary already built, skipping cargo+npm rebuild"; \
	fi
	@if [ ! -f $(ADAPTER_SRC_DIR)/openclaw/dist/index.js ] || [ "$(FORCE_BUILD)" = "1" ]; then \
		echo "==> Plugin dist not found, building adapter resources..."; \
		$(MAKE) install-adapter-resources; \
	else \
		echo "==> Adapter resources already built, skipping npm rebuild"; \
		$(MAKE) install-adapter-resources-staged; \
	fi
	$(MAKE) install-core-files

.PHONY: install-core-files
install-core-files: ## Install existing core files without building or activation
	install -d -m 0755 "$(DESTDIR)$(BINDIR)"
	install -p -m 0755 "$(BINARY)" "$(DESTDIR)$(BINDIR)/$(NAME)"
	install -d -m 0755 "$(DESTDIR)$(CONFIGDIR)"
	install -p -m 0644 config/default.toml "$(DESTDIR)$(CONFIGDIR)/"
	install -d -m 0755 "$(DESTDIR)$(MCPDIR)"
	python3 packaging/render-install.py mcp config/mcp-server.json "$(DESTDIR)$(MCPDIR)/agent-memory.json" "$(BINDIR)"
	chmod 0644 "$(DESTDIR)$(MCPDIR)/agent-memory.json"
	@echo "==> Installed $(NAME) to $(DESTDIR)$(BINDIR)/$(NAME)"

install-adapter-resources-staged: ## Install adapter bundle (no rebuild)
	@echo "==> Installing adapter resources to $(DESTDIR)$(SHARE_DIR)..."
ifeq ($(PLUGIN_PAYLOAD),runtime)
	python3 packaging/stage-adapter.py "$(ADAPTER_SRC_DIR)" "$(DESTDIR)$(SHARE_DIR)"
else ifeq ($(PLUGIN_PAYLOAD),source)
	rm -rf "$(DESTDIR)$(SHARE_DIR)"
	install -d -m 0755 "$(DESTDIR)$(SHARE_DIR)"
	cp -pr "$(ADAPTER_SRC_DIR)/." "$(DESTDIR)$(SHARE_DIR)/"
else
	$(error PLUGIN_PAYLOAD must be source or runtime)
endif
	@test -f "$(DESTDIR)$(SHARE_DIR)/openclaw/dist/index.js" \
	    || { echo "ERROR: $(DESTDIR)$(SHARE_DIR)/openclaw/dist/index.js missing after install-adapter-resources-staged"; exit 1; }

install-adapter-resources: build-openclaw-plugin ## Install adapter bundle per FHS spec
	$(MAKE) install-adapter-resources-staged

install-systemd-user: ## Install per-user systemd template (Linux only)
	@echo "==> Installing systemd template to $(UNITDIR)/..."
	install -d -m0755 "$(DESTDIR)$(UNITDIR)"
	python3 packaging/render-install.py systemd packaging/systemd/anolisa-memory@.service.in \
		"$(DESTDIR)$(UNITDIR)/anolisa-memory@.service" "$(BINDIR)"
	chmod 0644 "$(DESTDIR)$(UNITDIR)/anolisa-memory@.service"
	@echo "==> Enable with: systemctl --user enable --now anolisa-memory@$$USER"

uninstall: ## Remove installed files
	rm -f "$(DESTDIR)$(BINDIR)/$(NAME)"
	rm -f "$(DESTDIR)$(CONFIGDIR)/default.toml"
	rm -f "$(DESTDIR)$(MCPDIR)/agent-memory.json"
	rmdir "$(DESTDIR)$(CONFIGDIR)" 2>/dev/null || true
	rmdir "$(DESTDIR)$(MCPDIR)" 2>/dev/null || true

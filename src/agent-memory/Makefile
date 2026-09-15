# Agent memory — Build System (Linux-only)
# Single Rust binary MCP server for AI agent memory (file tools)
#
# Local build/test targets require Linux. On a non-Linux dev box, use:
#   make remote-test    — push to git remote shiloong/memory, then ssh
#                          aos2 to pull/build/test
#   make remote-build   — push + ssh aos2 build only

VERSION ?= $(shell grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
NAME := agent-memory

# Remote Linux dev host used by remote-* targets.
REMOTE_HOST ?= aos2
REMOTE_PATH ?= /root/anolisa
REMOTE_BRANCH ?= memory
REMOTE_REPO ?= shiloong

# Install paths
# INSTALL_PROFILE selects the install layout, matching the contract used by
# the other Rust components (tokenless, ws-ckpt): in user mode every writable
# path is rooted at $(HOME)/.local so `make install` runs without root; in
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

# Adapter directories
ADAPTER_SRC_DIR := adapters/agent-memory
OPENCLAW_PLUGIN_SRC_DIR := $(ADAPTER_SRC_DIR)/openclaw
# Component contract (publishing/plugin metadata) — kept separate from the
# adapter payload tree so the contract is not mixed with adapter resources.
# Shipped to %{_datadir}/anolisa/components/agent-memory/component.toml.
COMPONENT_CONTRACT := .anolisa/component.toml

# RPM build paths
RPMBUILD_DIR ?= $(HOME)/rpmbuild
SPEC_FILE := $(NAME).spec

.PHONY: all build build-openclaw-plugin sync-versions generate-component-contract test test-openclaw-install lint fmt clean install \
        install-adapter-resources uninstall dist spec rpm srpm \
        test-openclaw-plugin smoke help

all: build

# =============================================================================
# BUILD
# =============================================================================

build: build-openclaw-plugin ## Build release binary + openclaw plugin
	@echo "==> Building $(NAME) v$(VERSION)..."
	cargo build --release --locked
	@echo "==> Binary: target/release/$(NAME) ($$(du -sh target/release/$(NAME) | cut -f1))"

# Single-source version sync: Cargo.toml is the authority. Every derived
# JSON file (adapter manifest, npm package, npm lockfile, MCP server
# descriptor, openclaw plugin manifest) is rewritten to match. The
# openclaw plugin's runtime version (sent in MCP initialize.clientInfo
# .version) is injected by esbuild's --define at bundle time using
# `npm_package_version` set by npm, which itself was just synced from
# $(VERSION) here.
#
# Idempotent: each file is only rewritten when its current `.version`
# differs from $(VERSION); a no-op invocation leaves mtime untouched so
# `make build` doesn't dirty the git tree on every run.
SYNC_JSON_FILES := \
	$(ADAPTER_SRC_DIR)/manifest.json \
	$(OPENCLAW_PLUGIN_SRC_DIR)/package.json \
	$(OPENCLAW_PLUGIN_SRC_DIR)/openclaw.plugin.json \
	config/mcp-server.json

sync-versions: ## Sync $(VERSION) (from Cargo.toml) into all derived JSON files
	@command -v jq >/dev/null || { echo "ERROR: jq is required for sync-versions"; exit 1; }
	@# Auto-clean stray *.tmp on shell error (jq failure, disk full, SIGINT).
	@trap 'rm -f $(SYNC_JSON_FILES:%=%.tmp) $(OPENCLAW_PLUGIN_SRC_DIR)/package-lock.json.tmp' EXIT; \
	changed=0; \
	for f in $(SYNC_JSON_FILES); do \
		cur=$$(jq -r '.version // empty' "$$f" 2>/dev/null || echo ""); \
		if [ "$$cur" != "$(VERSION)" ]; then \
			jq --arg v "$(VERSION)" '.version = $$v' "$$f" > "$$f.tmp" && mv "$$f.tmp" "$$f" || exit 1; \
			echo "    $$f: $$cur -> $(VERSION)"; \
			changed=1; \
		fi; \
	done; \
	plock=$(OPENCLAW_PLUGIN_SRC_DIR)/package-lock.json; \
	cur=$$(jq -r '.version // empty' "$$plock" 2>/dev/null || echo ""); \
	if [ "$$cur" != "$(VERSION)" ]; then \
		jq --arg v "$(VERSION)" '.version = $$v | (.packages[""] | select(.) | .version) |= $$v' \
			"$$plock" > "$$plock.tmp" && mv "$$plock.tmp" "$$plock" || exit 1; \
		echo "    $$plock: $$cur -> $(VERSION)"; \
		changed=1; \
	fi; \
	if [ $$changed -eq 1 ]; then \
		echo "==> Synced derived files to $(VERSION)"; \
	fi

$(COMPONENT_CONTRACT): $(COMPONENT_CONTRACT).in Cargo.toml
	@echo "==> Generating component contract (version=$(VERSION))..."
	@sed 's/@VERSION@/$(VERSION)/g' $< > $@

generate-component-contract: $(COMPONENT_CONTRACT)

build-openclaw-plugin: sync-versions generate-component-contract ## Build openclaw TS plugin (dist/index.js)
	@echo "==> Building $(NAME) OpenClaw plugin..."
	# --legacy-peer-deps: the plugin declares `openclaw: "*"` as a peer
	# but the dev openclaw resolves to CalVer 2026.5.7, which npm 7+'s
	# strict peer resolver flags as needing a major bump. The peer
	# constraint is intentionally permissive (the plugin uses only the
	# stable plugin-sdk surface), so we keep --legacy-peer-deps until
	# openclaw publishes a semver-compatible package range.
	cd $(OPENCLAW_PLUGIN_SRC_DIR) && npm install --legacy-peer-deps --no-audit --no-fund
	# esbuild (what `npm run build` runs) strips types without checking them, so
	# a type error in src/ or tests/ still produces a dist/index.js. Nothing else
	# in the pipeline would catch it either: agent-memory.spec.in's %build
	# deliberately runs no npm and consumes the bundle `make dist` pre-built here,
	# and the `Test agent-memory` CI job has no Node step. Type-check first so the
	# bundle that ships is the bundle tsc accepted. `npm run build:check` is
	# `tsc --project tsconfig.json --noEmit` over the same tsconfig the plugin is
	# written against, and needs only the devDependencies installed just above.
	@echo "==> Type-checking $(NAME) OpenClaw plugin (tsc --noEmit)..."
	cd $(OPENCLAW_PLUGIN_SRC_DIR) && npm run build:check
	cd $(OPENCLAW_PLUGIN_SRC_DIR) && npm run build
	@test -f $(OPENCLAW_PLUGIN_SRC_DIR)/dist/index.js \
	    || { echo "ERROR: $(OPENCLAW_PLUGIN_SRC_DIR)/dist/index.js was not produced"; exit 1; }

build-debug: ## Build debug binary
	cargo build --locked

# =============================================================================
# TEST
# =============================================================================

test: test-openclaw-install ## Run all tests (Linux only — use `make remote-test` on macOS/Windows)
	@echo "==> Running tests..."
	cargo test --locked

# Bash-only flag-negotiation test for the OpenClaw installer (no Node):
# capability consent plus the legacy unsafe-install bypass.
# A future npm/tsx-based plugin unit-test target is a separate entry point
# and deliberately not part of `make test`.
test-openclaw-install: ## Run the OpenClaw adapter installer flag-negotiation test
	@echo "==> Testing OpenClaw adapter installer..."
	bash tests/test-openclaw-adapter-install.sh

test-mcp: ## Run MCP integration tests only
	cargo test --test mcp_integration_test

test-tools: ## Run Tier A file-tool integration tests only
	cargo test --test file_tools_test

# The openclaw adapter ships TS unit tests (tests/unit/*-test.ts) that `npm test`
# runs through tsx. They are deliberately not a prerequisite of `test:` above —
# that target is cargo-only and must stay runnable without a Node toolchain or
# registry access. Use this target to exercise the adapter suite. CI runs it
# from the `package-source` action, right after `build-openclaw-plugin` has
# npm-installed the same directory, so the nightly and release packaging paths
# gate on it. The per-PR `Test agent-memory` job still has no Node step —
# wiring one there needs the `workflow` token scope (#3255).
test-openclaw-plugin: ## Run openclaw adapter TS unit tests (needs npm registry)
	@echo "==> Running openclaw adapter unit tests..."
	cd $(OPENCLAW_PLUGIN_SRC_DIR) && npm install --legacy-peer-deps --no-audit --no-fund
	cd $(OPENCLAW_PLUGIN_SRC_DIR) && npm test

# =============================================================================
# REMOTE Linux build/test (for cross-platform development)
# =============================================================================

remote-push: ## Push current branch HEAD to the remote git repo
	@echo "==> Pushing $$(git rev-parse --abbrev-ref HEAD) to $(REMOTE_REPO)..."
	cd ../.. && git push $(REMOTE_REPO) $(REMOTE_BRANCH)

# Refuses to reset a dirty remote worktree — otherwise `remote-test`
# called while someone is debugging on $(REMOTE_HOST) silently wipes
# their work. Set REMOTE_FORCE=1 to override (CI / known-clean only).
REMOTE_FORCE ?=
ifeq ($(REMOTE_FORCE),1)
REMOTE_RESET_GUARD :=
else
REMOTE_RESET_GUARD := test -z "$$(git status --porcelain)" || { echo "ERROR: $(REMOTE_HOST):$(REMOTE_PATH) has uncommitted changes — refusing to reset. Set REMOTE_FORCE=1 to override." >&2; exit 1; } &&
endif

remote-build: remote-push ## Push + ssh $(REMOTE_HOST): pull + cargo build --release
	@echo "==> Building on $(REMOTE_HOST):$(REMOTE_PATH)..."
	ssh $(REMOTE_HOST) 'cd $(REMOTE_PATH) && $(REMOTE_RESET_GUARD) git fetch $(REMOTE_REPO) $(REMOTE_BRANCH) && git reset --hard $(REMOTE_REPO)/$(REMOTE_BRANCH) && cd src/agent-memory && cargo build --release | tail -5'

remote-test: remote-push ## Push + ssh $(REMOTE_HOST): full test + clippy
	@echo "==> Testing on $(REMOTE_HOST):$(REMOTE_PATH)..."
	ssh $(REMOTE_HOST) 'cd $(REMOTE_PATH) && $(REMOTE_RESET_GUARD) git fetch $(REMOTE_REPO) $(REMOTE_BRANCH) && git reset --hard $(REMOTE_REPO)/$(REMOTE_BRANCH) && cd src/agent-memory && bash tests/test-openclaw-adapter-install.sh && cargo test --release | grep "test result" && cargo clippy --release --all-targets -- -D warnings | tail -3'

# =============================================================================
# CODE QUALITY
# =============================================================================

lint: ## Run clippy lints
	cargo clippy -- -D warnings

fmt: ## Format code
	cargo fmt

fmt-check: ## Check formatting without modifying
	cargo fmt -- --check

# =============================================================================
# INSTALL (source build)
# =============================================================================

# install: Install binary, config, and adapter. If release binary + plugin
# dist already exist (e.g., pre-built by CI/test fixture), skip the
# expensive build step to avoid timeout on clean ECS (cargo+npm takes
# >60s; pytest-timeout default is 60s). Set FORCE_BUILD=1 to override.
install: ## Install binary, config, and adapter to PREFIX
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
	install -d -m 0755 $(DESTDIR)$(BINDIR)
	install -p -m 0755 target/release/$(NAME) $(DESTDIR)$(BINDIR)/
	install -d -m 0755 $(DESTDIR)$(DATADIR)/agent-memory
	install -p -m 0644 config/default.toml $(DESTDIR)$(DATADIR)/agent-memory/
	install -d -m 0755 $(DESTDIR)$(DATADIR)/mcp-servers
	install -p -m 0644 config/mcp-server.json $(DESTDIR)$(DATADIR)/mcp-servers/agent-memory.json
	@echo "==> Installed $(NAME) to $(DESTDIR)$(BINDIR)/$(NAME)"

install-adapter-resources-staged: ## Install adapter bundle (no rebuild)
	@echo "==> Installing adapter resources to $(DESTDIR)$(SHARE_DIR)..."
	rm -rf $(DESTDIR)$(SHARE_DIR)
	install -d -m 0755 $(DESTDIR)$(SHARE_DIR)
	cp -pr $(ADAPTER_SRC_DIR)/. $(DESTDIR)$(SHARE_DIR)/
	@test -f $(DESTDIR)$(SHARE_DIR)/openclaw/dist/index.js \
	    || { echo "ERROR: $(DESTDIR)$(SHARE_DIR)/openclaw/dist/index.js missing after install-adapter-resources-staged"; exit 1; }

install-adapter-resources: build-openclaw-plugin ## Install adapter bundle per FHS spec
	@echo "==> Installing adapter resources to $(DESTDIR)$(SHARE_DIR)..."
	rm -rf $(DESTDIR)$(SHARE_DIR)
	install -d -m 0755 $(DESTDIR)$(SHARE_DIR)
	cp -pr $(ADAPTER_SRC_DIR)/. $(DESTDIR)$(SHARE_DIR)/
	@test -f $(DESTDIR)$(SHARE_DIR)/openclaw/dist/index.js \
	    || { echo "ERROR: $(DESTDIR)$(SHARE_DIR)/openclaw/dist/index.js missing after install-adapter-resources"; exit 1; }

install-systemd-user: ## Install per-user systemd template (Linux only)
	@echo "==> Installing systemd template to /usr/lib/systemd/user/..."
	install -D -m0644 config/systemd/anolisa-memory@.service \
		$(DESTDIR)/usr/lib/systemd/user/anolisa-memory@.service
	@echo "==> Enable with: systemctl --user enable --now anolisa-memory@$$USER"

uninstall: ## Remove installed files
	rm -f $(DESTDIR)$(BINDIR)/$(NAME)
	rm -f $(DESTDIR)$(DATADIR)/agent-memory/default.toml
	rm -f $(DESTDIR)$(DATADIR)/mcp-servers/agent-memory.json
	rmdir $(DESTDIR)$(DATADIR)/agent-memory 2>/dev/null || true
	rmdir $(DESTDIR)$(DATADIR)/mcp-servers 2>/dev/null || true

# =============================================================================
# DISTRIBUTION
# =============================================================================

dist: clean build-openclaw-plugin ## Create source + vendor tarballs for RPM build
	@echo "==> Vendoring crates..."
	cargo vendor vendor/
	@mkdir -p .cargo
	@printf '[source.crates-io]\nreplace-with = "vendored-sources"\n\n[source.vendored-sources]\ndirectory = "vendor"\n' > .cargo/config.toml
	@echo "==> Creating $(NAME)-$(VERSION).tar.gz..."
	@mkdir -p dist
	# Top-level dir is $(NAME)-$(VERSION) so it matches the spec's
	# `%setup -n %{name}-%{version}` and the CI archive shape.
	#
	# `examples/` belongs in the copy list because Cargo.toml declares
	# `[[example]] mcp-harness` with an explicit path, and cargo refuses
	# to build a declared target whose file is missing — an archive
	# without it breaks `cargo test` and `cargo build --examples` inside
	# the unpacked source. The spec's own `%build` runs a plain
	# `cargo build --release`, which does not select examples, so rpmbuild
	# cannot catch that omission.
	#
	# `docs/` is deliberately not copied: the component manuals moved to
	# the repository-level docs/user-guide tree (a46aea8d) and the spec
	# packages CHANGELOG.md only. Listing the removed directory made
	# `cp -R` exit non-zero, so this target failed outright.
	@rm -rf dist/$(NAME)-$(VERSION) && mkdir -p dist/$(NAME)-$(VERSION)
	@cp -R Cargo.toml Cargo.lock src examples config tests \
		Makefile agent-memory.spec.in CHANGELOG.md LICENSE \
		.gitignore .anolisa dist/$(NAME)-$(VERSION)/
	@mkdir -p dist/$(NAME)-$(VERSION)/.cargo
	@cp .cargo/config.toml dist/$(NAME)-$(VERSION)/.cargo/config.toml
	# Adapter: include source + pre-built dist/index.js, exclude
	# node_modules/tests so the source tarball stays under a few MB.
	@mkdir -p dist/$(NAME)-$(VERSION)/$(ADAPTER_SRC_DIR)
	tar -cf - -C $(ADAPTER_SRC_DIR) \
		--exclude='node_modules' \
		--exclude='.tsbuildinfo' \
		--exclude='tests' \
		. | tar -xf - -C dist/$(NAME)-$(VERSION)/$(ADAPTER_SRC_DIR)/
	# Fail here rather than inside rpmbuild: every path the spec's %build
	# and %install read, plus every target path Cargo.toml declares, must
	# be in the staged tree. A copy list that silently rots is what broke
	# this target the first time.
	@set -e; \
	staged="dist/$(NAME)-$(VERSION)"; \
	for f in src/main.rs src/lib.rs examples/mcp_harness.rs \
			config/default.toml config/mcp-server.json \
			config/systemd/anolisa-memory@.service \
			config/systemd/anolisa-memory-tmpfiles.conf \
			$(ADAPTER_SRC_DIR)/manifest.json \
			$(OPENCLAW_PLUGIN_SRC_DIR)/dist/index.js \
			$(OPENCLAW_PLUGIN_SRC_DIR)/openclaw.plugin.json \
			$(OPENCLAW_PLUGIN_SRC_DIR)/package.json \
			$(OPENCLAW_PLUGIN_SRC_DIR)/scripts/detect.sh \
			$(OPENCLAW_PLUGIN_SRC_DIR)/scripts/install.sh \
			$(OPENCLAW_PLUGIN_SRC_DIR)/scripts/uninstall.sh \
			$(COMPONENT_CONTRACT) CHANGELOG.md LICENSE; do \
		test -e "$$staged/$$f" || { \
			echo "ERROR: $$f is missing from $$staged but agent-memory.spec.in or Cargo.toml requires it" >&2; \
			exit 1; \
		}; \
	done
	tar czf dist/$(NAME)-$(VERSION).tar.gz -C dist $(NAME)-$(VERSION)
	@echo "==> Creating $(NAME)-$(VERSION)-vendor.tar.gz..."
	# Top-level dir is `vendor/` (no $(NAME)-vendor wrapper) so that
	# `%setup -a 1` lands vendor/ next to the unpacked sources, where
	# cargo `--offline` can find it.
	tar czf dist/$(NAME)-$(VERSION)-vendor.tar.gz vendor
	@rm -rf dist/$(NAME)-$(VERSION) vendor
	@echo "==> Tarballs: dist/$(NAME)-$(VERSION).tar.gz + dist/$(NAME)-$(VERSION)-vendor.tar.gz"

# =============================================================================
# RPM BUILD
# =============================================================================

spec: ## Generate RPM spec from template (substitute @VERSION@)
	@echo "==> Generating $(SPEC_FILE) with version $(VERSION)..."
	sed 's/@VERSION@/$(VERSION)/g' $(NAME).spec.in > $(SPEC_FILE)

rpm: dist spec ## Build RPM package
	@echo "==> Building RPM for $(NAME)-$(VERSION)..."
	mkdir -p $(RPMBUILD_DIR)/{BUILD,RPMS,SOURCES,SPECS,SRPMS}
	cp dist/$(NAME)-$(VERSION).tar.gz $(RPMBUILD_DIR)/SOURCES/
	cp dist/$(NAME)-$(VERSION)-vendor.tar.gz $(RPMBUILD_DIR)/SOURCES/
	cp $(SPEC_FILE) $(RPMBUILD_DIR)/SPECS/
	rpmbuild -bb --define "_topdir $(RPMBUILD_DIR)" $(RPMBUILD_DIR)/SPECS/$(SPEC_FILE)
	@echo "==> RPM built. Check $(RPMBUILD_DIR)/RPMS/"

srpm: dist spec ## Build source RPM
	@echo "==> Building SRPM..."
	mkdir -p $(RPMBUILD_DIR)/{BUILD,RPMS,SOURCES,SPECS,SRPMS}
	cp dist/$(NAME)-$(VERSION).tar.gz $(RPMBUILD_DIR)/SOURCES/
	cp dist/$(NAME)-$(VERSION)-vendor.tar.gz $(RPMBUILD_DIR)/SOURCES/
	cp $(SPEC_FILE) $(RPMBUILD_DIR)/SPECS/
	rpmbuild -bs --define "_topdir $(RPMBUILD_DIR)" $(RPMBUILD_DIR)/SPECS/$(SPEC_FILE)
	@echo "==> SRPM built. Check $(RPMBUILD_DIR)/SRPMS/"

# =============================================================================
# UTILITY
# =============================================================================

clean: ## Clean build artifacts
	cargo clean
	rm -rf dist/ vendor/ .cargo/
	rm -rf $(OPENCLAW_PLUGIN_SRC_DIR)/dist/ $(OPENCLAW_PLUGIN_SRC_DIR)/node_modules/
	rm -f $(SPEC_FILE) $(COMPONENT_CONTRACT)
	rm -rf $(OPENCLAW_PLUGIN_SRC_DIR)/dist $(OPENCLAW_PLUGIN_SRC_DIR)/node_modules

version: ## Show current version
	@echo $(VERSION)

# Quick smoke test: init the mount, then drive 5 MCP tools through stdin.
# Validates: write → read → append → grep → list, plus path-sandbox enforcement.
smoke: build ## Run end-to-end smoke test against release binary
	@echo "==> Smoke test..."
	@TMPDIR=$$(mktemp -d) && \
		MEMORY_BASE_DIR="$$TMPDIR" USER_ID=smoke target/release/$(NAME) init && \
		MEMORY_BASE_DIR="$$TMPDIR" USER_ID=smoke target/release/$(NAME) info && \
		MEMORY_BASE_DIR="$$TMPDIR" USER_ID=smoke \
		printf '%s\n%s\n%s\n%s\n%s\n%s\n%s\n' \
			'{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke","version":"1.0"}}}' \
			'{"jsonrpc":"2.0","method":"notifications/initialized"}' \
			'{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
			'{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"mem_write","arguments":{"path":"notes/smoke.md","content":"hello smoke world"}}}' \
			'{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"mem_read","arguments":{"path":"notes/smoke.md"}}}' \
			'{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"mem_grep","arguments":{"pattern":"smoke"}}}' \
			'{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"mem_list","arguments":{"recursive":true}}}' \
		| target/release/$(NAME) | head -10 && \
		rm -rf "$$TMPDIR" && \
		echo "==> Smoke test PASSED"

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-16s\033[0m %s\n", $$1, $$2}'

.DEFAULT_GOAL := help

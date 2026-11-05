# Agent memory — Build System (Linux-only)
# Single Rust binary MCP server for AI agent memory (file tools)
#
# Local build/test targets require Linux. On a non-Linux dev box, use:
#   make remote-test    — push to git remote shiloong/memory, then ssh
#                          aos2 to pull/build/test
#   make remote-build   — push + ssh aos2 build only

VERSION ?= $(shell grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
NAME := agent-memory
SMOKE_TIMEOUT ?= 30
NPM_INSTALL ?= install
PLUGIN_BUILD_ARGS ?=

# Remote Linux dev host used by remote-* targets.
REMOTE_HOST ?= aos2
REMOTE_PATH ?= /root/anolisa
REMOTE_BRANCH ?= memory
REMOTE_REPO ?= shiloong

include packaging/install.mk
include packaging/release.mk
include packaging/rpm.mk
include packaging/delivery.mk


.PHONY: all build build-openclaw-plugin sync-versions generate-component-contract test test-openclaw-install lint fmt clean install \
        install-adapter-resources uninstall dist spec rpm srpm \
        test-openclaw-plugin test-auto-capture smoke help

all: build

# =============================================================================
# BUILD
# =============================================================================

build: build-openclaw-plugin build-core ## Build release binary + openclaw plugin

.PHONY: build-core
build-core: ## Build only the Rust binary
	@echo "==> Building $(NAME) v$(VERSION)..."
	cargo build --release --locked
	@echo "==> Binary: target/release/$(NAME) ($$(du -sh target/release/$(NAME) | cut -f1))"

build-openclaw-plugin: sync-versions ## Build openclaw TS plugin (dist/index.js)
	@echo "==> Building $(NAME) OpenClaw plugin..."
	# --legacy-peer-deps: the plugin declares `openclaw: "*"` as a peer
	# but the dev openclaw resolves to CalVer 2026.5.7, which npm 7+'s
	# strict peer resolver flags as needing a major bump. The peer
	# constraint is intentionally permissive (the plugin uses only the
	# stable plugin-sdk surface), so we keep --legacy-peer-deps until
	# openclaw publishes a semver-compatible package range.
	cd $(OPENCLAW_PLUGIN_SRC_DIR) && npm $(NPM_INSTALL) --legacy-peer-deps --no-audit --no-fund
	# esbuild (what `npm run build` runs) strips types without checking them, so
	# a type error in src/ or tests/ still produces a dist/index.js. Nothing else
	# in the pipeline would catch it either: packaging/rpm/agent-memory.spec.in's %build
	# deliberately runs no npm and consumes the bundle `make dist` pre-built here,
	# while the PR CI job runs the plugin unit suite. Type-check first so the
	# bundle that ships is the bundle tsc accepted. `npm run build:check` is
	# `tsc --project tsconfig.json --noEmit` over the same tsconfig the plugin is
	# written against, and needs only the devDependencies installed just above.
	@echo "==> Type-checking $(NAME) OpenClaw plugin (tsc --noEmit)..."
	cd $(OPENCLAW_PLUGIN_SRC_DIR) && npm run build:check
	cd $(OPENCLAW_PLUGIN_SRC_DIR) && npm run build -- $(PLUGIN_BUILD_ARGS)
	@test -f $(OPENCLAW_PLUGIN_SRC_DIR)/dist/index.js \
	    || { echo "ERROR: $(OPENCLAW_PLUGIN_SRC_DIR)/dist/index.js was not produced"; exit 1; }

.PHONY: build-openclaw-plugin-ci
build-openclaw-plugin-ci: ## Build the plugin from locked dependencies without version repair
	python3 packaging/raw/verify-release.py .
	$(MAKE) build-openclaw-plugin NPM_INSTALL=ci PLUGIN_BUILD_ARGS=--metafile=dist/build-meta.json

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
# The npm/tsx-based plugin unit-test target is a separate entry point
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
# gate on it. The per-PR `Test agent-memory` job also runs this target.
test-openclaw-plugin: ## Run openclaw adapter TS unit tests (needs npm registry)
	@echo "==> Running openclaw adapter unit tests..."
	cd $(OPENCLAW_PLUGIN_SRC_DIR) && npm $(NPM_INSTALL) --legacy-peer-deps --no-audit --no-fund
	cd $(OPENCLAW_PLUGIN_SRC_DIR) && npm test

# Run after test-openclaw-plugin has installed the pinned adapter dependencies.
test-auto-capture: build-debug ## Exercise the OpenClaw hook against the real MCP binary
	@test -x $(OPENCLAW_PLUGIN_SRC_DIR)/node_modules/.bin/tsx || { echo "Run make test-openclaw-plugin first to install adapter dependencies." >&2; exit 1; }
	AGENT_MEMORY_TEST_BINARY="$(abspath $(or $(CARGO_TARGET_DIR),target))/debug/$(NAME)" npm --prefix $(OPENCLAW_PLUGIN_SRC_DIR) run test:auto-capture

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

clean: ## Clean build artifacts
	cargo clean
	rm -rf dist/ vendor/ .cargo/
	rm -rf $(OPENCLAW_PLUGIN_SRC_DIR)/dist/ $(OPENCLAW_PLUGIN_SRC_DIR)/node_modules/
	rm -f $(SPEC_FILE) $(COMPONENT_CONTRACT)
	rm -rf $(OPENCLAW_PLUGIN_SRC_DIR)/dist $(OPENCLAW_PLUGIN_SRC_DIR)/node_modules

version: ## Show current version
	@echo $(VERSION)

# Check the release server's MCP responses in an isolated userland namespace.
smoke: build ## Run end-to-end smoke test against release binary
	python3 tests/smoke.py target/release/$(NAME) --timeout $(SMOKE_TIMEOUT)

help: ## Show this help
	@grep -h -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-16s\033[0m %s\n", $$1, $$2}'

.DEFAULT_GOAL := help

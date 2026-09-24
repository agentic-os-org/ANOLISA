# =============================================================================
# BUILD
# =============================================================================

LIBBPF_SYS_LIBRARY_PATH ?= $(shell pkg-config --variable=libdir libbpf 2>/dev/null || echo /usr/lib64:/usr/lib)
NPM_REGISTRY ?= https://registry.npmmirror.com
NPM_REPLACE_REGISTRY_HOST ?= always

.PHONY: build
build: ## Build agentsight binary
	env -u DESTDIR -u MAKEFLAGS -u MFLAGS -u MAKEOVERRIDES \
		LIBBPF_SYS_LIBRARY_PATH="$(LIBBPF_SYS_LIBRARY_PATH)" \
		cargo build --release

.PHONY: build-enforcer
build-enforcer: ## Build the privileged ActPlane enforcement daemon
	env -u DESTDIR -u MAKEFLAGS -u MFLAGS -u MAKEOVERRIDES \
		LIBBPF_SYS_LIBRARY_PATH="$(LIBBPF_SYS_LIBRARY_PATH)" \
		./scripts/build-enforcer.sh

.PHONY: build-frontend
build-frontend: ## Build and embed frontend into frontend-dist/
	cd dashboard && npm install \
		--registry="$(NPM_REGISTRY)" \
		--replace-registry-host="$(NPM_REPLACE_REGISTRY_HOST)" \
		&& npm run build:embed

.PHONY: build-all
build-all: build-frontend build build-enforcer ## Build frontend, AgentSight, and enforcer

.PHONY: build-mac
build-mac: build-frontend ## Build macOS local viewer (serve only; no eBPF)
	cargo build --release --bin agentsight

.PHONY: example
example: build ## Build C FFI example (requires libagentsight)
	gcc -o target/release/agentsight_example examples/agentsight_example.c \
		-Iinclude -Ltarget/release -lagentsight -lpthread -ldl -lm

# =============================================================================
# INSTALL
# =============================================================================

INSTALL_PROFILE ?= system
DESTDIR ?=
ifeq ($(INSTALL_PROFILE),user)
PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin
INSTALL_SYSTEMD ?= 0
else
PREFIX ?= /usr
BINDIR ?= /usr/local/bin
INSTALL_SYSTEMD ?= 1
endif
SYSCONFDIR ?= /etc
LIBDIR ?= $(PREFIX)/lib
SYSTEMD_SYSTEM_DIR ?= $(LIBDIR)/systemd/system
SERVICE_BINDIR ?= $(BINDIR)
SETCAP ?= auto

.PHONY: install uninstall
install: build-all ## Build and install agentsight binary and set BPF capabilities
	install -d -m 0755 $(DESTDIR)$(BINDIR)
	install -d -m 0755 $(DESTDIR)$(SYSCONFDIR)/agentsight
	install -p -m 0755 target/release/agentsight $(DESTDIR)$(BINDIR)/
	install -p -m 0755 target/release/agentsight-enforcer $(DESTDIR)$(BINDIR)/
	install -p -m 0644 agentsight.json $(DESTDIR)$(SYSCONFDIR)/agentsight/config.json
	@if [ "$(SETCAP)" = "1" ] || { [ "$(SETCAP)" = "auto" ] && [ "$(INSTALL_PROFILE)" = "system" ] && [ -z "$(DESTDIR)" ] && command -v setcap >/dev/null 2>&1; }; then \
		setcap cap_bpf,cap_perfmon=ep $(DESTDIR)$(BINDIR)/agentsight; \
	else \
		echo "Skipping setcap for $(DESTDIR)$(BINDIR)/agentsight"; \
	fi
	@if [ "$(INSTALL_SYSTEMD)" = "1" ]; then \
		install -d -m 0755 $(DESTDIR)$(SYSTEMD_SYSTEM_DIR); \
		install -p -m 0755 scripts/agentsight-start.sh $(DESTDIR)$(BINDIR)/agentsight-start; \
		sed 's|/usr/local/bin/agentsight-start|$(SERVICE_BINDIR)/agentsight-start|g' \
			scripts/agentsight.service > $(DESTDIR)$(SYSTEMD_SYSTEM_DIR)/agentsight.service; \
		sed 's|/usr/local/bin/agentsight-enforcer|$(SERVICE_BINDIR)/agentsight-enforcer|g' \
			scripts/agentsight-enforcer.service > $(DESTDIR)$(SYSTEMD_SYSTEM_DIR)/agentsight-enforcer.service; \
		chmod 0644 $(DESTDIR)$(SYSTEMD_SYSTEM_DIR)/agentsight.service; \
		chmod 0644 $(DESTDIR)$(SYSTEMD_SYSTEM_DIR)/agentsight-enforcer.service; \
	fi

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/agentsight
	rm -f $(DESTDIR)$(BINDIR)/agentsight-enforcer
	rm -f $(DESTDIR)$(BINDIR)/agentsight-start
	rm -f $(DESTDIR)$(SYSTEMD_SYSTEM_DIR)/agentsight.service
	rm -f $(DESTDIR)$(SYSTEMD_SYSTEM_DIR)/agentsight-enforcer.service
	rm -f $(DESTDIR)$(SYSCONFDIR)/agentsight/config.json

# =============================================================================
# TEST & QUALITY
# =============================================================================

.PHONY: package-raw stage-raw test-raw-package
package-raw: ## Package prebuilt AgentSight binaries for one raw target
	@./packaging/raw/package.sh package

stage-raw: ## Stage one raw payload into DESTDIR
	@./packaging/raw/package.sh stage

test-raw-package: ## Validate Linux and macOS raw package assembly
	@bash scripts/test-package-raw.sh

.PHONY: test
test: test-rpm-packaging test-raw-package ## Run unit tests (fast, no coverage)
	cd $(CURDIR) && cargo test

.PHONY: test-rpm-packaging
test-rpm-packaging: ## Validate the portable RPM payload contract
	cd $(CURDIR) && bash scripts/test-rpm-packaging.sh

.PHONY: lint
lint: ## Check formatting + architecture + clippy
	cd $(CURDIR) && cargo fmt --all --check
	cd $(CURDIR) && python3 scripts/check-arch-boundaries.py
	cd $(CURDIR) && python3 scripts/check-sqlite-entrypoints.py --self-test
	cd $(CURDIR) && python3 scripts/check-sqlite-entrypoints.py
	cd $(CURDIR) && cargo clippy --all-targets -- -D warnings

.PHONY: test-coverage
test-coverage: ## Run unit tests with coverage (same as CI)
	cd $(CURDIR) && cargo llvm-cov --cobertura --output-path coverage.xml \
		--ignore-filename-regex '(\.skel\.rs|target/debug/build|target/release/build|src/probes/)'

.PHONY: coverage
coverage: ## Generate HTML coverage report (opens browser)
	cd $(CURDIR) && cargo llvm-cov --html --output-dir coverage-html --open \
		--ignore-filename-regex '(\.skel\.rs|target/debug/build|target/release/build|src/probes/)'

# =============================================================================
# GIT HOOKS (opt-in, agent-agnostic)
# =============================================================================

.PHONY: install-hooks
install-hooks: ## Install opt-in pre-push hook mirroring CI gates (any human/agent)
	@existing=$$(git config --get core.hooksPath || true); \
	if [ -n "$$existing" ] && [ "$$existing" != "src/agentsight/scripts/hooks" ]; then \
		echo "core.hooksPath already set to '$$existing' (e.g. copilot-shell husky)."; \
		echo "It is single-valued; not overriding. Unset it first to use the agentsight hook:"; \
		echo "  git config --unset core.hooksPath"; \
		exit 1; \
	fi; \
	chmod +x scripts/hooks/pre-push; \
	git config core.hooksPath src/agentsight/scripts/hooks; \
	echo "Installed pre-push hook (core.hooksPath -> src/agentsight/scripts/hooks)."; \
	echo "Runs CI-mirror checks on pushes touching src/agentsight/. Uninstall: make uninstall-hooks"

.PHONY: uninstall-hooks
uninstall-hooks: ## Remove the agentsight pre-push hook (unset core.hooksPath)
	@current=$$(git config --get core.hooksPath || true); \
	if [ "$$current" = "src/agentsight/scripts/hooks" ]; then \
		git config --unset core.hooksPath; \
		echo "Removed pre-push hook (core.hooksPath unset)."; \
	else \
		echo "agentsight hook not active (core.hooksPath='$$current'); nothing to do."; \
	fi

.PHONY: help
help: ## Show this help message
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-20s\033[0m %s\n", $$1, $$2}'

.DEFAULT_GOAL := build-all

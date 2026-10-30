_AGENTSIGHT_MAKE_DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

include $(_AGENTSIGHT_MAKE_DIR)packaging/build.mk
include $(_AGENTSIGHT_MAKE_DIR)packaging/install.mk
include $(_AGENTSIGHT_MAKE_DIR)packaging/delivery.mk

.PHONY: package-raw stage-raw test-raw-package
package-raw: ## Package prebuilt AgentSight binaries for one raw target
	@./packaging/raw/package.sh package

stage-raw: ## Stage one raw payload into DESTDIR
	@./packaging/raw/package.sh stage

test-raw-package: ## Validate Linux and macOS raw package assembly
	@bash scripts/test-package-raw.sh

# =============================================================================
# TEST & QUALITY
# =============================================================================

.PHONY: test-batch-analysis
test-batch-analysis: ## Exercise batch discovery and execution without model calls
	python3 -m unittest discover -s scripts/benchmark/tests -p test_batch_execution_regressions.py

# Add the dependency separately from the shared native-test rule.
test: test-batch-analysis

.PHONY: test
test: test-rpm-packaging test-raw-package ## Run unit tests (fast, no coverage)
	cd $(CURDIR) && cargo test

.PHONY: bench
bench: ## Run stable userspace performance benchmarks
	cd $(CURDIR) && cargo bench --bench pipeline

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
	@grep -h -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-20s\033[0m %s\n", $$1, $$2}'

.DEFAULT_GOAL := build-all

.PHONY: test-tokenless-api
test-tokenless-api: ## Verify built Tokenless and AgentSight CLI processes on Linux
	python3 tests/tokenless-api-e2e.py

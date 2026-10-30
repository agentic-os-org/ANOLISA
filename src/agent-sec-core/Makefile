_SEC_CORE_MAKE_DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

# =============================================================================
# CODE QUALITY
# =============================================================================

.PHONY: python-code-pretty
python-code-pretty: ## Format Python code using black and isort
	@echo "🎨 Formatting code with black and isort..."
	@uv run --project agent-sec-cli isort --settings-path ../.. --profile black --line-length 80 --skip-glob "*/backend-skill/templates/*" .
	@uv run --project agent-sec-cli black --force-exclude "backend-skill/templates/" .

.PHONY: python-lint
python-lint: ## Run ruff lint check on all Python code (no fix)
	@echo "🔍 Running ruff lint check..."
	@uv run --project agent-sec-cli ruff check --config agent-sec-cli/pyproject.toml .

COMPARE_BRANCH ?= upstream/main

.PHONY: python-lint-ci
python-lint-ci: ## Run incremental ruff lint check (only changed lines vs main, non-blocking)
	@echo "🔍 Running incremental ruff lint check (diff-quality)..."
	@uv run --project agent-sec-cli ruff check --config agent-sec-cli/pyproject.toml --output-format=concise . > ruff_report.txt || true
	@sed -i '' 's|^\([^: ]*\.py\)|src/agent-sec-core/\1|' ruff_report.txt
	@cd "$(shell git rev-parse --show-toplevel)" && diff-quality --violations=flake8 --compare-branch=$(COMPARE_BRANCH) src/agent-sec-core/ruff_report.txt || true
	@rm -f ruff_report.txt

# -----------------------------------------------------------------------------
# Rust code quality. agent-sec-cli (prompt-scanner) is cross-platform; linux-sandbox is Linux-only.
#
# pyo3 0.20 rejects Python > 3.12, so point it at the uv project env instead of
# whatever PATH offers.  Never export an empty value -- that is fatal to pyo3.
# Recipes group `{ $(PYO3_PIN); cargo ...; }` behind `cd ... &&` so a failed cd
# aborts the whole line instead of running cargo from the wrong directory.
PYO3_PIN = PY="$$(uv python find 2>/dev/null)"; if [ -n "$$PY" ]; then export PYO3_PYTHON="$$PY"; fi

.PHONY: rust-code-pretty
rust-code-pretty: ## Format Rust code with cargo fmt (agent-sec-cli + V2 + linux-sandbox)
	@echo "🎨 Formatting Rust code with cargo fmt..."
	cd agent-sec-cli && cargo fmt --all
	cd v2 && cargo fmt --all
	cd linux-sandbox && cargo fmt

.PHONY: rust-fmt-check
rust-fmt-check: ## Check Rust formatting without modifying files
	@echo "🔍 Checking Rust formatting..."
	cd agent-sec-cli && cargo fmt --all -- --check
	cd v2 && cargo fmt --all -- --check
	cd linux-sandbox && cargo fmt -- --check

.PHONY: rust-lint
rust-lint: ## Run cargo clippy on Rust crates (linux-sandbox on Linux only)
	@echo "🔍 Running cargo clippy..."
	cd agent-sec-cli && { $(PYO3_PIN); cargo clippy --workspace --all-targets -- -D warnings; }
	cd v2 && cargo clippy --workspace --all-targets --locked -- -D warnings
	@if [ "$$(uname -s)" = "Linux" ]; then \
		cd linux-sandbox && cargo clippy --all-targets -- -D warnings; \
	else \
		echo "  (linux-sandbox clippy skipped: requires Linux)"; \
	fi

# =============================================================================
# TEST
# =============================================================================

.PHONY: test-python
test-python: ## Run Python unit and integration tests
	@echo "🧪 Running Python tests..."
	cd agent-sec-cli && uv sync
	uv run --project agent-sec-cli pytest tests/ --ignore=tests/e2e/ --ignore=tests/v2 -v
	uv run --project agent-sec-cli pytest tests/e2e/cli -v
	uv run --project agent-sec-cli pytest tests/e2e/daemon -v
	uv run --project agent-sec-cli pytest tests/e2e/prompt-scanner -v
	@echo "🧪 Running skill-ledger e2e tests..."
	uv run --project agent-sec-cli python3 tests/e2e/skill-ledger/e2e_test.py

.PHONY: test-prompt-scanner-e2e
test-prompt-scanner-e2e: ## Run prompt-scanner e2e tests (L2 cases self-skip unless a model is served)
	@# L2 (standard/strict) cases skip when no model backend is reachable. To run
	@# them, serve the model (e.g. `ollama serve` + `ollama pull <model>`) and set
	@# PROMPT_SCANNER_E2E_REQUIRE_L2=1 to turn the skip into a hard failure.
	@echo "🧪 Running prompt-scanner e2e tests..."
	uv run --project agent-sec-cli pytest tests/e2e/prompt-scanner

.PHONY: test-e2e-rpm
test-e2e-rpm: ## Run E2E tests against RPM-installed agent-sec-cli binary
	@echo "🧪 Running E2E tests on installed RPM..."
	@command -v agent-sec-cli >/dev/null 2>&1 || { echo "ERROR: agent-sec-cli not found on PATH"; exit 1; }
	@command -v pytest >/dev/null 2>&1 || pip3 install --quiet pytest
	@# The code-scanner rule inventory test parses rule YAML directly, so PyYAML
	@# is required even though the CLI itself is installed from the RPM.
	@python3 -c 'import yaml' >/dev/null 2>&1 || pip3 install --quiet pyyaml
	python3 -m pytest tests/e2e/ \
		--import-mode=importlib \
		--ignore=tests/e2e/linux-sandbox \
		-k 'not test_error_event_writes_to_sqlite' \
		-v --tb=short
	@# linux-sandbox e2e skipped: requires privileged container

# V1 e2e files are ignored one by one, each with the V2 capability it waits for.
# Delete a line as soon as its capability lands in the V2 CLI/daemon; do not
# collapse the list into a directory-level ignore, or migrated coverage will
# stay silently disabled.
PII_HOOK_LAYOUT = installed
SKILLSEC_HOOK_LAYOUT = installed

.PHONY: test-e2e-rpm-v2
test-e2e-rpm-v2: ## Run E2E tests against V2 RPM-installed binaries
	@echo "🧪 Running V2 E2E tests on installed RPM..."
	@command -v agent-sec-cli >/dev/null 2>&1 || { echo "ERROR: agent-sec-cli not found on PATH"; exit 1; }
	@command -v agent-sec-daemon >/dev/null 2>&1 || { echo "ERROR: agent-sec-daemon not found on PATH"; exit 1; }
	@command -v pytest >/dev/null 2>&1 || pip3 install --quiet pytest
	@# The shared code-scanner E2E imports rule metadata before it can decide
	@# whether source-only inventory assertions apply to this installed V2 RPM.
	@python3 -c 'import yaml' >/dev/null 2>&1 || pip3 install --quiet pyyaml
	CODE_SCANNER_E2E_SKIP_TELEMETRY=1 PII_E2E_RUNTIME=v2 OBS_E2E_RUNTIME=v2 SEC_EVENTS_E2E_RUNTIME=v2 PII_HOOK_LAYOUT=$(PII_HOOK_LAYOUT) SKILLSEC_HOOK_LAYOUT=$(SKILLSEC_HOOK_LAYOUT) python3 -m pytest tests/e2e/ tests/v2/ \
		--import-mode=importlib -ra \
		--ignore=tests/e2e/cli/test_observability_record_jsonl_e2e.py \
		--ignore=tests/e2e/cli/test_observability_record_sqlite_e2e.py \
		--ignore=tests/e2e/cli/test_skill_ledger_analyze_e2e.py \
		--ignore=tests/e2e/codex-hooks/test_codex_hooks_e2e.py \
		--ignore=tests/e2e/daemon/test_daemon_e2e.py \
		--ignore=tests/e2e/qoder-hooks/test_qoder_skill_ledger_hook_e2e.py \
		--ignore=tests/e2e/qwen-code-extension/test_qwen_direct_hook_execution.py \
		--ignore=tests/e2e/skill-ledger/e2e_test.py \
		--ignore=tests/e2e/skill-signing/e2e_test.py \
		--ignore=tests/e2e/linux-sandbox \
		-v --tb=short
	@# Pending V2 capabilities behind the ignores above:
	@#   codex|qwen hook tests                    -> mixed V1 prompt/observability fixtures
	@#   qoder skill hook tests                  -> V1 caller-local audit storage/schema
	@#   daemon/test_daemon_e2e.py                -> V1 daemon wire protocol
	@#   skill-ledger, skill-signing              -> retired V1 per-user keys and Agent hooks
	@# Rust core lifecycle: tests/v2/e2e/test_skillsec_cli_e2e.py.
	@# Observability ingestion and queries: tests/v2/e2e/test_observability_*_e2e.py.
	@# V1 event/report tests use isolated V2 daemons from tests/conftest.py.
	@# V1 record file-absence assertions differ from V2 daemon storage initialization;
	@# their V2 contracts are covered by test_observability_record_e2e.py.
	@# Installed SkillSec adapters: tests/v2/e2e/test_skillsec_hook_contracts.py.
	@#   linux-sandbox                            -> privileged container (same as V1)

.PHONY: test-e2e-source-build-v2
test-e2e-source-build-v2: ## Run V2 E2E tests against source-build-installed binaries
	@echo "🧪 Running V2 E2E tests on source build..."
	@command -v agent-sec-cli >/dev/null 2>&1 || { echo "ERROR: agent-sec-cli not found on PATH"; exit 1; }
	@command -v agent-sec-daemon >/dev/null 2>&1 || { echo "ERROR: agent-sec-daemon not found on PATH"; exit 1; }
	$(MAKE) test-e2e-rpm-v2

.PHONY: test-e2e-source-build
test-e2e-source-build: test-e2e-source-build-v2 ## Run V2 E2E tests against source-build-installed binaries

.PHONY: test-python-coverage
test-python-coverage: ## Run Python tests with coverage report
	@echo "🧪 Running Python tests with coverage..."
	cd agent-sec-cli && uv sync
	uv run --project agent-sec-cli pytest tests/ \
		--ignore=tests/e2e/linux-sandbox \
		--ignore=tests/e2e/skill-ledger \
		--ignore=tests/e2e/skill-signing \
		--ignore=tests/v2 \
		-v --cov --cov-config=agent-sec-cli/pyproject.toml --cov-report=xml --cov-report=term-missing

.PHONY: test-rust
test-rust: ## Run Rust tests (agent-sec-cli + V2; linux-sandbox on Linux only)
	@echo "🧪 Running agent-sec-cli workspace Rust tests..."
	cd agent-sec-cli && { $(PYO3_PIN); cargo test --workspace; }
	@$(MAKE) test-rust-v2
	@echo "🧪 Running linux-sandbox Rust tests (Linux only)..."
	@if [ "$$(uname -s)" = "Linux" ]; then \
		cd linux-sandbox && cargo test -- --skip bwrap_seccomp; \
	else \
		echo "  (linux-sandbox tests skipped: requires Linux)"; \
	fi

.PHONY: test-rust-v2
test-rust-v2: ## Run V2 Rust tests and verify daemon server identity separately
	@echo "🧪 Running AgentSecCore V2 workspace Rust tests..."
	cd v2 && AGENT_SEC_TEST_SKIP_SERVER_UID_CHECK=1 cargo test --workspace --locked --features asc-daemon-client/testing -- --skip untrusted_server_is_rejected_before_request_bytes_are_sent
	cd v2 && cargo test --locked -p asc-daemon-client --test transport untrusted_server_is_rejected_before_request_bytes_are_sent -- --exact

# One report per workspace: cargo-llvm-cov cannot merge across workspaces, and
# CI unions them via diff-cover.  `-p` skips the untested PyO3 cdylib root.
#
# Exclude `src/bin/`: build-time tools (sync_atr) that ship in no artifact, so
# their main()/git/IO glue would skew the gate. Their logic is unit-tested.
.PHONY: test-rust-coverage
test-rust-coverage: ## Run Rust tests with coverage (agent-sec-cli crates; linux-sandbox on Linux only)
	@echo "🧪 Running agent-sec-cli Rust tests with coverage..."
	cd agent-sec-cli && cargo llvm-cov --cobertura \
		--ignore-filename-regex '/src/bin/' \
		--output-path ../rust-cli-coverage.xml \
		-p prompt-scanner -p model-service
	@echo "🧪 Running linux-sandbox Rust tests with coverage (Linux only)..."
	@if [ "$$(uname -s)" = "Linux" ]; then \
		cd linux-sandbox && cargo llvm-cov --cobertura --output-path ../rust-coverage.xml -- --skip bwrap_seccomp; \
	else \
		echo "  (linux-sandbox coverage skipped: requires Linux)"; \
	fi

.PHONY: test-openclaw-plugin
test-openclaw-plugin: ## Run openclaw-plugin tests
	@echo "🧪 Running openclaw-plugin tests..."
	cd openclaw-plugin && npm run test && npm run smoke

.PHONY: test-openclaw-plugin-coverage
test-openclaw-plugin-coverage: ## Run openclaw-plugin tests with coverage report
	@echo "🧪 Running openclaw-plugin tests with coverage..."
	cd openclaw-plugin && npm run test:coverage && npm run smoke

.PHONY: test
test: test-python test-rust test-openclaw-plugin ## Run all tests

include $(_SEC_CORE_MAKE_DIR)packaging/build.mk
include $(_SEC_CORE_MAKE_DIR)packaging/raw.mk
include $(_SEC_CORE_MAKE_DIR)packaging/install.mk
include $(_SEC_CORE_MAKE_DIR)packaging/delivery.mk

.PHONY: help
help: ## Show this help message
	@grep -h -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-20s\033[0m %s\n", $$1, $$2}'

.DEFAULT_GOAL := help

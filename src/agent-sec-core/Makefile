# =============================================================================
# CODE QUALITY
# =============================================================================

.PHONY: python-code-pretty
python-code-pretty: ## Format Python code using black and isort
	@echo "🎨 Formatting code with black and isort..."
	@uv run --project agent-sec-cli isort --profile black --line-length 80 --skip-glob "*/backend-skill/templates/*" .
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
.PHONY: test-e2e-rpm-v2
test-e2e-rpm-v2: ## Run E2E tests against V2 RPM-installed binaries
	@echo "🧪 Running V2 E2E tests on installed RPM..."
	@command -v agent-sec-cli >/dev/null 2>&1 || { echo "ERROR: agent-sec-cli not found on PATH"; exit 1; }
	@command -v agent-sec-daemon >/dev/null 2>&1 || { echo "ERROR: agent-sec-daemon not found on PATH"; exit 1; }
	@command -v pytest >/dev/null 2>&1 || pip3 install --quiet pytest
	@# The shared code-scanner E2E imports rule metadata before it can decide
	@# whether source-only inventory assertions apply to this installed V2 RPM.
	@python3 -c 'import yaml' >/dev/null 2>&1 || pip3 install --quiet pyyaml
	CODE_SCANNER_E2E_SKIP_TELEMETRY=1 python3 -m pytest tests/e2e/ tests/v2/e2e/ \
		--import-mode=importlib \
		--ignore=tests/e2e/cli/test_capabilities_e2e.py \
		--ignore=tests/e2e/cli/test_events_e2e.py \
		--ignore=tests/e2e/cli/test_observability_record_jsonl_e2e.py \
		--ignore=tests/e2e/cli/test_observability_record_sqlite_e2e.py \
		--ignore=tests/e2e/cli/test_scan_pii_e2e.py \
		--ignore=tests/e2e/cli/test_session_report_e2e.py \
		--ignore=tests/e2e/cli/test_skill_ledger_analyze_e2e.py \
		--ignore=tests/e2e/codex-hooks/test_codex_hooks_e2e.py \
		--ignore=tests/e2e/daemon/test_daemon_e2e.py \
		--ignore=tests/e2e/prompt-scanner/test_prompt_scanner_e2e.py \
		--ignore=tests/e2e/qoder-hooks/test_qoder_skill_ledger_hook_e2e.py \
		--ignore=tests/e2e/qwen-code-extension/test_qwen_direct_hook_execution.py \
		--ignore=tests/e2e/skill-ledger/e2e_test.py \
		--ignore=tests/e2e/skill-signing/e2e_test.py \
		--ignore=tests/e2e/linux-sandbox \
		-v --tb=short
	@# Pending V2 capabilities behind the ignores above:
	@#   cli/test_capabilities_e2e.py             -> capability view command
	@#   cli/test_events_e2e.py                   -> security event query
	@#   cli/test_observability_record_*_e2e.py   -> observability record sinks
	@#   cli/test_scan_pii_e2e.py                 -> PII scanning
	@#   cli/test_session_report_e2e.py           -> session report
	@#   cli/test_skill_ledger_analyze_e2e.py     -> skill ledger analyze
	@#   codex|qoder|qwen hook tests              -> hooks calling the V2 CLI
	@#   daemon/test_daemon_e2e.py                -> V1 daemon wire protocol
	@#   prompt-scanner/test_prompt_scanner_e2e.py -> prompt scanning
	@#   skill-ledger, skill-signing              -> skill ledger and signing
	@#   linux-sandbox                            -> privileged container (same as V1)

VENV_PYTHON ?= $(HOME)/.local/lib/anolisa/sec-core/venv/bin/python

.PHONY: test-e2e-source-build
test-e2e-source-build: ## Run E2E tests against source-build-installed agent-sec-cli
	@echo "🧪 Running E2E tests on source build..."
	@command -v agent-sec-cli >/dev/null 2>&1 || { echo "ERROR: agent-sec-cli not found on PATH"; exit 1; }
	@$(VENV_PYTHON) -m pytest --version >/dev/null 2>&1 || uv pip install --python $(VENV_PYTHON) --quiet pytest
	$(VENV_PYTHON) -m pytest tests/e2e/ \
		--import-mode=importlib \
		--ignore=tests/e2e/linux-sandbox \
		-k 'not test_error_event_writes_to_sqlite' \
		-v --tb=short
	@# linux-sandbox e2e skipped: requires privileged container

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
	@echo "🧪 Running AgentSecCore V2 workspace Rust tests..."
	cd v2 && cargo test --workspace --locked
	@echo "🧪 Running linux-sandbox Rust tests (Linux only)..."
	@if [ "$$(uname -s)" = "Linux" ]; then \
		cd linux-sandbox && cargo test -- --skip bwrap_seccomp; \
	else \
		echo "  (linux-sandbox tests skipped: requires Linux)"; \
	fi

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

# =============================================================================
# BUILD
# =============================================================================

# Build output directory — all artifacts are collected here after build.
# Override from outside: make build-all BUILD_DIR=/path/to/output
BUILD_DIR ?= target
CLI_WHEEL_BUILD_DIR ?= agent-sec-cli/target/wheels
CLI_WHEEL_PATTERN := agent_sec_cli-*.whl

# The normal targets retain their network-enabled developer workflow. RPM builds
# select OFFLINE_BUILD through build-all-v2-offline after Source1 supplies every
# Cargo crate and npm package cache entry.
OFFLINE_BUILD ?= 0
CARGO_OFFLINE_FLAGS = $(if $(filter 1,$(OFFLINE_BUILD)),--offline,)
CARGO_BUILD_ENV = $(if $(filter 1,$(OFFLINE_BUILD)),CARGO_HOME="$(abspath $(BUILD_DIR)/cargo-home)",)
NPM_INSTALL = $(if $(filter 1,$(OFFLINE_BUILD)),npm ci --offline --include=dev --cache .npm-cache --no-audit --no-fund,npm install --include=dev)

# Component targets shared by the V1 and V2 aggregates. Only the CLI/daemon
# build differs between them, so one list prevents the two aggregates from
# drifting apart as components are added. $(1) is the CLI slot; its position is
# preserved so prerequisite order stays identical to the pre-V2 build-all.
define component_build_targets
build-sandbox $(1) build-openclaw-plugin build-hermes-plugin build-qwen-code-extension stage-codex-plugin stage-qoder-plugin stage-cosh-extension stage-skills stage-component-manifest
endef

# Install targets shared by the V1 and V2 rpmbuild aggregates; the CLI install
# target is prepended by each aggregate.
RPM_INSTALL_COMMON_TARGETS := install-cosh-hook install-codex-plugin install-qoder-plugin \
	install-openclaw-plugin install-hermes-plugin install-qwen-code-extension \
	install-skills install-component-manifest install-systemd-user

# V2 Rust binaries are collected here by build-cli-v2 and consumed by
# install-cli-v2, mirroring the build/install split used for linux-sandbox.
V2_BIN_BUILD_DIR ?= $(BUILD_DIR)/v2/bin

define require_single_cli_wheel
set -- "$(1)"/$(CLI_WHEEL_PATTERN); \
if [ "$$#" -ne 1 ] || [ ! -f "$$1" ]; then \
	echo "ERROR: expected exactly one agent-sec-cli wheel in $(1)" >&2; \
	exit 1; \
fi;
endef

.PHONY: build-sandbox
build-sandbox: ## Build linux-sandbox binary
	cd linux-sandbox && $(CARGO_BUILD_ENV) cargo build --release --locked $(CARGO_OFFLINE_FLAGS)
	install -d -m 0755 $(BUILD_DIR)
	cp -p linux-sandbox/target/release/linux-sandbox $(BUILD_DIR)/

.PHONY: build-cli
build-cli: ## Build agent-sec-cli wheel with maturin (Rust + Python)
	rm -f $(CLI_WHEEL_BUILD_DIR)/$(CLI_WHEEL_PATTERN)
	install -d -m 0755 $(BUILD_DIR)/wheels
	rm -f $(BUILD_DIR)/wheels/$(CLI_WHEEL_PATTERN)
	cd agent-sec-cli && uv sync --only-group dev --no-install-project && \
		uv run --no-sync maturin build --release -i python3.11 --manylinux off
	@$(call require_single_cli_wheel,$(CLI_WHEEL_BUILD_DIR)) \
		cp -p "$$1" $(BUILD_DIR)/wheels/

.PHONY: build-cli-v2
build-cli-v2: ## Build V2 Rust CLI and daemon binaries
	cd v2 && $(CARGO_BUILD_ENV) cargo build --release --locked $(CARGO_OFFLINE_FLAGS) --workspace
	install -d -m 0755 $(V2_BIN_BUILD_DIR)
	cp -p v2/target/release/agent-sec-cli v2/target/release/agent-sec-daemon \
		$(V2_BIN_BUILD_DIR)/

.PHONY: setup
setup: ## Install all dependencies (including dev), create .venv
	cd agent-sec-cli && uv sync

.PHONY: build-openclaw-plugin
build-openclaw-plugin: ## Build openclaw-plugin TypeScript sources
	cd openclaw-plugin && NODE_ENV=development $(NPM_INSTALL) && npm run build
	install -d -m 0755 $(BUILD_DIR)/openclaw-plugin/dist
	install -d -m 0755 $(BUILD_DIR)/openclaw-plugin/scripts
	cp openclaw-plugin/openclaw.plugin.json $(BUILD_DIR)/openclaw-plugin/
	cp openclaw-plugin/package.json $(BUILD_DIR)/openclaw-plugin/
	cp -r openclaw-plugin/dist/* $(BUILD_DIR)/openclaw-plugin/dist/
	cp -r openclaw-plugin/scripts/* $(BUILD_DIR)/openclaw-plugin/scripts/

.PHONY: build-hermes-plugin
build-hermes-plugin: ## Stage hermes-plugin Python sources to BUILD_DIR
	install -d -m 0755 $(BUILD_DIR)/hermes-plugin/src
	install -d -m 0755 $(BUILD_DIR)/hermes-plugin/scripts
	cp -rp hermes-plugin/src/. $(BUILD_DIR)/hermes-plugin/src/
	cp -rp hermes-plugin/scripts/. $(BUILD_DIR)/hermes-plugin/scripts/

.PHONY: build-qwen-code-extension
build-qwen-code-extension: ## Stage Qwen Code extension files (no compilation)
	install -d -m 0755 $(BUILD_DIR)/qwen-code-extension
	cp -rp qwen-code-extension/. $(BUILD_DIR)/qwen-code-extension/

.PHONY: stage-codex-plugin
stage-codex-plugin: ## Stage codex-plugin hooks and install script to BUILD_DIR
	install -d -m 0755 $(BUILD_DIR)/codex-plugin
	cp -rp codex-plugin/hooks-plugin $(BUILD_DIR)/codex-plugin/
	cp -p codex-plugin/install.sh $(BUILD_DIR)/codex-plugin/
	cp -rp codex-plugin/.agents $(BUILD_DIR)/codex-plugin/

.PHONY: stage-qoder-plugin
stage-qoder-plugin: ## Stage qoder-plugin hooks and install script to BUILD_DIR
	install -d -m 0755 $(BUILD_DIR)/qoder-plugin
	cp -rp qoder-plugin/. $(BUILD_DIR)/qoder-plugin/

.PHONY: stage-cosh-extension
stage-cosh-extension: ## Stage cosh-extension hooks to BUILD_DIR
	install -d -m 0755 $(BUILD_DIR)/cosh-extension
	cp -rp cosh-extension/. $(BUILD_DIR)/cosh-extension/

.PHONY: stage-skills
stage-skills: ## Stage skill files to BUILD_DIR
	install -d -m 0755 $(BUILD_DIR)/skills
	cp -rp skills/. $(BUILD_DIR)/skills/

.PHONY: stage-tools
stage-tools: ## Stage tools (sign-skill.sh) to BUILD_DIR
	install -d -m 0755 $(BUILD_DIR)/tools
	cp -p tools/sign-skill.sh $(BUILD_DIR)/tools/

.PHONY: stage-component-manifest
stage-component-manifest: ## Stage component.toml into BUILD_DIR
	install -d -m 0755 $(BUILD_DIR)/share/anolisa/components/sec-core
	install -p -m 0644 .anolisa/component.toml \
		$(BUILD_DIR)/share/anolisa/components/sec-core/component.toml

.PHONY: build-all
build-all: $(call component_build_targets,build-cli) ## Build all components
	@echo "📦 All artifacts collected to $(BUILD_DIR)/"

.PHONY: build-all-v2
build-all-v2: $(call component_build_targets,build-cli-v2) ## Build all components with the V2 Rust CLI/daemon
	@echo "📦 All artifacts collected to $(BUILD_DIR)/"

.PHONY: build-all-v2-offline
build-all-v2-offline: OFFLINE_BUILD := 1
build-all-v2-offline: $(call component_build_targets,build-cli-v2) ## Build V2 components from vendored dependencies only
	@echo "📦 Offline V2 artifacts collected to $(BUILD_DIR)/"

.PHONY: export-requirements
export-requirements: ## Re-export agent-sec-cli/requirements.txt from uv.lock
	cd agent-sec-cli && uv export --frozen --no-dev --no-emit-project -o requirements.txt

.PHONY: download-deps
download-deps: ## Download ALL Python deps for agent-sec-cli (requires network)
	install -d -m 0755 $(BUILD_DIR)/wheels
	pip3 download --dest $(BUILD_DIR)/wheels/ --no-cache-dir \
		--python-version 3.11.6 --only-binary=:all: \
		--timeout 60 \
		--require-hashes \
		--index-url https://pypi.org/simple/ \
		-r agent-sec-cli/requirements.txt

.PHONY: stage-cli
stage-cli: ## Install all wheels to local staging dir (requires uv)
	install -d -m 0755 $(CLI_STAGED_SITE)
	uv pip install --target $(CLI_STAGED_SITE) --no-deps --no-cache --link-mode copy \
		$(BUILD_DIR)/wheels/*.whl
	rm -f $(CLI_STAGED_SITE)/.lock

# =============================================================================
# RAW PACKAGE
# =============================================================================

OUTPUT_DIR ?= $(BUILD_DIR)/raw
TARGET_OS ?= linux
TARGET_ARCH ?= $(shell uname -m)
RAW_CONTRACT ?= .anolisa/component.toml
RAW_PYTHON_VERSION := 3.11.6
RAW_PYTHON_ENV ?= $(BUILD_DIR)/raw-python-env
RAW_PYTHON_RUNTIME ?= $(BUILD_DIR)/python-runtime
RAW_PYTHON_ARCHIVE ?= $(BUILD_DIR)/downloads/cpython-3.11.6+20231002-x86_64-unknown-linux-gnu-install_only.tar.gz

.PHONY: prepare-raw-python
prepare-raw-python: build-all ## Stage pinned Python and locked production dependencies for raw packaging
	rm -rf $(CLI_STAGED_SITE) $(RAW_PYTHON_ENV)
	./packaging/raw/prepare_python_runtime.sh \
		"$(abspath $(RAW_PYTHON_RUNTIME))" "$(abspath $(RAW_PYTHON_ARCHIVE))"
	cd agent-sec-cli && UV_PROJECT_ENVIRONMENT="$(abspath $(RAW_PYTHON_ENV))" \
		uv sync --python "$(abspath $(RAW_PYTHON_RUNTIME))/bin/python3.11" \
			--no-managed-python --frozen --no-dev --no-install-project --link-mode copy
	@set -eu; \
		python="$(abspath $(RAW_PYTHON_ENV))/bin/python3.11"; \
		version="$$("$$python" -c 'import platform; print(platform.python_version())')"; \
		[ "$$version" = "$(RAW_PYTHON_VERSION)" ] || { \
			echo "ERROR: raw package requires Python $(RAW_PYTHON_VERSION), found $$version" >&2; \
			exit 1; \
		}
	install -d -m 0755 $(CLI_STAGED_SITE)
	cp -a $(RAW_PYTHON_ENV)/lib/python3.11/site-packages/. $(CLI_STAGED_SITE)/
	@$(call require_single_cli_wheel,$(BUILD_DIR)/wheels) \
		uv pip install --python $(RAW_PYTHON_ENV)/bin/python3.11 \
			--target $(CLI_STAGED_SITE) --no-deps --no-cache --link-mode copy "$$1"
	rm -rf $(CLI_STAGED_SITE)/bin
	rm -f $(CLI_STAGED_SITE)/.lock
	rm -rf $(RAW_PYTHON_ENV)

.PHONY: stage-raw
stage-raw: ## Stage built raw payload under DESTDIR (build outputs must already exist)
	@BUILD_DIR="$(abspath $(BUILD_DIR))" \
		DESTDIR="$(DESTDIR)" \
		RAW_CONTRACT="$(abspath $(RAW_CONTRACT))" \
		TARGET_OS="$(TARGET_OS)" \
		TARGET_ARCH="$(TARGET_ARCH)" \
		./packaging/raw/package.sh stage

.PHONY: package-raw
package-raw: prepare-raw-python ## Build deterministic raw tar archive in OUTPUT_DIR
	@BUILD_DIR="$(abspath $(BUILD_DIR))" \
		OUTPUT_DIR="$(abspath $(OUTPUT_DIR))" \
		RAW_CONTRACT="$(abspath $(RAW_CONTRACT))" \
		TARGET_OS="$(TARGET_OS)" \
		TARGET_ARCH="$(TARGET_ARCH)" \
		SOURCE_DATE_EPOCH="$(SOURCE_DATE_EPOCH)" \
		./packaging/raw/package.sh package

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

.PHONY: install-sandbox
install-sandbox: ## Install linux-sandbox binary only
	install -d -m 0755 $(DESTDIR)$(BINDIR)
	install -p -m 0755 $(BUILD_DIR)/linux-sandbox $(DESTDIR)$(BINDIR)/

.PHONY: install-tool
install-tool: ## Install sign-skill.sh to LIBEXECDIR
	install -d -m 0755 $(DESTDIR)$(LIBEXECDIR)
	install -p -m 0755 $(BUILD_DIR)/tools/sign-skill.sh $(DESTDIR)$(LIBEXECDIR)/

.PHONY: install
install: install-all ## Install all components (alias for install-all)

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

.PHONY: install-cli-v2
install-cli-v2: ## RPM: Install V2 Rust CLI and daemon binaries
	install -d -m 0755 $(DESTDIR)/usr/bin
	install -p -m 0755 $(V2_BIN_BUILD_DIR)/agent-sec-cli \
		$(DESTDIR)/usr/bin/agent-sec-cli
	install -p -m 0755 $(V2_BIN_BUILD_DIR)/agent-sec-daemon \
		$(DESTDIR)/usr/bin/agent-sec-daemon

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

.PHONY: install-skills
install-skills: ## Install skill files to SKILLDIR
	install -d -m 0755 $(DESTDIR)$(SKILLDIR)
	cp -rp $(BUILD_DIR)/skills/. $(DESTDIR)$(SKILLDIR)/
	find $(DESTDIR)$(SKILLDIR) -type f -name '*.sh' -exec chmod 0755 {} +
	find $(DESTDIR)$(SKILLDIR) -type f -name '*.py' -exec chmod 0755 {} +

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
install-component-manifest: ## Install component.toml to DESTDIR
	install -d -m 0755 $(DESTDIR)$(COMPONENT_DIR)
	install -m 0644 .anolisa/component.toml \
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

.PHONY: install-all
install-all: install-cli-venv install-cosh-hook install-codex-plugin install-qoder-plugin install-openclaw-plugin install-hermes-plugin install-qwen-code-extension install-skills ## Install all (user source build)

.PHONY: install-all-for-rpmbuild
install-all-for-rpmbuild: OPENCLAW_PLUGIN_DIR := $(RPM_OPENCLAW_PLUGIN_DIR)
install-all-for-rpmbuild: HERMES_PLUGIN_DIR := $(RPM_HERMES_PLUGIN_DIR)
install-all-for-rpmbuild: QWEN_CODE_EXTENSION_DIR := $(RPM_QWEN_CODE_EXTENSION_DIR)
install-all-for-rpmbuild: CODEX_PLUGIN_DIR := $(RPM_CODEX_PLUGIN_DIR)
install-all-for-rpmbuild: QODER_PLUGIN_DIR := $(RPM_QODER_PLUGIN_DIR)
install-all-for-rpmbuild: install-cli-site $(RPM_INSTALL_COMMON_TARGETS) ## Install all (RPM build)

# V2 differs from install-all-for-rpmbuild only in the CLI/daemon implementation;
# both versions install the same systemd unit contract.
.PHONY: install-all-for-rpmbuild-v2
install-all-for-rpmbuild-v2: OPENCLAW_PLUGIN_DIR := $(RPM_OPENCLAW_PLUGIN_DIR)
install-all-for-rpmbuild-v2: HERMES_PLUGIN_DIR := $(RPM_HERMES_PLUGIN_DIR)
install-all-for-rpmbuild-v2: QWEN_CODE_EXTENSION_DIR := $(RPM_QWEN_CODE_EXTENSION_DIR)
install-all-for-rpmbuild-v2: CODEX_PLUGIN_DIR := $(RPM_CODEX_PLUGIN_DIR)
install-all-for-rpmbuild-v2: QODER_PLUGIN_DIR := $(RPM_QODER_PLUGIN_DIR)
install-all-for-rpmbuild-v2: install-cli-v2 $(RPM_INSTALL_COMMON_TARGETS) ## Install all (V2 RPM build)

# =============================================================================
# UNINSTALL
# =============================================================================
# Mirrors install-all (source build). RPM uninstall is handled by the package
# manager via the .spec %files manifests, not by `make uninstall`.

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
uninstall: uninstall-cli-venv uninstall-cosh-hook uninstall-codex-plugin uninstall-qoder-plugin uninstall-openclaw-plugin uninstall-hermes-plugin uninstall-qwen-code-extension uninstall-skills ## Uninstall everything install-all installed

.PHONY: help
help: ## Show this help message
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-20s\033[0m %s\n", $$1, $$2}'

.DEFAULT_GOAL := help
